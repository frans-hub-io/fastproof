package io.franshub.fastproof;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.Map;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class UnitTest {
    @TempDir
    Path dir;

    @Test
    void jsonRoundTrip() {
        final String text = Json.write(Map.of("a", List.of(1, "x\n\"y\"", true), "b", "é\u0001"));
        final Map<String, Object> back = Json.object(Json.parse(text));
        assertEquals("é\u0001", back.get("b"));
        assertEquals("x\n\"y\"", Json.array(back.get("a")).get(1));
    }

    @Test
    void jsonKeepsIntegersIntegers() {
        final Map<String, Object> m = Json.object(Json.parse("{\"retry\":0,\"start\":1763712000000,\"x\":1.5,\"n\":-3}"));
        assertEquals("{\"retry\":0,\"start\":1763712000000,\"x\":1.5,\"n\":-3}", Json.write(m));
    }

    @Test
    void jsonRejectsDeepNestingInsteadOfOverflowingTheStack() {
        assertNull(Json.parse("[".repeat(200_000) + "]".repeat(200_000)));
        assertNull(Json.parse("{\"a\":".repeat(100_000) + "1" + "}".repeat(100_000)));
        final String ok = "[".repeat(Json.MAX_DEPTH) + "]".repeat(Json.MAX_DEPTH);
        assertEquals(ok, Json.write(Json.parse(ok)));
    }

    @Test
    void aTestPlanNestedTooDeeplyIsIgnored() throws Exception {
        final Path plan = Files.writeString(dir.resolve("plan.json"), "{\"tests\":" + "[".repeat(200_000) + "]".repeat(200_000) + "}");
        assertNull(TestPlan.load(plan));
    }

    @Test
    void maskerHidesEnvValuesUrlEncodedFormAndPatterns() {
        final Masker m = new Masker(Masker.DEFAULT_ENV, List.of("literal-value"), List.of("card-\\d{4}"),
                Map.of("SHOP_PASSWORD", "p@ss word!", "PLAIN", "visible-value", "FLAG_TOKEN", "true"));
        assertEquals("a **** b ****", m.mask("a p@ss word! b literal-value"));
        assertEquals("x **** y", m.mask("x p%40ss%20word%21 y"));
        assertEquals("**** and ****", m.mask("card-1234 and literal-value"));
        assertEquals("visible-value true", m.mask("visible-value true"));
    }

    @Test
    void ciRulesMatchTheOtherReporters() {
        final Ci gitlab = Ci.detect(Map.of("GITLAB_CI", "true", "CI_PROJECT_ID", "7", "CI_PIPELINE_ID", "99", "CI_PIPELINE_IID", "3", "CI_PROJECT_PATH", "g/p"));
        assertEquals("gitlab-7-99", gitlab.externalId);
        assertEquals("g/p #3", gitlab.name);
        final Ci github = Ci.detect(Map.of("GITHUB_ACTIONS", "true", "GITHUB_REPOSITORY", "o/r", "GITHUB_RUN_ID", "5"));
        assertEquals("github-o/r-5-1", github.externalId);
        assertEquals("jenkins-job-8", Ci.detect(Map.of("JENKINS_URL", "http://j", "JOB_NAME", "job", "BUILD_NUMBER", "8")).externalId);
        assertNull(Ci.detect(Map.of()));
    }

    @Test
    void enabledFalseInTheEnvironmentAlwaysWins() {
        final Map<String, String> env = Map.of("FASTPROOF_URL", "http://127.0.0.1:1", "FASTPROOF_PROJECT", "demo", "FASTPROOF_ENABLED", "false");
        assertFalse(Config.resolve(env, dir).enabled);
        assertTrue(Config.resolve(Map.of("FASTPROOF_URL", "http://127.0.0.1:1", "FASTPROOF_PROJECT", "demo"), dir).enabled);
        assertFalse(Config.resolve(Map.of("FASTPROOF_URL", "http://127.0.0.1:1", "FASTPROOF_PROJECT", "Bad Key"), dir).enabled);
        assertFalse(Config.resolve(Map.of("FASTPROOF_PROJECT", "demo"), dir).enabled);
    }

    @Test
    void testPlanMatchesSelectorsAndIdsAndFailsOpenOnGarbage() throws Exception {
        final Path file = Files.writeString(dir.resolve("plan.json"),
                "{\"version\":\"1.0\",\"tests\":[{\"id\":\"42\"},{\"selector\":\"a.B#c\"}]}");
        final TestPlan plan = TestPlan.load(file);
        assertEquals(2, plan.size());
        assertTrue(plan.matches(List.of("a.B#c"), List.of()));
        assertTrue(plan.matches(List.of("other"), List.of("42")));
        assertFalse(plan.matches(List.of("a.B#d"), List.of("7")));
        assertNull(TestPlan.load(Files.writeString(dir.resolve("bad.json"), "not json")));
        assertNull(TestPlan.load(dir.resolve("missing.json")));
    }

    @Test
    void headerValuePercentEncodesNonAscii() {
        assertEquals("plain.txt", Client.headerValue("plain.txt"));
        assertEquals("caf%C3%A9.txt", Client.headerValue("café.txt"));
    }

    @Test
    void statusOfSeparatesAssertionsFromErrors() {
        assertEquals("failed", Attempt.statusOf(new AssertionError("x")));
        assertEquals("broken", Attempt.statusOf(new IllegalStateException("x")));
        assertEquals("failed", Attempt.statusOf(new org.opentest4j.AssertionFailedError("x")));
    }
}
