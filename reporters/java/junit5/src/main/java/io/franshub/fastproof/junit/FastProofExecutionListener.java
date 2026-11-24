package io.franshub.fastproof.junit;

import io.franshub.fastproof.Attempt;
import io.franshub.fastproof.Reporter;
import io.franshub.fastproof.TestDescriptor;

import java.util.Map;
import java.util.Optional;
import java.util.Set;
import java.util.TreeSet;
import java.util.concurrent.ConcurrentHashMap;

import org.junit.platform.engine.TestExecutionResult;
import org.junit.platform.engine.reporting.ReportEntry;
import org.junit.platform.engine.support.descriptor.MethodSource;
import org.junit.platform.launcher.TestExecutionListener;
import org.junit.platform.launcher.TestIdentifier;
import org.junit.platform.launcher.TestPlan;

/**
 * Sends every JUnit Platform test to FastProof: a {@code running} line at the start, live output, logs and steps, and
 * the final result at the end. Registered through the Platform's {@link java.util.ServiceLoader}, so a test project
 * only needs the dependency. Reporting problems never reach the tests.
 */
public final class FastProofExecutionListener implements TestExecutionListener {
    private final Map<String, Attempt> running = new ConcurrentHashMap<>();
    private volatile TestPlan plan;

    @Override
    public void testPlanExecutionStarted(final TestPlan testPlan) {
        this.plan = testPlan;
    }

    @Override
    public void executionStarted(final TestIdentifier id) {
        if (!id.isTest()) {
            return;
        }
        final Reporter r = Reporter.get();
        if (r == null) {
            return;
        }
        guard(r, () -> {
            final TestDescriptor d = describe(id);
            running.put(id.getUniqueId(), r.begin(d, r.nextRetry(d.key())));
        });
    }

    @Override
    public void reportingEntryPublished(final TestIdentifier id, final ReportEntry entry) {
        final Attempt a = running.get(id.getUniqueId());
        if (a != null) {
            a.log("info", entry.toString());
        }
    }

    @Override
    public void executionSkipped(final TestIdentifier id, final String reason) {
        final Reporter r = Reporter.get();
        if (r == null) {
            return;
        }
        guard(r, () -> {
            final TestPlan p = plan;
            if (id.isTest()) {
                skip(r, id, reason);
            } else if (p != null) {
                boolean any = false;
                for (final TestIdentifier t : p.getDescendants(id)) {
                    if (t.isTest()) {
                        skip(r, t, reason);
                        any = true;
                    }
                }
                if (!any && id.getSource().isPresent() && id.getSource().get() instanceof MethodSource) {
                    skip(r, id, reason);
                }
            }
        });
    }

    private void skip(final Reporter r, final TestIdentifier id, final String reason) {
        final TestDescriptor d = describe(id);
        r.record(d, r.nextRetry(d.key()), "skipped", reason);
    }

    @Override
    public void executionFinished(final TestIdentifier id, final TestExecutionResult result) {
        final Reporter r = Reporter.get();
        if (r == null) {
            return;
        }
        if (id.isTest()) {
            final Attempt a = running.remove(id.getUniqueId());
            if (a != null) {
                guard(r, () -> end(a, result));
            }
        } else if (result.getStatus() == TestExecutionResult.Status.FAILED) {
            guard(r, () -> {
                final TestDescriptor d = describe(id);
                final Throwable t = result.getThrowable().orElse(null);
                r.record(d, r.nextRetry(d.key()), t == null ? "broken" : Attempt.statusOf(t), t);
            });
        }
    }

    private static void end(final Attempt a, final TestExecutionResult result) {
        final Throwable t = result.getThrowable().orElse(null);
        switch (result.getStatus()) {
            case SUCCESSFUL -> a.finish("passed", (Throwable) null);
            case ABORTED -> a.finish("skipped", t == null ? null : t.getMessage());
            default -> a.finish(t == null ? "broken" : Attempt.statusOf(t), t);
        }
    }

    private TestDescriptor describe(final TestIdentifier id) {
        final Set<String> tags = new TreeSet<>();
        id.getTags().forEach(t -> tags.add(t.getName()));
        final Optional<org.junit.platform.engine.TestSource> source = id.getSource();
        final TestPlan p = plan;
        final TestIdentifier parent = p == null ? null : p.getParent(id).orElse(null);
        final boolean invocation = parent != null && sameMethod(parent.getSource(), source);
        String name = id.getDisplayName();
        String fullName = name;
        String className = null;
        String method = null;
        if (source.isPresent() && source.get() instanceof MethodSource m) {
            className = m.getClassName();
            method = m.getMethodName();
            fullName = Names.fullName(m);
            if (invocation) {
                name = parent.getDisplayName() + " " + name;
            }
        } else if (source.isPresent()) {
            className = Names.className(source.get());
            fullName = (className == null ? "" : Names.readable(className) + " › ") + name;
        }
        final TestDescriptor d = new TestDescriptor("ju", id.getUniqueId(), name).fullName(fullName);
        if (invocation) {
            d.parameter("invocation", id.getDisplayName());
        } else if (source.isEmpty() || !(source.get() instanceof MethodSource)) {
            d.parameter("test", id.getUniqueId());
        }
        Names.labels(d, className, method, tags);
        return d;
    }

    private static boolean sameMethod(final Optional<org.junit.platform.engine.TestSource> a, final Optional<org.junit.platform.engine.TestSource> b) {
        return a.isPresent() && b.isPresent() && a.get() instanceof MethodSource x && b.get() instanceof MethodSource y
                && x.getClassName().equals(y.getClassName()) && x.getMethodName().equals(y.getMethodName());
    }

    private static void guard(final Reporter r, final Runnable body) {
        try {
            body.run();
        } catch (RuntimeException | LinkageError e) {
            r.warnOnce("hook:" + e.getClass().getSimpleName(), "reporter error (ignored): " + e);
        }
    }
}
