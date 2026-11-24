package io.franshub.fastproof;

import java.io.PrintWriter;
import java.io.StringWriter;
import java.nio.file.Path;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Deque;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * One attempt of one test: collects output, log records, steps, attachments and the outcome, streams them live and
 * sends the final result. Nothing here throws into the test.
 */
public final class Attempt {
    private static final InheritableThreadLocal<Attempt> CURRENT = new InheritableThreadLocal<>();
    private static final int MAX_STEPS = 2000;
    private static final int MAX_DEPTH = 32;
    private static final int MAX_LABELS = 200;
    private static final int MAX_TEXT = 1024 * 1024;
    private static final int MAX_ATTACH_BYTES = 64 * 1024 * 1024;
    private static final String[] LEVELS = {"trace", "debug", "info", "warn", "error"};

    /** A step of the attempt. */
    public static final class Step {
        final String name;
        final long start = System.currentTimeMillis();
        long stop;
        String status = "passed";
        Map<String, Object> error;
        final List<Step> children = new ArrayList<>();
        final Step parent;
        boolean ended;

        Step(final String name, final Step parent) {
            this.name = name;
            this.parent = parent;
        }
    }

    private final Reporter reporter;
    private final Client client;
    private final TestDescriptor desc;
    private final String id;
    private final int retry;
    private final Trace trace;
    private final long start;
    private final String thread;
    private final Object lock = new Object();
    private final StringBuilder stdout = new StringBuilder();
    private final StringBuilder stderr = new StringBuilder();
    private final StringBuilder logBuffer = new StringBuilder();
    private final List<Step> roots = new ArrayList<>();
    private final ThreadLocal<Step> openStep = new ThreadLocal<>();
    private final List<Client.FileRef> files = new ArrayList<>();
    private final List<String[]> extraLabels = new ArrayList<>();
    private final List<String[]> extraParameters = new ArrayList<>();
    private int steps;
    private boolean finished;
    private boolean truncatedOut;
    private String testCaseId;

    Attempt(final Reporter reporter, final Client client, final TestDescriptor desc, final String id, final int retry,
            final Trace trace, final long start) {
        this.reporter = reporter;
        this.client = client;
        this.desc = desc;
        this.id = id;
        this.retry = retry;
        this.trace = trace;
        this.start = start;
        this.thread = Thread.currentThread().getName();
        this.testCaseId = desc.testCaseId;
    }

    /** The attempt running on this thread (or on the thread that started this one), if any. */
    public static Attempt current() {
        return CURRENT.get();
    }

    public String id() {
        return id;
    }

    public int retry() {
        return retry;
    }

    public Trace trace() {
        return trace;
    }

    /** Makes this the attempt of the current thread, so output, log records and {@link FastProof} calls reach it. */
    public Attempt enter() {
        CURRENT.set(this);
        return this;
    }

    private void leave() {
        if (CURRENT.get() == this) {
            CURRENT.remove();
        }
    }

    boolean sameReporter(final Reporter r) {
        return reporter == r;
    }

    // ---- output and logs --------------------------------------------------------------------------------

    void output(final String stream, final String line) {
        final boolean err = stream.equals("stderr");
        synchronized (lock) {
            if (finished) {
                return;
            }
            final StringBuilder sb = err ? stderr : stdout;
            if (sb.length() < MAX_TEXT) {
                sb.append(line).append('\n');
            } else if (!truncatedOut) {
                truncatedOut = true;
            }
        }
        client.log(id, retry, err ? "warn" : "info", stream, line, stepPath());
    }

    /** A log record ({@code trace|debug|info|warn|error}); also reaches the final result when the server has no live logs. */
    public void log(final String level, final String message) {
        if (message == null) {
            return;
        }
        final String lv = normalize(level);
        synchronized (lock) {
            if (finished) {
                return;
            }
            if (logBuffer.length() < MAX_TEXT) {
                logBuffer.append('[').append(lv.toUpperCase()).append("] ").append(message).append('\n');
            }
        }
        client.log(id, retry, lv, "log", message, stepPath());
    }

    private static String normalize(final String level) {
        final String l = level == null ? "info" : level.toLowerCase();
        for (final String v : LEVELS) {
            if (v.equals(l)) {
                return v;
            }
        }
        return switch (l) {
            case "warning" -> "warn";
            case "fatal", "critical", "severe" -> "error";
            default -> "info";
        };
    }

    // ---- steps ------------------------------------------------------------------------------------------

    /** Opens a step on this thread; close it with {@link #endStep}. Returns {@code null} when the step limit is reached. */
    public Step startStep(final String name) {
        final Step parent = openStep.get();
        synchronized (lock) {
            if (finished || steps >= MAX_STEPS || depth(parent) >= MAX_DEPTH) {
                return null;
            }
            steps++;
            final Step s = new Step(client.masker.mask(name == null ? "step" : cut(name, 1000)), parent);
            if (parent == null) {
                roots.add(s);
            } else {
                parent.children.add(s);
            }
            openStep.set(s);
            client.log(id, retry, "info", "step", "▶ " + s.name, path(s));
            return s;
        }
    }

    private static int depth(Step s) {
        int d = 0;
        while (s != null) {
            d++;
            s = s.parent;
        }
        return d;
    }

    /** Closes a step; a non-null {@code error} marks it failed or broken. */
    public void endStep(final Step s, final Throwable error) {
        if (s == null) {
            return;
        }
        synchronized (lock) {
            if (s.ended) {
                return;
            }
            s.ended = true;
            s.stop = System.currentTimeMillis();
            if (error != null) {
                s.status = statusOf(error);
                s.error = error(error);
            }
            openStep.set(s.parent);
            final boolean failed = error != null;
            final String msg = failed ? "✘ " + s.name : "✔ " + s.name + " (" + (s.stop - s.start) + " ms)";
            client.log(id, retry, failed ? "error" : "info", "step", msg, path(s));
        }
    }

    private String stepPath() {
        final Step s = openStep.get();
        return s == null ? null : path(s);
    }

    private static String path(Step s) {
        final Deque<String> names = new ArrayDeque<>();
        while (s != null) {
            names.addFirst(s.name);
            s = s.parent;
        }
        return String.join(" › ", names);
    }

    // ---- attachments and extras -------------------------------------------------------------------------

    public void attach(final String name, final String contentType, final byte[] body) {
        if (body == null) {
            return;
        }
        if (body.length > MAX_ATTACH_BYTES) {
            client.warnOnce("attach:" + name, "attachment " + name + " is larger than 64 MiB; attach it as a file instead");
            return;
        }
        synchronized (lock) {
            if (!finished) {
                files.add(new Client.FileRef(client.masker.mask(name), contentType == null ? "application/octet-stream" : contentType, null, body));
            }
        }
    }

    public void attachFile(final String name, final String contentType, final Path file) {
        if (file == null) {
            return;
        }
        synchronized (lock) {
            if (!finished) {
                final String n = name != null ? name : file.getFileName().toString();
                files.add(new Client.FileRef(client.masker.mask(n), contentType == null ? "application/octet-stream" : contentType, file, null));
            }
        }
    }

    public void label(final String name, final String value) {
        synchronized (lock) {
            if (name != null && value != null && !finished) {
                extraLabels.add(new String[] {name, value});
            }
        }
    }

    public void parameter(final String name, final String value) {
        synchronized (lock) {
            if (name != null && !finished) {
                extraParameters.add(new String[] {name, value});
            }
        }
    }

    /** Replaces the parameters the attempt was opened with (the extension learns the argument values after the start). */
    public void replaceParameters(final List<String[]> parameters) {
        synchronized (lock) {
            if (!finished) {
                desc.parameters.clear();
                desc.parameters.addAll(parameters);
            }
        }
    }

    public void testCaseId(final String value) {
        synchronized (lock) {
            testCaseId = value;
        }
    }

    // ---- results ----------------------------------------------------------------------------------------

    /** {@code failed} for assertion errors, {@code broken} for anything else. */
    public static String statusOf(final Throwable t) {
        if (t instanceof AssertionError) {
            return "failed";
        }
        for (Class<?> c = t == null ? null : t.getClass(); c != null; c = c.getSuperclass()) {
            if (c.getName().endsWith("AssertionFailedError") || c.getName().endsWith("ComparisonFailure")) {
                return "failed";
            }
        }
        return "broken";
    }

    private Map<String, Object> error(final Throwable t) {
        final Map<String, Object> e = new LinkedHashMap<>();
        final String msg = t.getMessage() == null ? t.getClass().getName() : t.getClass().getName() + ": " + t.getMessage();
        e.put("message", cut(client.masker.mask(msg), 64 * 1024));
        final StringWriter sw = new StringWriter();
        t.printStackTrace(new PrintWriter(sw));
        e.put("trace", cut(client.masker.mask(sw.toString()), MAX_TEXT));
        return e;
    }

    private static String cut(final String s, final int max) {
        return s.length() <= max ? s : s.substring(0, max) + "\n...[truncated]";
    }

    private String mask(final String s) {
        return s == null ? null : client.masker.mask(s);
    }

    private Map<String, Object> parameter(final String[] p) {
        final Map<String, Object> m = new LinkedHashMap<>();
        m.put("name", cut(mask(p[0]), 1000));
        if (p[1] != null) {
            m.put("value", cut(mask(p[1]), 1000));
        }
        return m;
    }

    private Map<String, Object> identity() {
        final Map<String, Object> m = new LinkedHashMap<>();
        m.put("name", cut(mask(desc.name), 1000));
        if (desc.fullName != null) {
            m.put("fullName", cut(mask(desc.fullName), 2000));
        }
        if (testCaseId != null) {
            m.put("testCaseId", testCaseId);
        }
        final List<Object> ps = new ArrayList<>();
        for (final String[] p : desc.parameters) {
            ps.add(parameter(p));
        }
        for (final String[] p : extraParameters) {
            ps.add(parameter(p));
        }
        m.put("parameters", ps);
        return m;
    }

    private List<Object> labels() {
        final List<Object> out = new ArrayList<>();
        final List<String[]> all = new ArrayList<>(desc.labels);
        all.addAll(extraLabels);
        all.add(new String[] {"thread", thread});
        for (final String[] l : all) {
            if (out.size() >= MAX_LABELS) {
                break;
            }
            final Map<String, Object> lm = new LinkedHashMap<>();
            lm.put("name", l[0]);
            lm.put("value", cut(mask(l[1]), 1000));
            out.add(lm);
        }
        return out;
    }

    /** Sends the {@code running} line: the identity of the final line, known at the start. */
    void open() {
        final Map<String, Object> line = new LinkedHashMap<>();
        line.put("id", id);
        synchronized (lock) {
            line.putAll(identity());
        }
        line.put("start", start);
        line.put("retry", retry);
        line.put("labels", labels());
        client.openRunning(line);
    }

    private Map<String, Object> stepMap(final Step s, final long fallbackStop) {
        final Map<String, Object> m = new LinkedHashMap<>();
        m.put("name", s.name);
        m.put("status", s.status);
        m.put("start", s.start);
        final long stop = s.ended ? s.stop : Math.max(s.start, fallbackStop);
        m.put("stop", stop);
        m.put("durationMs", Math.max(0, stop - s.start));
        if (!s.ended) {
            m.put("status", "broken");
        }
        if (s.error != null) {
            m.put("error", s.error);
        }
        if (!s.children.isEmpty()) {
            final List<Object> kids = new ArrayList<>();
            for (final Step c : s.children) {
                kids.add(stepMap(c, fallbackStop));
            }
            m.put("steps", kids);
        }
        return m;
    }

    /** Ends the attempt with an outcome and hands the final result (and its files) to the sender. */
    public void finish(final String status, final Throwable error) {
        finishWith(status, error == null ? null : error(error));
    }

    /** Ends the attempt with a message only (skips: the reason). */
    public void finish(final String status, final String message) {
        Map<String, Object> e = null;
        if (message != null && !message.isBlank()) {
            e = new LinkedHashMap<>();
            e.put("message", cut(client.masker.mask(message), 64 * 1024));
        }
        finishWith(status, e);
    }

    private void finishWith(final String status, final Map<String, Object> error) {
        LogCapture.flushThread();
        final Map<String, Object> result = new LinkedHashMap<>();
        final List<Client.FileRef> toUpload;
        final long stop = System.currentTimeMillis();
        synchronized (lock) {
            if (finished) {
                return;
            }
            finished = true;
            result.put("id", id);
            result.putAll(identity());
            result.put("status", status);
            result.put("retry", retry);
            result.put("traceparent", trace.traceparent());
            if (desc.description != null) {
                result.put("description", cut(mask(desc.description), 64 * 1024));
            }
            result.put("start", start);
            result.put("stop", Math.max(stop, start));
            result.put("durationMs", Math.max(0, stop - start));
            result.put("labels", labels());
            final List<Object> links = new ArrayList<>();
            for (final String[] l : desc.links) {
                final Map<String, Object> lm = new LinkedHashMap<>();
                lm.put("url", l[1]);
                lm.put("name", l[0] == null ? null : mask(l[0]));
                lm.put("type", l[2]);
                links.add(lm);
            }
            if (!links.isEmpty()) {
                result.put("links", links);
            }
            if (desc.sourceFile != null) {
                final Map<String, Object> src = new LinkedHashMap<>();
                src.put("file", desc.sourceFile);
                src.put("line", desc.sourceLine);
                result.put("source", src);
            }
            Map<String, Object> err = error;
            if (err == null && (status.equals("failed") || status.equals("broken"))) {
                err = Map.of("message", "the test failed without details");
            }
            if (err != null) {
                result.put("error", err);
            }
            if (!roots.isEmpty()) {
                final List<Object> st = new ArrayList<>();
                for (final Step s : roots) {
                    st.add(stepMap(s, stop));
                }
                result.put("steps", st);
            }
            final StringBuilder out = new StringBuilder(stdout);
            if (!client.logsLive() && logBuffer.length() > 0) {
                out.append("--- log ---\n").append(logBuffer);
            }
            if (truncatedOut) {
                out.append("...[output truncated]\n");
            }
            if (out.length() > 0) {
                result.put("stdout", client.masker.mask(out.toString()));
            }
            if (stderr.length() > 0) {
                result.put("stderr", client.masker.mask(stderr.toString()));
            }
            toUpload = new ArrayList<>(files);
        }
        leave();
        client.finish(result, toUpload, status.equals("passed") || status.equals("skipped"));
    }
}
