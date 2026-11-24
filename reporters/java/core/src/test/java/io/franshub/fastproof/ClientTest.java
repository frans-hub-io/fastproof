package io.franshub.fastproof;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.PrintStream;
import java.net.ServerSocket;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.stream.Stream;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class ClientTest {
    static final String TOKEN = "test-token-abcdef";
    static final String SECRET = "correct-horse-battery";

    @TempDir
    Path dir;

    private Client client(final String url, final Map<String, String> extra) {
        final Map<String, String> env = new java.util.HashMap<>();
        env.put("FASTPROOF_URL", url);
        env.put("FASTPROOF_TOKEN", TOKEN);
        env.put("FASTPROOF_PROJECT", "demo");
        env.put("FASTPROOF_SPOOL_DIR", dir.resolve("spool").toString());
        env.put("FASTPROOF_FLUSH_TIMEOUT_MS", "4000");
        env.put("SHOP_PASSWORD", SECRET);
        env.putAll(extra);
        final Config cfg = Config.resolve(env, dir);
        cfg.batchIntervalMs = 20;
        return new Client(cfg, new PrintStream(new ByteArrayOutputStream()));
    }

    private static Map<String, Object> result(final String id, final String status) {
        final Map<String, Object> m = new LinkedHashMap<>();
        m.put("id", id);
        m.put("name", id);
        m.put("fullName", "suite > " + id);
        m.put("status", status);
        m.put("start", System.currentTimeMillis());
        m.put("stop", System.currentTimeMillis() + 1);
        m.put("retry", 0);
        return m;
    }

    @Test
    void runningThenFinalWithMaskedAttachmentAndDedup() throws Exception {
        try (FakeServer srv = new FakeServer(TOKEN, true, true)) {
            final Client c = client(srv.url(), Map.of("FASTPROOF_LAUNCH", "ci-1"));
            c.start();
            c.openRunning(result("r1", "passed"));
            c.log("r1", 0, "info", "step", "hello " + SECRET, "outer");
            final Path text = Files.writeString(dir.resolve("note.txt"), "pw=" + SECRET);
            final Path bin = Files.write(dir.resolve("a.bin"), new byte[2048]);
            c.finish(result("r1", "passed"), List.of(
                    new Client.FileRef("note.txt", "text/plain", text, null), new Client.FileRef("shot", "image/png", bin, null)), true);
            c.finish(result("r2", "failed"), List.of(new Client.FileRef("shot", "image/png", bin, null)), false);
            final Client.Summary s = c.close("finished");
            assertEquals(2, s.sent());
            assertEquals(0, s.spooled());
            assertEquals(List.of("running", "passed"), srv.history.get("r1"));
            assertEquals(2, srv.uploads);
            assertTrue(srv.attachmentBodies.values().stream().anyMatch(b -> new String(b, StandardCharsets.UTF_8).equals("pw=****")));
            assertFalse(Json.write(srv.logLines).contains(SECRET));
            assertEquals(1, srv.logLines.size());
            assertEquals(0, srv.finished.size());
        }
    }

    @Test
    void ownLaunchIsFinished() throws Exception {
        try (FakeServer srv = new FakeServer(TOKEN, true, true)) {
            final Client c = client(srv.url(), Map.of());
            c.start();
            c.finish(result("r1", "passed"), List.of(), true);
            c.close("finished");
            assertEquals(List.of("finished"), srv.finished);
        }
    }

    @Test
    void stageBServerTakesNoRunningLinesAndNoLogsAndCloseIsFast() throws Exception {
        try (FakeServer srv = new FakeServer(TOKEN, false, false)) {
            final Client c = client(srv.url(), Map.of("FASTPROOF_LAUNCH", "ci-2"));
            c.start();
            c.openRunning(result("r1", "passed"));
            c.log("r1", 0, "info", "step", "x", null);
            c.finish(result("r1", "passed"), List.of(), true);
            final long t0 = System.currentTimeMillis();
            final Client.Summary s = c.close("finished");
            assertTrue(System.currentTimeMillis() - t0 < 3000);
            assertEquals(1, s.sent());
            assertEquals(List.of("passed"), srv.history.get("r1"));
            assertTrue(srv.logLines.isEmpty());
        }
    }

    @Test
    void serverDownWritesTheSpool() throws Exception {
        final int port;
        try (ServerSocket ss = new ServerSocket(0)) {
            port = ss.getLocalPort();
        }
        final Client c = client("http://127.0.0.1:" + port, Map.of("FASTPROOF_FLUSH_TIMEOUT_MS", "1500", "FASTPROOF_LAUNCH", "ci-3"));
        c.start();
        final Path bin = Files.write(dir.resolve("a.bin"), new byte[64]);
        c.finish(result("r1", "failed"), List.of(new Client.FileRef("shot", "image/png", bin, null)), false);
        final Client.Summary s = c.close("finished");
        assertEquals(1, s.spooled());
        assertNotNull(s.spoolDir());
        final String line = Files.readString(s.spoolDir().resolve("results.ndjson")).strip();
        final Map<String, Object> o = Json.object(Json.parse(line));
        assertEquals(1L, o.get("v"));
        final Map<String, Object> blob = Json.object(Json.array(o.get("blobs")).get(0));
        assertTrue(Files.exists(s.spoolDir().resolve("blobs").resolve((String) blob.get("sha256"))));
        assertEquals("blob:" + blob.get("sha256"), Json.object(Json.array(Json.object(o.get("result")).get("attachments")).get(0)).get("id"));
        final Map<String, Object> meta = Json.object(Json.parse(Files.readString(s.spoolDir().resolve("meta.json"))));
        assertEquals("demo", meta.get("project"));
        assertFalse(Files.readString(s.spoolDir().resolve("meta.json")).contains(TOKEN));
        try (Stream<Path> files = Files.walk(dir.resolve("spool"))) {
            assertTrue(files.noneMatch(p -> p.toString().endsWith(".tmp")));
        }
    }

    @Test
    void wrongTokenGoesOffline() throws IOException {
        try (FakeServer srv = new FakeServer("other-token-123", true, true)) {
            final Client c = client(srv.url(), Map.of("FASTPROOF_LAUNCH", "ci-4"));
            c.start();
            c.finish(result("r1", "passed"), List.of(), true);
            final Client.Summary s = c.close("finished");
            assertTrue(s.offline());
            assertEquals(1, s.spooled());
            assertTrue(srv.results.isEmpty());
        }
    }

    @Test
    void drainWaitsForABatchThatIsAlreadyOnTheWire() throws Exception {
        try (FakeServer srv = new FakeServer(TOKEN, true, true)) {
            srv.resultsDelayMs = 600;
            final Client c = client(srv.url(), Map.of("FASTPROOF_LAUNCH", "ci-drain"));
            c.start();
            c.finish(result("r1", "failed"), List.of(), false);
            final long until = System.currentTimeMillis() + 5000;
            while (srv.resultRequests == 0 && System.currentTimeMillis() < until) {
                Thread.sleep(5);
            }
            assertEquals(1, srv.resultRequests);
            c.drain(5000);
            assertEquals(1, c.progress().sent());
            assertEquals(1, srv.results.size());
            c.close("finished");
        }
    }
}
