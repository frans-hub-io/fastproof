package io.franshub.fastproof.allure;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.qameta.allure.model.Attachment;
import io.qameta.allure.model.Label;
import io.qameta.allure.model.Parameter;
import io.qameta.allure.model.Status;
import io.qameta.allure.model.StatusDetails;
import io.qameta.allure.model.StepResult;
import io.qameta.allure.model.TestResult;

import java.nio.file.Path;
import java.util.List;
import java.util.Map;

import org.junit.jupiter.api.Test;

class UnitTest {
    @Test
    void jsonRoundTrip() {
        final String text = Json.write(Map.of("a", List.of(1, "x\n\"y\"", true), "b", "é\u0001"));
        final Map<String, Object> back = Json.object(Json.parse(text));
        assertEquals("é\u0001", back.get("b"));
        assertEquals("x\n\"y\"", Json.array(back.get("a")).get(1));
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
    void mapperMasksParametersAndSkipsEscapingAttachmentSources() {
        final TestResult r = new TestResult().setUuid("u1").setName("pay").setFullName("a.B.pay").setTestCaseId("tc")
                .setStatus(Status.FAILED).setStart(10L).setStop(20L)
                .setStatusDetails(new StatusDetails().setMessage("boom hunter22x").setTrace("t"));
        r.getParameters().add(new Parameter().setName("pw").setValue("hunter22x").setMode(Parameter.Mode.MASKED));
        r.getParameters().add(new Parameter().setName("env").setValue("hunter22x"));
        r.getLabels().add(new Label().setName("framework").setValue("junit"));
        r.getAttachments().add(new Attachment().setName("ok").setSource("a-attachment.txt").setType("text/plain"));
        r.getAttachments().add(new Attachment().setName("bad").setSource("../x.txt").setType("text/plain"));
        final StepResult s = new StepResult().setName("inner").setStatus(Status.PASSED);
        s.getAttachments().add(new Attachment().setName("n").setSource("b-attachment.png").setType("image/png"));
        r.getSteps().add(s);
        final Masker masker = new Masker(List.of(), List.of("hunter22x"), List.of(), Map.of());
        final Mapper m = new Mapper(masker, Path.of("/tmp/res"));
        final Map<String, Object> out = m.result(r, "id1", 1, "00-aa-bb-01", 5);
        assertEquals("failed", out.get("status"));
        assertEquals(1, out.get("retry"));
        assertEquals(10L, out.get("start"));
        assertEquals("boom ****", Json.object(out.get("error")).get("message"));
        final String json = Json.write(out);
        assertTrue(!json.contains("hunter22x"));
        assertTrue(json.contains("\"mode\":\"masked\""));
        assertEquals(2, m.files.size());
        assertEquals("ok.txt", m.files.get(0).name());
        assertEquals(Path.of("/tmp/res/a-attachment.txt"), m.files.get(0).path());
    }

    @Test
    void ciRulesMatchTheOtherReporters() {
        final Ci gitlab = Ci.detect(Map.of("GITLAB_CI", "true", "CI_PROJECT_ID", "7", "CI_PIPELINE_ID", "99", "CI_PIPELINE_IID", "3", "CI_PROJECT_PATH", "g/p"));
        assertEquals("gitlab-7-99", gitlab.externalId);
        assertEquals("g/p #3", gitlab.name);
        final Ci github = Ci.detect(Map.of("GITHUB_ACTIONS", "true", "GITHUB_REPOSITORY", "o/r", "GITHUB_RUN_ID", "5"));
        assertEquals("github-o/r-5-1", github.externalId);
        assertEquals("jenkins-job-8", Ci.detect(Map.of("JENKINS_URL", "http://j", "JOB_NAME", "job", "BUILD_NUMBER", "8")).externalId);
    }
}
