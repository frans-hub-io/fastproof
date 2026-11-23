package io.franshub.fastproof.allure;

import com.sun.net.httpserver.HttpServer;

import java.io.IOException;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.HexFormat;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/** A small in-memory FastProof API: launches, results (with running), logs and attachments. */
final class FakeServer implements AutoCloseable {
    private static final Pattern ROUTE = Pattern.compile("^/api/v1/projects/([^/]+)/launches(?:/([^/]+)/(results|logs|attachments|finish))?$");

    final boolean logs;
    final boolean running;
    final String token;
    final HttpServer server;
    final Map<String, Map<String, Object>> results = new LinkedHashMap<>();
    final Map<String, List<String>> history = new LinkedHashMap<>();
    final List<Map<String, Object>> logLines = new ArrayList<>();
    final Map<String, byte[]> attachmentBodies = new LinkedHashMap<>();
    final Map<String, String> attachmentNames = new LinkedHashMap<>();
    final Map<String, String> launches = new LinkedHashMap<>();
    final List<String> finished = new ArrayList<>();
    int uploads;

    FakeServer(final String token, final boolean logs, final boolean running) throws IOException {
        this.token = token;
        this.logs = logs;
        this.running = running;
        server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext("/", ex -> {
            final byte[] body = ex.getRequestBody().readAllBytes();
            final String auth = ex.getRequestHeaders().getFirst("Authorization");
            final Matcher m = ROUTE.matcher(ex.getRequestURI().getPath());
            int status;
            Object resp;
            synchronized (this) {
                if (token != null && !("Bearer " + token).equals(auth)) {
                    status = 401;
                    resp = Map.of("code", "unauthorized", "message", "no");
                } else if (!m.matches()) {
                    status = 404;
                    resp = Map.of("code", "not_found", "message", "no");
                } else {
                    final Object[] r = handle(m.group(2), m.group(3), body, ex.getRequestHeaders().getFirst("X-FastProof-Attachment-Name"));
                    status = (Integer) r[0];
                    resp = r[1];
                }
            }
            final byte[] out = Json.write(resp).getBytes(StandardCharsets.UTF_8);
            ex.getResponseHeaders().add("Content-Type", "application/json");
            ex.sendResponseHeaders(status, out.length);
            ex.getResponseBody().write(out);
            ex.close();
        });
        server.start();
    }

    String url() {
        return "http://127.0.0.1:" + server.getAddress().getPort();
    }

    private Object[] handle(final String launch, final String what, final byte[] body, final String attName) {
        if (what == null) {
            final Map<String, Object> start = Json.object(Json.parse(new String(body, StandardCharsets.UTF_8)));
            final String ext = (String) start.get("externalId");
            for (final Map.Entry<String, String> e : launches.entrySet()) {
                if (e.getValue().equals(ext)) {
                    return new Object[] {200, Map.of("id", e.getKey(), "number", 1)};
                }
            }
            final String id = UUID.randomUUID().toString();
            launches.put(id, ext);
            return new Object[] {201, Map.of("id", id, "number", launches.size())};
        }
        if (!launches.containsKey(launch)) {
            return new Object[] {404, Map.of("code", "not_found", "message", "launch")};
        }
        if (what.equals("finish")) {
            finished.add((String) Json.object(Json.parse(new String(body, StandardCharsets.UTF_8))).get("status"));
            return new Object[] {200, Map.of("id", launch)};
        }
        if (what.equals("attachments")) {
            uploads++;
            final String sha = sha(body);
            attachmentBodies.putIfAbsent(sha, body);
            attachmentNames.putIfAbsent(sha, attName);
            return new Object[] {201, Map.of("id", sha, "sha256", sha, "size", body.length)};
        }
        final List<Map<String, Object>> lines = new ArrayList<>();
        for (final String l : new String(body, StandardCharsets.UTF_8).split("\n")) {
            if (!l.isBlank()) {
                lines.add(Json.object(Json.parse(l)));
            }
        }
        if (what.equals("logs")) {
            if (!logs) {
                return new Object[] {405, Map.of("code", "method_not_allowed", "message", "no")};
            }
            logLines.addAll(lines);
            return new Object[] {202, Map.of("accepted", lines.size())};
        }
        final List<Object> rejected = new ArrayList<>();
        int accepted = 0;
        int dup = 0;
        for (int i = 0; i < lines.size(); i++) {
            final Map<String, Object> r = lines.get(i);
            final String id = (String) r.get("id");
            if ("running".equals(r.get("status")) && !running) {
                rejected.add(Map.of("index", i, "message", "invalid result: unknown variant `running`"));
                continue;
            }
            boolean missing = false;
            for (final Object a : Json.array(r.get("attachments"))) {
                if (!attachmentBodies.containsKey(Json.object(a).get("id"))) {
                    missing = true;
                }
            }
            if (missing) {
                rejected.add(Map.of("index", i, "message", "unknown attachment id"));
                continue;
            }
            final Map<String, Object> known = results.get(id);
            if (known != null && !"running".equals(known.get("status"))) {
                dup++;
                continue;
            }
            results.put(id, r);
            history.computeIfAbsent(id, k -> new ArrayList<>()).add((String) r.get("status"));
            accepted++;
        }
        return new Object[] {202, Map.of("accepted", accepted, "duplicates", dup, "rejected", rejected)};
    }

    private static String sha(final byte[] b) {
        try {
            return HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(b));
        } catch (java.security.NoSuchAlgorithmException e) {
            throw new IllegalStateException(e);
        }
    }

    @Override
    public void close() {
        server.stop(0);
    }
}
