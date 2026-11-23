package io.franshub.fastproof.allure;

import io.qameta.allure.model.Attachment;
import io.qameta.allure.model.Label;
import io.qameta.allure.model.Link;
import io.qameta.allure.model.Parameter;
import io.qameta.allure.model.StatusDetails;
import io.qameta.allure.model.StepResult;
import io.qameta.allure.model.TestResult;

import java.nio.file.Path;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

/** Converts an allure-java {@link TestResult} into a FastProof report result (reporters/PROTOCOL.md). */
final class Mapper {
    private static final int MAX_STEPS = 2000;
    private static final int MAX_DEPTH = 32;
    private static final int MAX_LABELS = 200;
    private static final int MAX_TEXT = 4 * 1024 * 1024;

    private final Masker masker;
    private final Path resultsDir;
    private int steps;
    private final Set<String> seen = new LinkedHashSet<>();
    final List<Client.FileRef> files = new ArrayList<>();

    Mapper(final Masker masker, final Path resultsDir) {
        this.masker = masker;
        this.resultsDir = resultsDir;
    }

    private String mask(final String s) {
        return masker.mask(s);
    }

    private static String cut(final String s, final int max) {
        return s.length() <= max ? s : s.substring(0, max) + "\n...[truncated]";
    }

    private static String status(final Object s) {
        if (s == null) {
            return "unknown";
        }
        final String v = s.toString().toLowerCase();
        return List.of("passed", "failed", "broken", "skipped").contains(v) ? v : "unknown";
    }

    Map<String, Object> parameter(final Parameter p) {
        final Map<String, Object> m = new LinkedHashMap<>();
        m.put("name", mask(p.getName()));
        final String mode = p.getMode() == null ? "" : p.getMode().toString().toLowerCase();
        if (mode.equals("masked") || mode.equals("hidden")) {
            m.put("mode", mode);
            m.put("value", Masker.MASK);
        } else if (p.getValue() != null) {
            m.put("value", mask(cut(p.getValue(), 1000)));
        }
        if (Boolean.TRUE.equals(p.getExcluded())) {
            m.put("excluded", true);
        }
        return m;
    }

    /** Fields that identify the test; the running line carries them so the final line updates the same result. */
    Map<String, Object> identity(final TestResult r) {
        final Map<String, Object> m = new LinkedHashMap<>();
        m.put("name", mask(cut(r.getName() != null ? r.getName() : String.valueOf(r.getFullName()), 1000)));
        if (r.getFullName() != null) {
            m.put("fullName", mask(r.getFullName()));
        }
        if (r.getTestCaseId() != null) {
            m.put("testCaseId", r.getTestCaseId());
        }
        final List<Object> params = new ArrayList<>();
        for (final Parameter p : r.getParameters()) {
            params.add(parameter(p));
        }
        m.put("parameters", params);
        return m;
    }

    private Map<String, Object> error(final StatusDetails d) {
        if (d == null || (d.getMessage() == null && d.getTrace() == null)) {
            return null;
        }
        final Map<String, Object> e = new LinkedHashMap<>();
        if (d.getMessage() != null) {
            e.put("message", mask(cut(d.getMessage(), 64 * 1024)));
        }
        if (d.getTrace() != null) {
            e.put("trace", mask(cut(d.getTrace(), MAX_TEXT)));
        }
        return e;
    }

    private void attachments(final List<Attachment> list) {
        for (final Attachment a : list) {
            final String src = a.getSource();
            if (src == null || src.contains("/") || src.contains("\\") || src.contains("..") || !seen.add(src)) {
                continue;
            }
            String name = a.getName() == null || a.getName().isBlank() ? src : a.getName();
            final int dot = src.lastIndexOf('.');
            if (name.indexOf('.') < 0 && dot >= 0) {
                name += src.substring(dot);
            }
            files.add(new Client.FileRef(mask(name), a.getType() == null ? "application/octet-stream" : a.getType(), resultsDir.resolve(src)));
        }
    }

    private Map<String, Object> step(final StepResult s, final int depth) {
        if (steps >= MAX_STEPS || depth > MAX_DEPTH) {
            return null;
        }
        steps++;
        final Map<String, Object> m = new LinkedHashMap<>();
        m.put("name", mask(cut(s.getName() == null ? "step" : s.getName(), 1000)));
        m.put("status", status(s.getStatus()));
        if (s.getStart() != null) {
            m.put("start", s.getStart());
        }
        if (s.getStop() != null) {
            m.put("stop", s.getStop());
        }
        if (s.getStart() != null && s.getStop() != null && s.getStop() >= s.getStart()) {
            m.put("durationMs", s.getStop() - s.getStart());
        }
        final Map<String, Object> err = error(s.getStatusDetails());
        if (err != null) {
            m.put("error", err);
        }
        if (!s.getParameters().isEmpty()) {
            final List<Object> ps = new ArrayList<>();
            for (final Parameter p : s.getParameters()) {
                ps.add(parameter(p));
            }
            m.put("parameters", ps);
        }
        attachments(s.getAttachments());
        final List<Object> kids = new ArrayList<>();
        for (final StepResult c : s.getSteps()) {
            final Map<String, Object> k = step(c, depth + 1);
            if (k != null) {
                kids.add(k);
            }
        }
        if (!kids.isEmpty()) {
            m.put("steps", kids);
        }
        return m;
    }

    Map<String, Object> result(final TestResult r, final String id, final int attempt, final String traceparent, final long fallbackStart) {
        final Map<String, Object> m = new LinkedHashMap<>();
        m.put("id", id);
        m.putAll(identity(r));
        m.put("status", status(r.getStatus()));
        m.put("retry", attempt);
        m.put("traceparent", traceparent);
        if (r.getDescription() != null) {
            m.put("description", mask(cut(r.getDescription(), 64 * 1024)));
        }
        final long start = r.getStart() != null ? r.getStart() : fallbackStart;
        final long stop = Math.max(r.getStop() != null ? r.getStop() : System.currentTimeMillis(), start);
        m.put("start", start);
        m.put("stop", stop);
        m.put("durationMs", stop - start);
        final List<Object> labels = new ArrayList<>();
        for (final Label l : r.getLabels()) {
            if (labels.size() < MAX_LABELS && l.getName() != null && l.getValue() != null) {
                final Map<String, Object> lm = new LinkedHashMap<>();
                lm.put("name", l.getName());
                lm.put("value", mask(cut(l.getValue(), 1000)));
                labels.add(lm);
            }
        }
        m.put("labels", labels);
        final List<Object> links = new ArrayList<>();
        for (final Link l : r.getLinks()) {
            if (l.getUrl() != null) {
                final Map<String, Object> lm = new LinkedHashMap<>();
                lm.put("url", l.getUrl());
                lm.put("name", l.getName() == null ? null : mask(l.getName()));
                lm.put("type", l.getType());
                links.add(lm);
            }
        }
        if (!links.isEmpty()) {
            m.put("links", links);
        }
        final Map<String, Object> err = error(r.getStatusDetails());
        if (err != null) {
            m.put("error", err);
        } else if (m.get("status").equals("failed") || m.get("status").equals("broken")) {
            m.put("error", Map.of("message", "the test failed without details"));
        }
        attachments(r.getAttachments());
        final List<Object> st = new ArrayList<>();
        for (final StepResult s : r.getSteps()) {
            final Map<String, Object> k = step(s, 1);
            if (k != null) {
                st.add(k);
            }
        }
        if (!st.isEmpty()) {
            m.put("steps", st);
        }
        return m;
    }
}
