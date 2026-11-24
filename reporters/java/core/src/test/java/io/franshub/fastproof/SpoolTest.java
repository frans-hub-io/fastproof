package io.franshub.fastproof;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.ByteArrayOutputStream;
import java.io.PrintStream;
import java.net.ServerSocket;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.stream.Stream;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class SpoolTest {
    static final String TOKEN = "test-token-abcdef";

    @TempDir
    Path dir;

    private Map<String, String> env(final String url, final String launch) {
        final Map<String, String> env = new HashMap<>();
        env.put("FASTPROOF_URL", url);
        env.put("FASTPROOF_TOKEN", TOKEN);
        env.put("FASTPROOF_PROJECT", "demo");
        env.put("FASTPROOF_LAUNCH", launch);
        env.put("FASTPROOF_SPOOL_DIR", dir.resolve("spool").toString());
        env.put("FASTPROOF_FLUSH_TIMEOUT_MS", "1500");
        return env;
    }

    private static int freePort() throws Exception {
        try (ServerSocket ss = new ServerSocket(0)) {
            return ss.getLocalPort();
        }
    }

    @Test
    void serverDownSpoolsThenTheNextRunAndTheFlushCommandSendIt() throws Exception {
        final int port = freePort();
        final Reporter down = Reporter.start(env("http://127.0.0.1:" + port, "ci-spool"), dir, new PrintStream(new ByteArrayOutputStream()), false);
        final Path shot = Files.write(dir.resolve("shot.png"), new byte[64]);
        final Attempt a = down.begin(new TestDescriptor("tn", "k1", "t1").fullName("p.C#t1"), 0);
        a.attachFile("shot.png", "image/png", shot);
        a.finish("failed", new AssertionError("boom"));
        down.close();
        try (Stream<Path> s = Files.walk(dir.resolve("spool"))) {
            assertTrue(s.anyMatch(p -> p.getFileName().toString().equals("results.ndjson")));
        }
        final String meta;
        try (Stream<Path> s = Files.walk(dir.resolve("spool"))) {
            meta = Files.readString(s.filter(p -> p.getFileName().toString().equals("meta.json")).findFirst().orElseThrow());
        }
        assertTrue(meta.contains("\"ended\":true"));
        assertFalse(meta.contains(TOKEN));

        try (FakeServer srv = new FakeServer(TOKEN, true, true)) {
            final Reporter up = Reporter.start(env(srv.url(), "ci-spool"), dir, new PrintStream(new ByteArrayOutputStream()), false);
            up.close();
            assertEquals(1, srv.results.size());
            assertEquals("failed", srv.results.values().iterator().next().get("status"));
            assertEquals(1, srv.attachmentBodies.size());
            assertEquals("blob:", "blob:");
            final List<Object> atts = Json.array(srv.results.values().iterator().next().get("attachments"));
            assertEquals(1, atts.size());
            assertFalse(String.valueOf(Json.object(atts.get(0)).get("id")).startsWith("blob:"));
            try (Stream<Path> s = Files.list(dir.resolve("spool"))) {
                assertEquals(0, s.count());
            }
        }
    }

    @Test
    void flushCommandSendsSessionsToTheirOwnProjectAndListShowsThem() throws Exception {
        final Reporter down = Reporter.start(env("http://127.0.0.1:" + freePort(), "ci-cli"), dir, new PrintStream(new ByteArrayOutputStream()), false);
        down.begin(new TestDescriptor("ju", "k", "t").fullName("p.C#t"), 0).finish("passed", (Throwable) null);
        down.close();
        final ByteArrayOutputStream out = new ByteArrayOutputStream();
        final ByteArrayOutputStream err = new ByteArrayOutputStream();
        final Map<String, String> env = new HashMap<>();
        env.put("FASTPROOF_SPOOL_DIR", dir.resolve("spool").toString());
        assertEquals(0, Flush.run(new String[] {"list"}, env, new PrintStream(out), new PrintStream(err)));
        assertTrue(out.toString(StandardCharsets.UTF_8).contains("\tdemo\tci-cli\tended"), out.toString());
        try (FakeServer srv = new FakeServer(TOKEN, true, true)) {
            env.put("FASTPROOF_URL", srv.url());
            env.put("FASTPROOF_TOKEN", TOKEN);
            final ByteArrayOutputStream out2 = new ByteArrayOutputStream();
            assertEquals(0, Flush.run(new String[] {"flush"}, env, new PrintStream(out2), new PrintStream(err)));
            assertTrue(out2.toString(StandardCharsets.UTF_8).startsWith("sent 1 results from 1 sessions"), out2.toString());
            assertEquals(1, srv.results.size());
            assertTrue(srv.projects.stream().allMatch(p -> p.equals("demo")));
        }
    }

    @Test
    void directUploadsGoToTheStoreAndFallBackWhenItIsUnreachable() throws Exception {
        try (FakeServer srv = new FakeServer(TOKEN, true, true)) {
            srv.direct = true;
            final Map<String, String> env = env(srv.url(), "ci-direct");
            env.put("FASTPROOF_DIRECT_UPLOAD_MIN_BYTES", "100");
            final Reporter r = Reporter.start(env, dir, new PrintStream(new ByteArrayOutputStream()), false);
            final Path big = Files.write(dir.resolve("big.webm"), new byte[500]);
            final byte[] other = new byte[400];
            other[0] = 7;
            final Path big2 = Files.write(dir.resolve("big2.webm"), other);
            final Path small = Files.write(dir.resolve("small.png"), new byte[10]);
            final Attempt a = r.begin(new TestDescriptor("ju", "k1", "t1").fullName("p.C#t1"), 0);
            a.attachFile("video.webm", "video/webm", big);
            a.attachFile("video-copy.webm", "video/webm", big);
            a.attachFile("small.png", "image/png", small);
            a.finish("passed", (Throwable) null);
            r.drain();
            srv.s3Mode = "unreachable";
            final Attempt b = r.begin(new TestDescriptor("ju", "k2", "t2").fullName("p.C#t2"), 0);
            b.attachFile("video2.webm", "video/webm", big2);
            b.finish("passed", (Throwable) null);
            r.close();
            assertEquals(1, srv.directPuts);
            assertTrue(srv.authSeenOnPut.stream().allMatch(String::isEmpty));
            assertEquals(3, srv.attachmentBodies.size());
            assertEquals(2, srv.uploads);
            assertEquals(3, Json.array(srv.results.get(a.id()).get("attachments")).size());
            assertEquals(1, Json.array(srv.results.get(b.id()).get("attachments")).size());
        }
    }

    @Test
    void serverWithoutDirectUploadsKeepsTheServerPath() throws Exception {
        try (FakeServer srv = new FakeServer(TOKEN, true, true)) {
            final Map<String, String> env = env(srv.url(), "ci-nodirect");
            env.put("FASTPROOF_DIRECT_UPLOAD_MIN_BYTES", "10");
            final Reporter r = Reporter.start(env, dir, new PrintStream(new ByteArrayOutputStream()), false);
            final Attempt a = r.begin(new TestDescriptor("ju", "k1", "t1").fullName("p.C#t1"), 0);
            a.attachFile("a.bin", "application/octet-stream", Files.write(dir.resolve("a.bin"), new byte[50]));
            a.finish("passed", (Throwable) null);
            r.close();
            assertEquals(0, srv.directPuts);
            assertEquals(1, srv.uploads);
            assertEquals(1, srv.results.size());
        }
    }
}
