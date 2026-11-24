package io.franshub.fastproof;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.ByteArrayOutputStream;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.logging.Logger;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class ReporterTest {
    static final String TOKEN = "test-token-abcdef";
    static final String SECRET = "correct-horse-battery";

    @TempDir
    Path dir;

    private Reporter reporter(final String url, final Map<String, String> extra) {
        final Map<String, String> env = new HashMap<>();
        env.put("FASTPROOF_URL", url);
        env.put("FASTPROOF_TOKEN", TOKEN);
        env.put("FASTPROOF_PROJECT", "demo");
        env.put("FASTPROOF_SPOOL_DIR", dir.resolve("spool").toString());
        env.put("FASTPROOF_FLUSH_TIMEOUT_MS", "4000");
        env.put("SHOP_PASSWORD", SECRET);
        env.putAll(extra);
        return Reporter.start(env, dir, new PrintStream(new ByteArrayOutputStream()), false);
    }

    private static TestDescriptor desc(final String key) {
        return new TestDescriptor("ju", key, key).fullName("pkg.Cls#" + key).label("framework", "junit5");
    }

    @Test
    void attemptStreamsOutputLogsStepsAndMasksEverything() throws Exception {
        try (FakeServer srv = new FakeServer(TOKEN, true, true)) {
            final Reporter r = reporter(srv.url(), Map.of("FASTPROOF_LAUNCH", "ci-1"));
            final Attempt a = r.begin(desc("pays").parameter("amount", "10 " + SECRET), r.nextRetry("pays"));
            System.out.println("hello " + SECRET);
            System.err.println("oops");
            Logger.getLogger("shop").info("paying " + SECRET);
            FastProof.step("outer", () -> FastProof.step("inner", () -> System.out.println("deep")));
            FastProof.attach("note-" + SECRET + ".txt", "text/plain", ("body " + SECRET).getBytes(StandardCharsets.UTF_8));
            assertTrue(FastProof.trace().isPresent());
            a.finish("passed", (Throwable) null);
            r.close();

            final String id = srv.results.keySet().iterator().next();
            assertTrue(id.startsWith("ju-" + r.runId() + "-"), id);
            assertEquals(List.of("running", "passed"), srv.history.get(id));
            final Map<String, Object> res = srv.results.get(id);
            assertEquals(0, ((Number) res.get("retry")).intValue());
            assertTrue(((String) res.get("traceparent")).matches("00-[0-9a-f]{32}-[0-9a-f]{16}-01"));
            assertTrue(((String) res.get("stdout")).contains("hello ****"));
            assertTrue(((String) res.get("stdout")).contains("deep"));
            assertEquals("oops\n", res.get("stderr"));
            final String all = Json.write(srv.results) + Json.write(srv.logLines) + srv.attachmentNames;
            assertFalse(all.contains(SECRET), all);
            assertTrue(srv.attachmentBodies.values().stream().anyMatch(b -> new String(b, StandardCharsets.UTF_8).equals("body ****")));
            assertEquals("note-****.txt", Json.object(Json.array(res.get("attachments")).get(0)).get("name"));
            final Map<String, Object> outer = Json.object(Json.array(res.get("steps")).get(0));
            assertEquals("outer", outer.get("name"));
            assertEquals("inner", Json.object(Json.array(outer.get("steps")).get(0)).get("name"));
            final List<String> msgs = srv.logLines.stream().map(l -> l.get("stream") + ":" + l.get("message")).toList();
            assertTrue(msgs.contains("stdout:hello ****"), msgs.toString());
            assertTrue(msgs.contains("stderr:oops"), msgs.toString());
            assertTrue(msgs.contains("log:[shop] paying ****"), msgs.toString());
            assertTrue(msgs.contains("step:▶ outer"), msgs.toString());
            final Map<String, Object> deep = srv.logLines.stream().filter(l -> "deep".equals(l.get("message"))).findFirst().orElseThrow();
            assertEquals("outer › inner", deep.get("step"));
            final List<Integer> seqs = srv.logLines.stream().map(l -> ((Number) l.get("seq")).intValue()).toList();
            assertEquals(seqs.stream().sorted().toList(), seqs);
            assertEquals(0, seqs.get(0));
            assertEquals("10 ****", Json.object(Json.array(res.get("parameters")).get(0)).get("value"));
        }
    }

    @Test
    void retriesAreAttemptsOfOneResultAndEachHasItsOwnTrace() throws Exception {
        try (FakeServer srv = new FakeServer(TOKEN, true, true)) {
            final Reporter r = reporter(srv.url(), Map.of("FASTPROOF_LAUNCH", "ci-2"));
            final Attempt a0 = r.begin(desc("flaky"), r.nextRetry("flaky"));
            a0.finish("failed", new AssertionError("first"));
            final Attempt a1 = r.begin(desc("flaky"), r.nextRetry("flaky"));
            a1.finish("passed", (Throwable) null);
            assertEquals(1, a1.retry());
            assertFalse(a0.id().equals(a1.id()));
            assertFalse(a0.trace().traceId().equals(a1.trace().traceId()));
            r.close();
            assertEquals(2, srv.results.size());
            assertEquals("failed", srv.results.get(a0.id()).get("status"));
            assertTrue(Json.object(srv.results.get(a0.id()).get("error")).get("message").toString().contains("first"));
            assertEquals("passed", srv.results.get(a1.id()).get("status"));
        }
    }

    @Test
    void stageBServerKeepsFinalResultsAndFoldsLogsIntoStdout() throws Exception {
        try (FakeServer srv = new FakeServer(TOKEN, false, false)) {
            final Reporter r = reporter(srv.url(), Map.of("FASTPROOF_LAUNCH", "ci-3"));
            final Attempt a = r.begin(desc("old"), 0);
            Logger.getLogger("x").warning("careful now");
            a.finish("passed", (Throwable) null);
            final long t0 = System.currentTimeMillis();
            r.close();
            assertTrue(System.currentTimeMillis() - t0 < 4000);
            assertEquals(List.of("passed"), srv.history.get(a.id()));
            assertTrue(srv.logLines.isEmpty());
        }
    }

    @Test
    void outputOfThreadsStartedByTheTestBelongsToTheAttemptAndNothingLeaksAfterFinish() throws Exception {
        try (FakeServer srv = new FakeServer(TOKEN, true, true)) {
            final Reporter r = reporter(srv.url(), Map.of("FASTPROOF_LAUNCH", "ci-4"));
            final Attempt a = r.begin(desc("threads"), 0);
            final Thread t = new Thread(() -> System.out.println("from child"));
            t.start();
            t.join();
            System.out.print("no newline");
            a.finish("passed", (Throwable) null);
            System.out.println("after finish");
            r.close();
            final String out = (String) srv.results.get(a.id()).get("stdout");
            assertTrue(out.contains("from child"), out);
            assertTrue(out.contains("no newline"), out);
            assertFalse(out.contains("after finish"), out);
            assertNull(Attempt.current());
        }
    }

    @Test
    void skippedAndSetupFailuresAreRecordedWithoutARunningLine() throws Exception {
        try (FakeServer srv = new FakeServer(TOKEN, true, true)) {
            final Reporter r = reporter(srv.url(), Map.of("FASTPROOF_LAUNCH", "ci-5"));
            r.record(desc("skipme"), 0, "skipped", "not today");
            r.record(desc("setup"), 0, "broken", new IllegalStateException("db is down"));
            r.close();
            assertEquals(2, srv.results.size());
            for (final List<String> h : srv.history.values()) {
                assertEquals(1, h.size());
            }
            assertTrue(srv.results.values().stream().anyMatch(m -> "skipped".equals(m.get("status"))
                    && "not today".equals(Json.object(m.get("error")).get("message"))));
        }
    }

    @Test
    void parallelAttemptsKeepTheirOwnOutput() throws Exception {
        try (FakeServer srv = new FakeServer(TOKEN, true, true)) {
            final Reporter r = reporter(srv.url(), Map.of("FASTPROOF_LAUNCH", "ci-6"));
            final int n = 8;
            final Thread[] ts = new Thread[n];
            for (int i = 0; i < n; i++) {
                final int k = i;
                ts[i] = new Thread(() -> {
                    final Attempt a = r.begin(desc("t" + k), 0);
                    for (int j = 0; j < 20; j++) {
                        System.out.println("t" + k + " line " + j);
                    }
                    a.finish("passed", (Throwable) null);
                });
                ts[i].start();
            }
            for (final Thread t : ts) {
                t.join();
            }
            r.close();
            assertEquals(n, srv.results.size());
            for (final Map<String, Object> res : srv.results.values()) {
                final String name = (String) res.get("name");
                final String out = (String) res.get("stdout");
                assertEquals(20, out.lines().count(), name);
                assertTrue(out.lines().allMatch(l -> l.startsWith(name + " ")), out);
            }
        }
    }

    @Test
    void testPlanSelectsAndFailsOpen() throws Exception {
        final Path plan = Files.writeString(dir.resolve("plan.json"), "{\"tests\":[{\"selector\":\"pkg.Cls#a\"}]}");
        try (FakeServer srv = new FakeServer(TOKEN, true, true)) {
            final Reporter r = reporter(srv.url(), Map.of("FASTPROOF_TESTPLAN", plan.toString()));
            assertTrue(r.hasPlan());
            assertTrue(r.selected(List.of("pkg.Cls#a"), List.of()));
            assertFalse(r.selected(List.of("pkg.Cls#b"), List.of()));
            r.close();
            final Reporter broken = reporter(srv.url(), Map.of("FASTPROOF_TESTPLAN", dir.resolve("none.json").toString()));
            assertFalse(broken.hasPlan());
            assertTrue(broken.selected(List.of("anything"), List.of()));
            broken.close();
        }
    }

    @Test
    void offReporterIsNull() {
        assertNull(Reporter.start(Map.of(), dir, new PrintStream(new ByteArrayOutputStream()), false));
        assertNotNull(dir);
    }
}
