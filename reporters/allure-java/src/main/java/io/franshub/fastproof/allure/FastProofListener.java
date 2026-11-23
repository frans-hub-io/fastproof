package io.franshub.fastproof.allure;

import io.qameta.allure.listener.StepLifecycleListener;
import io.qameta.allure.listener.TestLifecycleListener;
import io.qameta.allure.model.Label;
import io.qameta.allure.model.Parameter;
import io.qameta.allure.model.StepResult;
import io.qameta.allure.model.TestResult;
import io.qameta.allure.util.PropertiesUtils;

import java.io.PrintStream;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.HexFormat;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Allure lifecycle listener (loaded through {@link java.util.ServiceLoader}, so it needs no change in the tests):
 * opens each attempt as {@code running}, streams steps, and sends the final result with its attachments, while
 * allure-java keeps writing allure-results. Reporting problems never reach the tests.
 */
public final class FastProofListener implements TestLifecycleListener, StepLifecycleListener {
    private static final ThreadLocal<Attempt> CURRENT = new ThreadLocal<>();
    private static final Object INIT = new Object();
    private static volatile State state;

    private record Attempt(String uuid, String id, int retry, Trace trace, long start) {
    }

    private static final class State {
        final Config cfg;
        final Client client;
        final String runId;
        final Map<String, Attempt> attempts = new ConcurrentHashMap<>();
        final Map<String, Integer> counts = new ConcurrentHashMap<>();

        State(final Config cfg, final Client client, final String runId) {
            this.cfg = cfg;
            this.client = client;
            this.runId = runId;
        }
    }

    static Trace currentTrace() {
        final Attempt a = CURRENT.get();
        return a == null ? null : a.trace();
    }

    private static State state() {
        State s = state;
        if (s == null) {
            synchronized (INIT) {
                s = state;
                if (s == null) {
                    s = create(System.getenv(), Path.of("").toAbsolutePath(), System.err);
                    state = s;
                }
            }
        }
        return s.client == null ? null : s;
    }

    private static State create(final Map<String, String> env, final Path cwd, final PrintStream err) {
        final String dir = PropertiesUtils.loadAllureProperties().getProperty("allure.results.directory", "allure-results");
        final Config cfg = Config.resolve(env, cwd, cwd.resolve(dir));
        if (!cfg.enabled) {
            if (!cfg.reason.equals("disabled") && !cfg.quiet) {
                err.println("[fastproof] reporting is off: " + cfg.reason);
            }
            return new State(cfg, null, "");
        }
        final byte[] rnd = new byte[6];
        new java.security.SecureRandom().nextBytes(rnd);
        final Client client = new Client(cfg, err);
        client.start();
        final State s = new State(cfg, client, HexFormat.of().formatHex(rnd));
        Runtime.getRuntime().addShutdownHook(new Thread(() -> finish(s, err), "fastproof-close"));
        return s;
    }

    private static void finish(final State s, final PrintStream err) {
        final Client.Summary sum = s.client.close("finished");
        if (sum.spooled() > 0) {
            err.println("[fastproof] " + sum.sent() + " results sent, " + sum.spooled() + " saved to " + sum.spoolDir()
                    + (sum.offlineReason().isEmpty() ? "" : " (" + sum.offlineReason() + ")"));
            err.println("[fastproof] the next run of a FastProof reporter sends them, or now: npx fastproof-playwright flush --spool "
                    + s.cfg.spoolDir + " (or python -m pytest_fastproof flush --spool ...)");
        } else if (!s.cfg.quiet) {
            err.println("[fastproof] " + (sum.launchNumber() == null ? "launch" : "launch #" + sum.launchNumber()) + ": "
                    + sum.sent() + " results, " + sum.attachments() + " attachments sent");
        }
        if (sum.lost() > 0) {
            err.println("[fastproof] " + sum.lost() + " results could not be reported");
        }
    }

    private static void guard(final State s, final Runnable r) {
        try {
            r.run();
        } catch (RuntimeException | LinkageError e) {
            s.client.warnOnce("hook:" + e.getClass().getSimpleName(), "reporter error (ignored): " + e);
        }
    }

    private static String key(final TestResult r) {
        final List<String> ps = new ArrayList<>();
        for (final Parameter p : r.getParameters()) {
            if (!Boolean.TRUE.equals(p.getExcluded())) {
                ps.add(p.getName() + "=" + p.getValue());
            }
        }
        ps.sort(Comparator.naturalOrder());
        return r.getFullName() + "\0" + r.getTestCaseId() + "\0" + String.join("&", ps);
    }

    @Override
    public void afterTestStart(final TestResult result) {
        final State s = state();
        if (s == null) {
            return;
        }
        guard(s, () -> begin(s, result));
    }

    private static void begin(final State s, final TestResult r) {
        final int retry = s.counts.merge(key(r), 1, Integer::sum) - 1;
        final Trace trace = new Trace(String.valueOf(s.cfg.launch.get("externalId")), String.valueOf(r.getFullName()), retry);
        final long start = r.getStart() != null ? r.getStart() : System.currentTimeMillis();
        final Attempt a = new Attempt(r.getUuid(), "jv-" + s.runId + "-" + r.getUuid(), retry, trace, start);
        s.attempts.put(r.getUuid(), a);
        CURRENT.set(a);
        if (s.cfg.traceLabel) {
            final Label l = new Label().setName("traceparent").setValue(trace.traceparent());
            r.getLabels().add(l);
        }
        final Mapper m = new Mapper(s.client.masker, s.cfg.resultsDir);
        final Map<String, Object> line = new LinkedHashMap<>(m.identity(r));
        line.put("id", a.id());
        line.put("start", start);
        line.put("retry", retry);
        s.client.openRunning(line);
    }

    @Override
    public void afterStepStop(final StepResult step) {
        final State s = state();
        final Attempt a = CURRENT.get();
        if (s == null || a == null || step.getName() == null) {
            return;
        }
        guard(s, () -> {
            final boolean failed = step.getStatus() != null && !step.getStatus().toString().equalsIgnoreCase("passed");
            final String ms = step.getStart() != null && step.getStop() != null ? " (" + (step.getStop() - step.getStart()) + " ms)" : "";
            final String msg = failed ? "✘ " + step.getName() : "✔ " + step.getName() + ms;
            s.client.log(a.id(), a.retry(), failed ? "error" : "info", "step", msg, step.getName());
        });
    }

    @Override
    public void afterTestStop(final TestResult result) {
        CURRENT.remove();
    }

    @Override
    public void afterTestWrite(final TestResult result) {
        final State s = state();
        if (s == null) {
            return;
        }
        guard(s, () -> written(s, result));
    }

    private static void written(final State s, final TestResult r) {
        Attempt a = s.attempts.remove(r.getUuid());
        if (a == null) {
            final int retry = s.counts.merge(key(r), 1, Integer::sum) - 1;
            a = new Attempt(r.getUuid(), "jv-" + s.runId + "-" + r.getUuid(), retry,
                    new Trace(String.valueOf(s.cfg.launch.get("externalId")), String.valueOf(r.getFullName()), retry), System.currentTimeMillis());
        }
        final Mapper m = new Mapper(s.client.masker, s.cfg.resultsDir);
        final Map<String, Object> result = m.result(r, a.id(), a.retry(), a.trace().traceparent(), a.start());
        final String st = String.valueOf(result.get("status"));
        s.client.finish(result, m.files, st.equals("passed") || st.equals("skipped"));
    }
}
