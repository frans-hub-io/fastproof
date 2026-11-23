package io.franshub.fastproof.allure;

import java.io.IOException;
import java.io.InputStream;
import java.io.PrintStream;
import java.net.URI;
import java.net.URLEncoder;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.security.MessageDigest;
import java.security.SecureRandom;
import java.time.Duration;
import java.time.OffsetDateTime;
import java.time.ZoneOffset;
import java.time.format.DateTimeFormatter;
import java.util.ArrayList;
import java.util.HexFormat;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Sends results, logs and attachments to FastProof following reporters/PROTOCOL.md: one background sender,
 * ordered single in-flight results, retries with backoff, a circuit breaker and an offline spool.
 * Nothing here throws into the caller.
 */
final class Client {
    static final String VERSION = "0.1.0";
    private static final int RESULT_BATCH = 500;
    private static final int RESULT_BYTES = 4 * 1024 * 1024;
    private static final int LOG_BATCH = 1000;
    private static final int MAX_QUEUED_LOGS = 20_000;
    private static final int MAX_LOG_MESSAGE = 64 * 1024;
    private static final int BREAKER = 5;
    private static final int MASK_TEXT_BYTES = 4 * 1024 * 1024;
    private static final SecureRandom RANDOM = new SecureRandom();

    /** A file to upload for an attempt. */
    record FileRef(String name, String contentType, Path path) {
    }

    record Summary(String launchId, Integer launchNumber, int sent, int spooled, int lost, int attachments, boolean offline,
            String offlineReason, Path spoolDir) {
    }

    private record Item(Map<String, Object> result, boolean running, int bytes) {
    }

    private record Resp(int status, Object body, long retryAfterMs, String error) {
        boolean ok() {
            return status >= 200 && status < 300;
        }

        boolean retryable() {
            return status == 0 || status == 408 || status == 429 || status >= 500;
        }
    }

    final Config cfg;
    final Masker masker;
    private final PrintStream out;
    private final HttpClient http;
    private final Object lock = new Object();
    private final Object sendLock = new Object();
    private final List<Item> queue = new ArrayList<>();
    private final List<Map<String, Object>> logs = new ArrayList<>();
    private final Map<String, Integer> seq = new ConcurrentHashMap<>();
    private final Map<String, String> uploads = new ConcurrentHashMap<>();
    private final Map<String, Object> uploadLocks = new ConcurrentHashMap<>();
    private final Set<String> warned = ConcurrentHashMap.newKeySet();
    private final Set<String> inflight = ConcurrentHashMap.newKeySet();
    private final ExecutorService pipeline = Executors.newFixedThreadPool(4, r -> daemon(r, "fastproof-upload"));
    private final AtomicInteger sent = new AtomicInteger();
    private final AtomicInteger lost = new AtomicInteger();
    private final AtomicInteger attachments = new AtomicInteger();
    private final Spool spool;
    private volatile String launchId;
    private volatile Integer launchNumber;
    private volatile boolean offline;
    private volatile String offlineReason = "";
    private volatile boolean running;
    private volatile String logsState;
    private volatile int failures;
    private volatile boolean stop;
    private Thread sender;

    Client(final Config cfg, final PrintStream out) {
        this.cfg = cfg;
        this.out = out;
        this.masker = new Masker(cfg.secretEnv, cfg.secretValues, cfg.secretPatterns, cfg.env);
        this.http = HttpClient.newBuilder().followRedirects(HttpClient.Redirect.NEVER).connectTimeout(Duration.ofSeconds(10)).build();
        this.offline = cfg.offline;
        this.offlineReason = cfg.offline ? "offline mode" : "";
        this.running = cfg.runningResults;
        this.logsState = cfg.liveLogs ? "unknown" : "off";
        this.spool = new Spool(cfg, VERSION);
    }

    private static Thread daemon(final Runnable r, final String name) {
        final Thread t = new Thread(r, name);
        t.setDaemon(true);
        return t;
    }

    void info(final String msg) {
        if (!cfg.quiet) {
            out.println("[fastproof] " + msg);
        }
    }

    void warnOnce(final String key, final String msg) {
        if (warned.add(key)) {
            out.println("[fastproof] " + masker.mask(msg));
        }
    }

    void start() {
        if (offline) {
            return;
        }
        sender = daemon(this::loop, "fastproof-sender");
        sender.start();
    }

    private void loop() {
        while (!stop) {
            try {
                Thread.sleep(cfg.batchIntervalMs);
                pump();
            } catch (InterruptedException e) {
                return;
            } catch (RuntimeException e) {
                warnOnce("sender", "reporting error (ignored): " + e);
            }
        }
    }

    // ---- HTTP -------------------------------------------------------------------------------------------

    private String base(final String suffix) {
        return cfg.url.replaceAll("/+$", "") + "/api/v1/projects/" + cfg.project + "/launches" + suffix;
    }

    private Resp call(final String suffix, final String contentType, final HttpRequest.BodyPublisher body, final Map<String, String> headers, final int retries) {
        Resp last = new Resp(0, null, 0, "no request");
        for (int attempt = 0; attempt <= retries; attempt++) {
            last = once(suffix, contentType, body, headers);
            if (!last.retryable() || attempt == retries) {
                return last;
            }
            try {
                Thread.sleep(last.retryAfterMs() > 0 ? last.retryAfterMs() : backoff(attempt));
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                return last;
            }
        }
        return last;
    }

    static long backoff(final int attempt) {
        final long exp = Math.min(8000, 250L << Math.min(attempt, 16));
        return exp / 2 + (long) (RANDOM.nextDouble() * (exp / 2));
    }

    private Resp once(final String suffix, final String contentType, final HttpRequest.BodyPublisher body, final Map<String, String> headers) {
        try {
            final HttpRequest.Builder b = HttpRequest.newBuilder(URI.create(base(suffix)))
                    .timeout(Duration.ofMillis(cfg.requestTimeoutMs))
                    .header("accept", "application/json")
                    .header("user-agent", "fastproof-allure-java/" + VERSION)
                    .header("content-type", contentType)
                    .POST(body);
            if (!cfg.token.isEmpty()) {
                b.header("authorization", "Bearer " + cfg.token);
            }
            headers.forEach(b::header);
            final HttpResponse<String> r = http.send(b.build(), HttpResponse.BodyHandlers.ofString());
            long ra = 0;
            final String h = r.headers().firstValue("retry-after").orElse(null);
            if (h != null) {
                try {
                    ra = Math.min(Math.max(Long.parseLong(h.strip()), 0), 60) * 1000;
                } catch (NumberFormatException e) {
                    ra = 0;
                }
            }
            return new Resp(r.statusCode(), r.body().isEmpty() ? null : Json.parse(r.body()), ra, "");
        } catch (IOException e) {
            return new Resp(0, null, 0, String.valueOf(e));
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return new Resp(0, null, 0, "interrupted");
        } catch (RuntimeException e) {
            return new Resp(-1, null, 0, String.valueOf(e));
        }
    }

    private static HttpRequest.BodyPublisher ndjson(final List<Map<String, Object>> lines) {
        final StringBuilder sb = new StringBuilder();
        for (final Map<String, Object> l : lines) {
            sb.append(Json.write(l)).append('\n');
        }
        return HttpRequest.BodyPublishers.ofString(sb.toString());
    }

    private void failed(final Resp r) {
        if (r.status > 0 && !r.retryable()) {
            if (r.status == 401 || r.status == 403 || r.status == 404) {
                goOffline("HTTP " + r.status);
            } else {
                warnOnce("fatal:" + r.status, "the server refused a request: HTTP " + r.status);
            }
            return;
        }
        if (++failures >= BREAKER) {
            goOffline(r.error.isEmpty() ? "HTTP " + r.status : r.error);
        }
    }

    private void goOffline(final String reason) {
        if (offline) {
            return;
        }
        offline = true;
        offlineReason = reason;
        warnOnce("offline", "reporting continues offline (" + reason + "); results go to " + cfg.spoolDir);
        final List<Item> q;
        synchronized (lock) {
            q = new ArrayList<>(queue);
            queue.clear();
            logs.clear();
        }
        for (final Item i : q) {
            if (!i.running()) {
                spoolResult(i.result(), List.of(), true);
            }
        }
    }

    private void ensureLaunch(final int retries) {
        if (launchId != null || offline) {
            return;
        }
        final Resp r = call("", "application/json", HttpRequest.BodyPublishers.ofString(Json.write(cfg.launch)), Map.of(), retries);
        if (r.ok()) {
            final Map<String, Object> m = Json.object(r.body());
            if (m.get("id") instanceof String id) {
                launchId = id;
                launchNumber = m.get("number") instanceof Number n ? n.intValue() : null;
                failures = 0;
            }
        } else {
            failed(r);
        }
    }

    // ---- queueing ---------------------------------------------------------------------------------------

    void openRunning(final Map<String, Object> result) {
        if (!running || offline) {
            return;
        }
        final Map<String, Object> r = new LinkedHashMap<>(result);
        r.put("status", "running");
        enqueue(r, true);
    }

    private void enqueue(final Map<String, Object> result, final boolean isRunning) {
        final int bytes = Json.write(result).length();
        synchronized (lock) {
            queue.add(new Item(result, isRunning, bytes));
        }
    }

    void log(final String resultId, final int attempt, final String level, final String stream, final String message, final String step) {
        if ("off".equals(logsState) || offline) {
            return;
        }
        final String key = resultId + "\0" + attempt;
        final Map<String, Object> line = new LinkedHashMap<>();
        line.put("resultId", resultId);
        line.put("attempt", attempt);
        line.put("seq", seq.merge(key, 1, Integer::sum) - 1);
        line.put("ts", System.currentTimeMillis());
        line.put("level", level);
        line.put("stream", stream);
        final String m = masker.mask(message);
        line.put("message", m.length() > MAX_LOG_MESSAGE ? m.substring(0, MAX_LOG_MESSAGE) : m);
        if (step != null) {
            line.put("step", masker.mask(step));
        }
        synchronized (lock) {
            logs.add(line);
            if (logs.size() > MAX_QUEUED_LOGS) {
                logs.subList(0, logs.size() - MAX_QUEUED_LOGS).clear();
            }
        }
    }

    /** Uploads the attempt's files in the background, then queues the final result. */
    void finish(final Map<String, Object> result, final List<FileRef> files, final boolean passed) {
        final String rid = (String) result.get("id");
        inflight.add(rid);
        try {
            pipeline.submit(() -> {
                try {
                    runPipeline(result, files, passed);
                } catch (RuntimeException e) {
                    warnOnce("pipeline", "reporting error (ignored): " + e);
                } finally {
                    inflight.remove(rid);
                }
            });
        } catch (RuntimeException e) {
            inflight.remove(rid);
        }
    }

    private record Prepared(String name, String contentType, Path path, byte[] body, String sha256, long size) {
    }

    private Prepared prepare(final FileRef f) {
        try {
            long size = Files.size(f.path());
            if (size > cfg.maxAttachmentBytes) {
                warnOnce("big:" + f.name(), "attachment " + f.name() + " is too large; skipped");
                return null;
            }
            byte[] body = null;
            Path path = f.path();
            if (masker.active() && isText(f.contentType()) && size <= MASK_TEXT_BYTES) {
                final String text = new String(Files.readAllBytes(path), StandardCharsets.UTF_8);
                final String masked = masker.mask(text);
                if (!masked.equals(text)) {
                    body = masked.getBytes(StandardCharsets.UTF_8);
                    path = null;
                    size = body.length;
                }
            }
            return new Prepared(f.name(), f.contentType(), path, body, sha256(body, path), size);
        } catch (IOException | RuntimeException e) {
            warnOnce("read:" + f.name(), "cannot read attachment " + f.name() + ": " + e.getMessage());
            return null;
        }
    }

    static boolean isText(final String ct) {
        final String base = ct == null ? "" : ct.split(";")[0].strip().toLowerCase();
        return base.startsWith("text/") || base.equals("application/json") || base.equals("application/x-ndjson") || base.endsWith("+json");
    }

    private static String sha256(final byte[] body, final Path path) throws IOException {
        try {
            final MessageDigest md = MessageDigest.getInstance("SHA-256");
            if (body != null) {
                md.update(body);
            } else {
                try (InputStream in = Files.newInputStream(path)) {
                    final byte[] buf = new byte[1 << 16];
                    for (int n = in.read(buf); n >= 0; n = in.read(buf)) {
                        md.update(buf, 0, n);
                    }
                }
            }
            return HexFormat.of().formatHex(md.digest());
        } catch (java.security.NoSuchAlgorithmException e) {
            throw new IllegalStateException(e);
        }
    }

    private void runPipeline(final Map<String, Object> result, final List<FileRef> files, final boolean passed) {
        final List<Prepared> prepared = new ArrayList<>();
        for (final FileRef f : files) {
            final Prepared p = prepare(f);
            if (p != null) {
                prepared.add(p);
            }
        }
        if (launchId == null && !offline) {
            ensureLaunch(1);
        }
        final List<Map<String, Object>> refs = new ArrayList<>();
        final List<Prepared> pending = new ArrayList<>();
        for (final Prepared p : prepared) {
            final String id = upload(p);
            if (id.equals("retry")) {
                pending.add(p);
            } else if (!id.equals("skip")) {
                refs.add(ref(id, p));
            }
        }
        result.put("attachments", refs);
        if (offline || !pending.isEmpty()) {
            spoolResult(result, pending, passed);
            return;
        }
        enqueue(result, false);
    }

    private static Map<String, Object> ref(final String id, final Prepared p) {
        final Map<String, Object> r = new LinkedHashMap<>();
        r.put("id", id);
        r.put("name", p.name());
        r.put("contentType", p.contentType());
        return r;
    }

    private String upload(final Prepared p) {
        final Object l = uploadLocks.computeIfAbsent(p.sha256(), k -> new Object());
        synchronized (l) {
            final String known = uploads.get(p.sha256());
            if (known != null) {
                return known;
            }
            if (offline || launchId == null) {
                return "retry";
            }
            final HttpRequest.BodyPublisher body;
            try {
                body = p.body() != null ? HttpRequest.BodyPublishers.ofByteArray(p.body()) : HttpRequest.BodyPublishers.ofFile(p.path());
            } catch (IOException e) {
                warnOnce("read:" + p.name(), "cannot read attachment " + p.name());
                return "skip";
            }
            final Resp r = call("/" + launchId + "/attachments", p.contentType(), body,
                    Map.of("X-FastProof-Attachment-Name", headerValue(p.name())), 2);
            if (r.ok() && Json.object(r.body()).get("id") instanceof String id) {
                attachments.incrementAndGet();
                uploads.put(p.sha256(), id);
                failures = 0;
                return id;
            }
            if (r.status == 400 || r.status == 413 || r.status == 415 || r.status == -1) {
                warnOnce("att:" + r.status, "attachment " + p.name() + " rejected: HTTP " + r.status);
                return "skip";
            }
            failed(r);
            return "retry";
        }
    }

    static String headerValue(final String name) {
        for (int i = 0; i < name.length(); i++) {
            final char c = name.charAt(i);
            if (c < 0x20 || c > 0x7e) {
                return URLEncoder.encode(name, StandardCharsets.UTF_8).replace("+", "%20");
            }
        }
        return name;
    }

    private void spoolResult(final Map<String, Object> result, final List<Prepared> blobs, final boolean passed) {
        final List<Map<String, Object>> kept = new ArrayList<>();
        final List<Object> refs = new ArrayList<>(Json.array(result.get("attachments")));
        for (final Prepared b : blobs) {
            if (spool.addBlob(b.sha256(), b.size(), passed, b.path(), b.body())) {
                final Map<String, Object> m = new LinkedHashMap<>();
                m.put("sha256", b.sha256());
                m.put("name", b.name());
                m.put("contentType", b.contentType());
                m.put("size", b.size());
                kept.add(m);
                refs.add(ref("blob:" + b.sha256(), b));
            }
        }
        final Map<String, Object> r = new LinkedHashMap<>(result);
        r.put("attachments", refs);
        if (!spool.append(r, kept)) {
            lost.incrementAndGet();
        }
    }

    // ---- sending ----------------------------------------------------------------------------------------

    void pump() {
        synchronized (sendLock) {
            if (offline) {
                return;
            }
            if (launchId == null) {
                ensureLaunch(0);
            }
            if (launchId == null) {
                return;
            }
            probeLogs();
            sendResults();
            sendLogs();
        }
    }

    private void probeLogs() {
        if (!"unknown".equals(logsState)) {
            return;
        }
        final Resp r = call("/" + launchId + "/logs", "application/x-ndjson", HttpRequest.BodyPublishers.ofString(""), Map.of(), 0);
        if (r.status == 404 || r.status == 405 || r.status == 501) {
            logsState = "off";
            synchronized (lock) {
                logs.clear();
            }
        } else if (r.ok()) {
            logsState = "on";
        }
    }

    private List<Item> take() {
        synchronized (lock) {
            final List<Item> batch = new ArrayList<>();
            int size = 0;
            while (!queue.isEmpty() && batch.size() < RESULT_BATCH) {
                final Item next = queue.get(0);
                if (!batch.isEmpty() && size + next.bytes() > RESULT_BYTES) {
                    break;
                }
                batch.add(queue.remove(0));
                size += next.bytes();
            }
            return batch;
        }
    }

    private void requeue(final List<Item> items) {
        synchronized (lock) {
            queue.addAll(0, items);
        }
    }

    private void sendResults() {
        while (!offline) {
            final List<Item> batch = take();
            if (batch.isEmpty()) {
                return;
            }
            final List<Map<String, Object>> lines = new ArrayList<>();
            for (final Item i : batch) {
                lines.add(i.result());
            }
            final Resp r = call("/" + launchId + "/results", "application/x-ndjson", ndjson(lines), Map.of(), 0);
            if (!r.ok()) {
                if (r.status == 413 && batch.size() > 1) {
                    final List<Item> big = new ArrayList<>();
                    for (final Item i : batch) {
                        big.add(new Item(i.result(), i.running(), RESULT_BYTES));
                    }
                    requeue(big);
                    continue;
                }
                if (r.status == 413) {
                    lost.addAndGet((int) batch.stream().filter(i -> !i.running()).count());
                    warnOnce("413", "a result is too large for the server");
                    continue;
                }
                final List<Item> back = new ArrayList<>();
                for (final Item i : batch) {
                    if (!i.running()) {
                        back.add(i);
                    }
                }
                requeue(back);
                failed(r);
                return;
            }
            failures = 0;
            final Set<Integer> rejected = new java.util.HashSet<>();
            for (final Object o : Json.array(Json.object(r.body()).get("rejected"))) {
                final Map<String, Object> rej = Json.object(o);
                final int idx = rej.get("index") instanceof Number n ? n.intValue() : -1;
                rejected.add(idx);
                if (idx >= 0 && idx < batch.size() && batch.get(idx).running()) {
                    running = false;
                } else {
                    lost.incrementAndGet();
                    warnOnce("rej:" + rej.get("message"), "the server rejected a result: " + rej.get("message"));
                }
            }
            for (int i = 0; i < batch.size(); i++) {
                if (!batch.get(i).running() && !rejected.contains(i)) {
                    sent.incrementAndGet();
                }
            }
        }
    }

    private void sendLogs() {
        while (!offline && !"off".equals(logsState)) {
            final List<Map<String, Object>> batch;
            synchronized (lock) {
                final int n = Math.min(LOG_BATCH, logs.size());
                batch = new ArrayList<>(logs.subList(0, n));
                logs.subList(0, n).clear();
            }
            if (batch.isEmpty()) {
                return;
            }
            final Resp r = call("/" + launchId + "/logs", "application/x-ndjson", ndjson(batch), Map.of(), 0);
            if (r.ok()) {
                logsState = "on";
                failures = 0;
            } else if (r.status == 404 || r.status == 405 || r.status == 501 || (r.status > 0 && !r.retryable())) {
                logsState = "off";
                synchronized (lock) {
                    logs.clear();
                }
                return;
            } else {
                synchronized (lock) {
                    logs.addAll(0, batch);
                }
                failed(r);
                return;
            }
        }
    }

    // ---- closing ----------------------------------------------------------------------------------------

    Summary close(final String status) {
        final long deadline = System.currentTimeMillis() + cfg.flushTimeoutMs;
        try {
            stop = true;
            if (sender != null) {
                sender.interrupt();
            }
            pipeline.shutdown();
            while (System.currentTimeMillis() < deadline && !inflight.isEmpty()) {
                Thread.sleep(20);
            }
            while (!offline && System.currentTimeMillis() < deadline) {
                final int before;
                synchronized (lock) {
                    before = queue.size() + logs.size();
                }
                if (before == 0) {
                    break;
                }
                pump();
                final int after;
                synchronized (lock) {
                    after = queue.size() + logs.size();
                }
                if (after >= before) {
                    Thread.sleep(Math.min(200, Math.max(1, deadline - System.currentTimeMillis())));
                }
            }
            final boolean leftover;
            synchronized (lock) {
                leftover = !inflight.isEmpty() || queue.stream().anyMatch(i -> !i.running());
            }
            if (leftover) {
                goOffline(offlineReason.isEmpty() ? "the flush timeout was reached" : offlineReason);
            }
            final boolean wantFinish = cfg.finishLaunch.equals("always") || (cfg.finishLaunch.equals("auto") && cfg.ownLaunch);
            if (spool.opened()) {
                spool.end(wantFinish ? status : null);
            } else if (wantFinish && launchId != null && System.currentTimeMillis() < deadline) {
                final Map<String, Object> body = new LinkedHashMap<>();
                body.put("status", status);
                call("/" + launchId + "/finish", "application/json", HttpRequest.BodyPublishers.ofString(Json.write(body)), Map.of(), 1);
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        } catch (RuntimeException e) {
            warnOnce("close", "reporting error (ignored): " + e);
        }
        final boolean spooled = spool.opened();
        return new Summary(launchId, launchNumber, sent.get(), spooled ? spool.written() : 0, lost.get() + spool.dropped(),
                attachments.get(), offline, offlineReason, spooled ? spool.dir() : null);
    }

    /** UTC timestamp used in session names. */
    static String stamp() {
        return OffsetDateTime.now(ZoneOffset.UTC).format(DateTimeFormatter.ofPattern("yyyyMMdd'T'HHmmss"));
    }

    static void append(final Path p, final String line) throws IOException {
        Files.writeString(p, line, StandardCharsets.UTF_8, StandardOpenOption.CREATE, StandardOpenOption.APPEND);
    }
}
