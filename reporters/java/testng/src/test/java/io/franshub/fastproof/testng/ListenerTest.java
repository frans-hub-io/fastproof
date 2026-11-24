package io.franshub.fastproof.testng;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.franshub.fastproof.FakeServer;
import io.franshub.fastproof.Reporter;
import io.franshub.fastproof.TestJson;

import java.io.ByteArrayOutputStream;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.stream.Collectors;

import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.testng.TestNG;
import org.testng.xml.XmlSuite;

class ListenerTest {
    static final String TOKEN = "test-token-abcdef";

    @TempDir
    Path dir;

    private FakeServer server;
    private Reporter reporter;

    private void start(final Map<String, String> extra) throws Exception {
        server = new FakeServer(TOKEN, true, true);
        final Map<String, String> env = new HashMap<>();
        env.put("FASTPROOF_URL", server.url());
        env.put("FASTPROOF_TOKEN", TOKEN);
        env.put("FASTPROOF_PROJECT", "demo");
        env.put("FASTPROOF_LAUNCH", "tn-test");
        env.put("FASTPROOF_SPOOL_DIR", dir.resolve("spool").toString());
        env.put("FASTPROOF_FLUSH_TIMEOUT_MS", "4000");
        env.put("TESTNG_FIXTURE_PASSWORD", Fixtures.SECRET);
        env.putAll(extra);
        reporter = Reporter.start(env, dir, new PrintStream(new ByteArrayOutputStream()), false);
        Reporter.install(reporter);
    }

    @AfterEach
    void stop() {
        Reporter.install(null);
        if (reporter != null) {
            reporter.close();
        }
        if (server != null) {
            server.close();
        }
    }

    private static void run(final boolean parallel, final Class<?>... classes) {
        final TestNG tng = new TestNG(false);
        tng.setUseDefaultListeners(false);
        tng.setVerbose(0);
        tng.setTestClasses(classes);
        if (parallel) {
            tng.setParallel(XmlSuite.ParallelMode.METHODS);
            tng.setThreadCount(4);
        }
        tng.run();
    }

    private List<Map<String, Object>> results() {
        reporter.drain();
        return new ArrayList<>(server.results.values());
    }

    private static Map<String, Object> named(final List<Map<String, Object>> all, final String name) {
        return all.stream().filter(r -> name.equals(r.get("name"))).findFirst().orElseThrow(() -> new AssertionError(name + " in " + all.stream().map(r -> r.get("name")).toList()));
    }

    private static List<String> labels(final Map<String, Object> r, final String name) {
        return TestJson.array(r.get("labels")).stream().map(TestJson::object).filter(l -> name.equals(l.get("name")))
                .map(l -> (String) l.get("value")).collect(Collectors.toList());
    }

    @Test
    void mapsStatusesNamesLabelsAndLiveData() throws Exception {
        start(Map.of());
        run(false, Fixtures.Basic.class);
        final List<Map<String, Object>> all = results();
        assertEquals(Set.of("pays", "failsAssertion", "breaks", "skips"), all.stream().map(r -> (String) r.get("name")).collect(Collectors.toSet()));
        final Map<String, Object> pays = named(all, "pays");
        assertEquals("passed", pays.get("status"));
        assertEquals("PAY-9", pays.get("testCaseId"));
        assertEquals(List.of("smoke"), labels(pays, "tag"));
        assertEquals(List.of("testng"), labels(pays, "framework"));
        assertEquals("pays by card", pays.get("description"));
        assertEquals("io.franshub.fastproof.testng.Fixtures.Basic#pays()", pays.get("fullName"));
        assertTrue(((String) pays.get("stdout")).contains("paying ****"));
        assertEquals("warning line\n", pays.get("stderr"));
        assertEquals(List.of("running", "passed"), server.history.get(pays.get("id")));
        assertEquals("inner", TestJson.object(TestJson.array(TestJson.object(TestJson.array(pays.get("steps")).get(0)).get("steps")).get(0)).get("name"));
        assertTrue(server.attachmentBodies.values().stream().anyMatch(b -> new String(b, StandardCharsets.UTF_8).equals("body ****")));
        assertFalse((TestJson.write(server.results) + TestJson.write(server.logLines)).contains(Fixtures.SECRET));
        final String stream = server.logLines.stream().filter(l -> pays.get("id").equals(l.get("resultId")))
                .map(l -> l.get("stream") + ":" + l.get("message")).collect(Collectors.joining("|"));
        assertTrue(stream.contains("log:[shop] log record"), stream);
        assertEquals("failed", named(all, "failsAssertion").get("status"));
        assertEquals("broken", named(all, "breaks").get("status"));
        assertEquals("skipped", named(all, "skips").get("status"));
        assertEquals("needs linux", TestJson.object(named(all, "skips").get("error")).get("message"));
    }

    @Test
    void retriesAreAttemptsOfOneResult() throws Exception {
        start(Map.of());
        Fixtures.Flaky.calls = 0;
        run(false, Fixtures.Flaky.class);
        final List<Map<String, Object>> all = results();
        assertEquals(3, all.size(), all.toString());
        all.sort((a, b) -> ((Number) a.get("retry")).intValue() - ((Number) b.get("retry")).intValue());
        assertEquals(List.of("failed", "failed", "passed"), all.stream().map(r -> r.get("status")).toList());
        assertEquals(List.of(0, 1, 2), all.stream().map(r -> ((Number) r.get("retry")).intValue()).toList());
        assertEquals(1, all.stream().map(r -> r.get("fullName")).distinct().count());
        assertEquals(3, all.stream().map(r -> ((String) r.get("traceparent")).split("-")[1]).distinct().count());
        assertEquals(3, all.stream().map(r -> r.get("id")).distinct().count());
    }

    @Test
    void invocationCountAndDataProviderRowsAreSeparateResults() throws Exception {
        start(Map.of());
        run(false, Fixtures.Repeated.class);
        final List<Map<String, Object>> all = results();
        final List<Map<String, Object>> repeats = all.stream().filter(r -> "repeats".equals(r.get("name"))).toList();
        assertEquals(3, repeats.size());
        assertEquals(Set.of("1", "2", "3"), repeats.stream().map(r -> (String) TestJson.object(TestJson.array(r.get("parameters")).get(0)).get("value")).collect(Collectors.toSet()));
        assertTrue(repeats.stream().allMatch(r -> ((Number) r.get("retry")).intValue() == 0));
        final List<Map<String, Object>> rows = all.stream().filter(r -> "rowsTest".equals(r.get("name"))).toList();
        assertEquals(3, rows.size());
        assertEquals(Set.of("1/a", "2/b", "3/c"), rows.stream().map(r -> {
            final List<Object> ps = TestJson.array(r.get("parameters"));
            return TestJson.object(ps.get(0)).get("value") + "/" + TestJson.object(ps.get(1)).get("value");
        }).collect(Collectors.toSet()));
        assertEquals("number", TestJson.object(TestJson.array(rows.get(0).get("parameters")).get(0)).get("name"));
        assertTrue(rows.stream().allMatch(r -> ((Number) r.get("retry")).intValue() == 0));
    }

    @Test
    void configurationFailureIsReportedAndDependentTestsAreSkipped() throws Exception {
        start(Map.of());
        run(false, Fixtures.BrokenSetup.class);
        final List<Map<String, Object>> all = results();
        assertTrue(all.stream().anyMatch(r -> "broken".equals(r.get("status")) && ((String) r.get("name")).contains("configuration")
                && TestJson.object(r.get("error")).get("message").toString().contains("no database")), all.toString());
        assertTrue(all.stream().anyMatch(r -> "skipped".equals(r.get("status")) && "neverRuns".equals(r.get("name"))), all.toString());
    }

    @Test
    void parallelMethodsKeepTheirOwnOutput() throws Exception {
        start(Map.of());
        run(true, Fixtures.Parallel.class);
        final List<Map<String, Object>> all = results();
        assertEquals(4, all.size());
        for (final Map<String, Object> r : all) {
            final String name = (String) r.get("name");
            final String out = (String) r.get("stdout");
            assertEquals(25, out.lines().count(), name);
            assertTrue(out.lines().allMatch(l -> l.startsWith(name + " ")), out);
        }
    }

    @Test
    void testPlanSelectsByNameAndById() throws Exception {
        final Path plan = Files.writeString(dir.resolve("plan.json"), TestJson.write(Map.of("version", "1.0", "tests", List.of(
                Map.of("selector", "io.franshub.fastproof.testng.Fixtures$Basic#failsAssertion"),
                Map.of("id", "PAY-9")))));
        start(Map.of("FASTPROOF_TESTPLAN", plan.toString()));
        run(false, Fixtures.Basic.class);
        final List<Map<String, Object>> all = results();
        assertEquals(Set.of("pays", "failsAssertion"), all.stream().map(r -> (String) r.get("name")).collect(Collectors.toSet()));
    }

    @Test
    void theSameTestRunAgainInTheJvmIsTheNextAttempt() throws Exception {
        start(Map.of());
        run(false, Fixtures.Basic.class);
        run(false, Fixtures.Basic.class);
        final List<Map<String, Object>> all = results();
        final List<Map<String, Object>> fails = all.stream().filter(r -> "failsAssertion".equals(r.get("name"))).toList();
        assertEquals(2, fails.size());
        assertEquals(Set.of(0, 1), fails.stream().map(r -> ((Number) r.get("retry")).intValue()).collect(Collectors.toSet()));
        assertEquals(1, fails.stream().map(r -> r.get("fullName")).distinct().count());
    }

    @Test
    void globalRetriesCanBeSwitchedOnWithoutTouchingTheTests() throws Exception {
        start(Map.of());
        System.setProperty("fastproof.testng.retries", "1");
        try {
            run(false, Fixtures.Basic.class);
        } finally {
            System.clearProperty("fastproof.testng.retries");
        }
        final List<Map<String, Object>> all = results();
        assertEquals(2, all.stream().filter(r -> "failsAssertion".equals(r.get("name"))).count());
        assertEquals(1, all.stream().filter(r -> "pays".equals(r.get("name"))).count());
    }
}
