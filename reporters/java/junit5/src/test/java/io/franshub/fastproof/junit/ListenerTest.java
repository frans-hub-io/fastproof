package io.franshub.fastproof.junit;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.platform.engine.discovery.DiscoverySelectors.selectClass;

import io.franshub.fastproof.FakeServer;
import io.franshub.fastproof.TestJson;
import io.franshub.fastproof.Reporter;

import java.io.ByteArrayOutputStream;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.stream.Collectors;

import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.junit.platform.launcher.core.LauncherDiscoveryRequestBuilder;
import org.junit.platform.launcher.core.LauncherFactory;

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
        env.put("FASTPROOF_LAUNCH", "ju-test");
        env.put("FASTPROOF_SPOOL_DIR", dir.resolve("spool").toString());
        env.put("FASTPROOF_FLUSH_TIMEOUT_MS", "4000");
        env.put("JUNIT_FIXTURE_PASSWORD", Fixtures.SECRET);
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

    private void run(final Map<String, String> config, final Class<?>... classes) {
        final LauncherDiscoveryRequestBuilder b = LauncherDiscoveryRequestBuilder.request();
        for (final Class<?> c : classes) {
            b.selectors(selectClass(c));
        }
        config.forEach(b::configurationParameter);
        LauncherFactory.create().execute(b.build());
    }

    private Map<String, Map<String, Object>> byName() {
        reporter.drain();
        final Map<String, Map<String, Object>> out = new HashMap<>();
        for (final Map<String, Object> r : server.results.values()) {
            out.put((String) r.get("name"), r);
        }
        return out;
    }

    private static Object label(final Map<String, Object> r, final String name) {
        return TestJson.array(r.get("labels")).stream().map(TestJson::object).filter(l -> name.equals(l.get("name")))
                .map(l -> l.get("value")).findFirst().orElse(null);
    }

    private static List<String> labels(final Map<String, Object> r, final String name) {
        return TestJson.array(r.get("labels")).stream().map(TestJson::object).filter(l -> name.equals(l.get("name")))
                .map(l -> (String) l.get("value")).collect(Collectors.toList());
    }

    @Test
    void mapsStatusesNamesLabelsAndLiveData() throws Exception {
        start(Map.of());
        run(Map.of(), Fixtures.Basic.class);
        final Map<String, Map<String, Object>> res = byName();
        assertEquals(6, res.size(), res.keySet().toString());
        final Map<String, Object> pays = res.get("pays with a card");
        assertEquals("passed", pays.get("status"));
        assertEquals("PAY-1", pays.get("testCaseId"));
        assertEquals(List.of("smoke"), labels(pays, "tag"));
        assertEquals("team-a", label(pays, "owner"));
        assertEquals("junit5", label(pays, "framework"));
        assertEquals("Fixtures.Basic", label(pays, "testClass"));
        assertEquals("io.franshub.fastproof.junit", label(pays, "package"));
        assertEquals("io.franshub.fastproof.junit.Fixtures.Basic#pays()", pays.get("fullName"));
        assertTrue(((String) pays.get("stdout")).contains("paying ****"));
        assertEquals("warning line\n", pays.get("stderr"));
        assertEquals(List.of("running", "passed"), server.history.get(pays.get("id")));
        assertEquals("inner", TestJson.object(TestJson.array(TestJson.object(TestJson.array(pays.get("steps")).get(0)).get("steps")).get(0)).get("name"));
        assertTrue(server.attachmentBodies.values().stream().anyMatch(b -> new String(b, StandardCharsets.UTF_8).equals("body ****")));
        assertFalse((TestJson.write(server.results) + TestJson.write(server.logLines)).contains(Fixtures.SECRET));
        final String stream = server.logLines.stream().filter(l -> pays.get("id").equals(l.get("resultId")))
                .map(l -> l.get("stream") + ":" + l.get("message")).collect(Collectors.joining("|"));
        assertTrue(stream.contains("log:[shop] log record"), stream);
        assertTrue(stream.contains("stdout:inside"), stream);

        assertEquals("failed", res.get("failsAssertion()").get("status"));
        assertTrue(TestJson.object(res.get("failsAssertion()").get("error")).get("message").toString().contains("one is not two"));
        assertEquals("broken", res.get("breaks()").get("status"));
        assertEquals("skipped", res.get("skipped()").get("status"));
        assertEquals(List.of("skipped"), server.history.get(res.get("skipped()").get("id")));
        assertEquals("not today", TestJson.object(res.get("skipped()").get("error")).get("message"));
        assertEquals("skipped", res.get("aborted()").get("status"));
        final Map<String, Object> nested = res.get("nestedPasses()");
        assertEquals("passed", nested.get("status"));
        assertEquals("Fixtures", label(nested, "suite"));
        assertEquals("Basic › Inner", label(nested, "subSuite"));
        assertEquals("nested\n", nested.get("stdout"));
        assertNotNull(res.get("nestedPasses()").get("traceparent"));
    }

    @Test
    void parameterizedRepeatedAndDynamicTestsAreSeparateResultsWithArguments() throws Exception {
        start(Map.of());
        run(Map.of(), Fixtures.Params.class);
        final List<Map<String, Object>> all = new ArrayList<>(server.results.values());
        final List<Map<String, Object>> doubles = all.stream().filter(r -> ((String) r.get("name")).startsWith("doubles")).toList();
        reporter.drain();
        assertEquals(3, doubles.size(), all.stream().map(r -> r.get("name")).toList().toString());
        final List<String> values = doubles.stream().map(r -> (String) TestJson.object(TestJson.array(r.get("parameters")).get(0)).get("value")).sorted().toList();
        assertEquals(List.of("1", "2", "3"), values);
        assertEquals(1, doubles.stream().map(r -> r.get("fullName")).distinct().count());
        assertEquals(2, all.stream().filter(r -> ((String) r.get("name")).startsWith("repeated")).count());
        final Map<String, Object> dyn = all.stream().filter(r -> ((String) r.get("name")).endsWith("second")).findFirst().orElseThrow();
        assertEquals("failed", dyn.get("status"));
        assertEquals(1, all.stream().filter(r -> ((String) r.get("name")).endsWith("first")).count());
    }

    @Test
    void brokenClassSetupAndDisabledClassAreNotLost() throws Exception {
        start(Map.of());
        run(Map.of(), Fixtures.BrokenSetup.class, Fixtures.DisabledClass.class);
        reporter.drain();
        final List<Map<String, Object>> all = new ArrayList<>(server.results.values());
        assertEquals(1, all.stream().filter(r -> "broken".equals(r.get("status"))
                && TestJson.object(r.get("error")).get("message").toString().contains("no database")).count(), all.toString());
        assertEquals(2, all.stream().filter(r -> "skipped".equals(r.get("status"))
                && "whole class".equals(TestJson.object(r.get("error")).get("message"))).count());
    }

    @Test
    void secondRunOfTheSameTestIsTheNextAttemptOfOneResult() throws Exception {
        start(Map.of());
        Fixtures.Retried.calls = 0;
        run(Map.of(), Fixtures.Retried.class);
        run(Map.of(), Fixtures.Retried.class);
        reporter.drain();
        final List<Map<String, Object>> all = new ArrayList<>(server.results.values());
        assertEquals(2, all.size());
        all.sort((a, b) -> ((Number) a.get("retry")).intValue() - ((Number) b.get("retry")).intValue());
        assertEquals("failed", all.get(0).get("status"));
        assertEquals("passed", all.get(1).get("status"));
        assertEquals(1, all.stream().map(r -> r.get("fullName")).distinct().count());
        assertFalse(all.get(0).get("id").equals(all.get(1).get("id")));
        assertFalse(((String) all.get(0).get("traceparent")).split("-")[1].equals(((String) all.get(1).get("traceparent")).split("-")[1]));
    }

    @Test
    void parallelTestsKeepTheirOwnOutput() throws Exception {
        start(Map.of());
        run(Map.of("junit.jupiter.execution.parallel.enabled", "true", "junit.jupiter.execution.parallel.mode.default", "concurrent",
                "junit.jupiter.execution.parallel.config.strategy", "fixed", "junit.jupiter.execution.parallel.config.fixed.parallelism", "4"),
                Fixtures.Parallel.class);
        final Map<String, Map<String, Object>> res = byName();
        assertEquals(4, res.size());
        for (final Map.Entry<String, Map<String, Object>> e : res.entrySet()) {
            final String prefix = e.getKey().replace("()", "");
            final String out = (String) e.getValue().get("stdout");
            assertEquals(25, out.lines().count(), e.getKey());
            assertTrue(out.lines().allMatch(l -> l.startsWith(prefix + " ")), out);
        }
    }

    @Test
    void testPlanSelectsByNameAndById() throws Exception {
        final Path plan = Files.writeString(dir.resolve("plan.json"), TestJson.write(Map.of("version", "1.0", "tests", List.of(
                Map.of("selector", "io.franshub.fastproof.junit.Fixtures$Basic#failsAssertion"),
                Map.of("id", "PAY-1")))));
        start(Map.of("FASTPROOF_TESTPLAN", plan.toString()));
        run(Map.of(), Fixtures.Basic.class);
        final Map<String, Map<String, Object>> res = byName();
        assertEquals(java.util.Set.of("pays with a card", "failsAssertion()"), res.keySet());
    }

    @Test
    void reportingOffRunsTheTestsUntouched() {
        Reporter.install(null);
        run(Map.of(), Fixtures.Basic.Inner.class.getEnclosingClass());
    }
}
