package io.franshub.fastproof;

import java.io.IOException;
import java.net.InetAddress;
import java.net.http.HttpRequest;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.stream.Stream;

/** Sends spooled sessions (format v1 in reporters/PROTOCOL.md): launch by external id, blobs once per hash, results in batches. */
final class SpoolFlush {
    private static final int BATCH = 200;

    record Session(String id, Path dir, Map<String, Object> meta) {
    }

    record Result(int sessions, int results, int failed, int remaining, List<String> errors) {
    }

    private final Http http;
    private final Config cfg;

    SpoolFlush(final Http http, final Config cfg) {
        this.http = http;
        this.cfg = cfg;
    }

    /** Sessions that can be sent now: ended, or left by a process that is gone. */
    static List<Session> list(final Path root, final String exclude) {
        final List<Session> out = new ArrayList<>();
        if (!Files.isDirectory(root)) {
            return out;
        }
        final List<Path> dirs = new ArrayList<>();
        try (Stream<Path> s = Files.list(root)) {
            s.filter(Files::isDirectory).sorted(Comparator.comparing(p -> p.getFileName().toString())).forEach(dirs::add);
        } catch (IOException e) {
            return out;
        }
        final String host = hostName();
        for (final Path d : dirs) {
            final String name = d.getFileName().toString();
            if (name.equals(exclude) || name.startsWith(".")) {
                continue;
            }
            final Map<String, Object> meta;
            try {
                meta = Json.object(Json.parse(Files.readString(d.resolve("meta.json"), StandardCharsets.UTF_8)));
            } catch (IOException | RuntimeException e) {
                continue;
            }
            if (!(meta.get("v") instanceof Number v) || v.intValue() != 1) {
                continue;
            }
            final long pid = meta.get("pid") instanceof Number n ? n.longValue() : 0;
            final boolean running = !Boolean.TRUE.equals(meta.get("ended")) && host.equals(meta.get("host"))
                    && pid != ProcessHandle.current().pid() && alive(pid);
            if (!running) {
                out.add(new Session(name, d, meta));
            }
        }
        return out;
    }

    private static boolean alive(final long pid) {
        final Optional<ProcessHandle> h = ProcessHandle.of(pid);
        return h.isPresent() && h.get().isAlive();
    }

    static String hostName() {
        try {
            return InetAddress.getLocalHost().getHostName();
        } catch (IOException e) {
            return "unknown";
        }
    }

    Result run(final Path root, final String exclude, final long deadlineMs, final String project) {
        int sessions = 0;
        int results = 0;
        int failed = 0;
        int remaining = 0;
        final List<String> errors = new ArrayList<>();
        for (final Session s : list(root, exclude)) {
            final String sessionProject = String.valueOf(s.meta().get("project"));
            if (project != null && !project.isEmpty() && !project.equals(sessionProject)) {
                continue;
            }
            final List<String> lines = new ArrayList<>();
            try {
                for (final String l : Files.readString(s.dir().resolve("results.ndjson"), StandardCharsets.UTF_8).split("\n")) {
                    if (!l.isBlank()) {
                        lines.add(l);
                    }
                }
            } catch (IOException e) {
                // an empty session is only cleaned up
            }
            int done = 0;
            try {
                final Object p = Json.object(Json.parse(Files.readString(s.dir().resolve("progress.json"), StandardCharsets.UTF_8))).get("lines");
                done = p instanceof Number n ? n.intValue() : 0;
            } catch (IOException | RuntimeException e) {
                done = 0;
            }
            if (System.currentTimeMillis() > deadlineMs) {
                remaining += Math.max(0, lines.size() - done);
                continue;
            }
            final Http.Resp lr = http.post(sessionProject, "", "application/json",
                    HttpRequest.BodyPublishers.ofString(Json.write(s.meta().get("launch"))), Map.of(), 1);
            if (!lr.ok() || !(Json.object(lr.body()).get("id") instanceof String launchId)) {
                remaining += Math.max(0, lines.size() - done);
                errors.add("launch: HTTP " + lr.status() + " " + lr.error());
                if (lr.retryable()) {
                    break;
                }
                continue;
            }
            final Map<String, String> blobIds = new HashMap<>();
            boolean stopped = false;
            while (done < lines.size() && System.currentTimeMillis() <= deadlineMs && !stopped) {
                final List<String> chunk = lines.subList(done, Math.min(lines.size(), done + BATCH));
                final List<Map<String, Object>> batch = new ArrayList<>();
                for (final String ln : chunk) {
                    final Object parsed = Json.parse(ln);
                    if (!(parsed instanceof Map)) {
                        failed++;
                        continue;
                    }
                    final Map<String, Object> entry = Json.object(parsed);
                    for (final Object b : Json.array(entry.get("blobs"))) {
                        final Map<String, Object> blob = Json.object(b);
                        final String sha = String.valueOf(blob.get("sha256"));
                        final Path file = s.dir().resolve("blobs").resolve(sha.replaceAll("[^0-9a-f]", ""));
                        if (blobIds.containsKey(sha) || !Files.isRegularFile(file)) {
                            continue;
                        }
                        try {
                            final Http.Resp up = http.post(sessionProject, "/" + launchId + "/attachments",
                                    String.valueOf(blob.get("contentType")), HttpRequest.BodyPublishers.ofFile(file),
                                    Map.of("X-FastProof-Attachment-Name", Client.headerValue(String.valueOf(blob.get("name")))), 2);
                            if (up.ok() && Json.object(up.body()).get("id") instanceof String id) {
                                blobIds.put(sha, id);
                            } else if (up.retryable()) {
                                stopped = true;
                                errors.add("attachment: HTTP " + up.status() + " " + up.error());
                            }
                        } catch (IOException e) {
                            errors.add("attachment: " + e.getMessage());
                        }
                    }
                    if (stopped) {
                        break;
                    }
                    replaceBlobIds(Json.object(entry.get("result")), blobIds);
                    batch.add(Json.object(entry.get("result")));
                }
                if (stopped) {
                    break;
                }
                if (!batch.isEmpty()) {
                    final StringBuilder body = new StringBuilder();
                    for (final Map<String, Object> r : batch) {
                        body.append(Json.write(r)).append('\n');
                    }
                    final Http.Resp pr = http.post(sessionProject, "/" + launchId + "/results", "application/x-ndjson",
                            HttpRequest.BodyPublishers.ofString(body.toString()), Map.of(), 2);
                    if (!pr.ok()) {
                        errors.add("results: HTTP " + pr.status() + " " + pr.error());
                        stopped = true;
                        break;
                    }
                    final Map<String, Object> summary = Json.object(pr.body());
                    final int accepted = summary.get("accepted") instanceof Number n ? n.intValue() : 0;
                    final int dup = summary.get("duplicates") instanceof Number n ? n.intValue() : 0;
                    final List<Object> rejected = Json.array(summary.get("rejected"));
                    results += accepted + dup;
                    failed += rejected.size();
                    for (final Object r : rejected.subList(0, Math.min(3, rejected.size()))) {
                        errors.add(String.valueOf(Json.object(r).get("message")));
                    }
                }
                done += chunk.size();
                write(s.dir().resolve("progress.json"), Json.write(Map.of("lines", done)));
            }
            if (!stopped && done >= lines.size()) {
                if (Boolean.TRUE.equals(s.meta().get("ended")) && s.meta().get("finish") instanceof String finish) {
                    http.post(sessionProject, "/" + launchId + "/finish", "application/json",
                            HttpRequest.BodyPublishers.ofString(Json.write(Map.of("status", finish))), Map.of(), 2);
                }
                delete(s.dir());
                sessions++;
            } else {
                remaining += Math.max(0, lines.size() - done);
            }
        }
        return new Result(sessions, results, failed, remaining, errors);
    }

    @SuppressWarnings("unchecked")
    private static void replaceBlobIds(final Map<String, Object> node, final Map<String, String> ids) {
        final Object atts = node.get("attachments");
        if (atts instanceof List<?> list) {
            final List<Object> kept = new ArrayList<>();
            for (final Object a : list) {
                final Map<String, Object> m = Json.object(a);
                final String id = String.valueOf(m.get("id"));
                if (id.startsWith("blob:")) {
                    final String real = ids.get(id.substring(5));
                    if (real == null) {
                        continue;
                    }
                    ((Map<String, Object>) a).put("id", real);
                }
                kept.add(a);
            }
            node.put("attachments", kept);
        }
        for (final Object st : Json.array(node.get("steps"))) {
            replaceBlobIds(Json.object(st), ids);
        }
    }

    private static void write(final Path p, final String text) {
        try {
            final Path tmp = p.resolveSibling(p.getFileName() + ".tmp");
            Files.writeString(tmp, text, StandardCharsets.UTF_8);
            Files.move(tmp, p, StandardCopyOption.REPLACE_EXISTING);
        } catch (IOException e) {
            // progress is only an optimisation: results are idempotent by client id
        }
    }

    private static void delete(final Path dir) {
        try (Stream<Path> s = Files.walk(dir)) {
            s.sorted(Comparator.reverseOrder()).forEach(p -> p.toFile().delete());
        } catch (IOException e) {
            // left for the next run
        }
    }

    static Map<String, Object> summary(final Result r) {
        final Map<String, Object> m = new LinkedHashMap<>();
        m.put("sessions", r.sessions());
        m.put("results", r.results());
        m.put("failed", r.failed());
        m.put("remaining", r.remaining());
        return m;
    }
}
