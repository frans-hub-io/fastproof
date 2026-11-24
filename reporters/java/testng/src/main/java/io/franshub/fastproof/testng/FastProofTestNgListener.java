package io.franshub.fastproof.testng;

import io.franshub.fastproof.Attempt;
import io.franshub.fastproof.Reporter;
import io.franshub.fastproof.TestDescriptor;
import io.franshub.fastproof.TestPlan;

import java.lang.reflect.Method;
import java.lang.reflect.Parameter;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;
import java.util.Map;
import java.util.WeakHashMap;
import java.util.concurrent.ConcurrentHashMap;

import org.testng.IConfigurationListener;
import org.testng.IMethodInstance;
import org.testng.IMethodInterceptor;
import org.testng.ISuite;
import org.testng.ISuiteListener;
import org.testng.ITestContext;
import org.testng.ITestListener;
import org.testng.ITestNGMethod;
import org.testng.ITestResult;
import org.testng.SkipException;

/**
 * Sends every TestNG test to FastProof: a {@code running} line at the start, live output, logs and steps, the final
 * result at the end, retries ({@code IRetryAnalyzer}) as attempts of one result, {@code invocationCount} and data
 * provider rows as separate results. Registered through TestNG's {@link java.util.ServiceLoader}, so a test project
 * only needs the dependency. Reporting problems never reach the tests.
 */
public final class FastProofTestNgListener implements ITestListener, IConfigurationListener, IMethodInterceptor, ISuiteListener {
    private static final String ATTEMPT = "fastproof.attempt";

    private static final class Slot {
        int invocation;
        int attempt;
        boolean started;
        boolean retried;
    }

    private static final Map<Reporter, Map<String, Slot>> SLOTS = Collections.synchronizedMap(new WeakHashMap<>());

    @Override
    public void onStart(final ISuite suite) {
        try {
            Reporter.get();
        } catch (RuntimeException | LinkageError e) {
            System.err.println("[fastproof] reporter error (ignored): " + e);
        }
    }

    @Override
    public void onFinish(final ISuite suite) {
        try {
            final Reporter r = Reporter.get();
            if (r != null) {
                r.drain();
            }
        } catch (RuntimeException | LinkageError e) {
            System.err.println("[fastproof] reporter error (ignored): " + e);
        }
    }

    @Override
    public List<IMethodInstance> intercept(final List<IMethodInstance> methods, final ITestContext context) {
        try {
            final Reporter r = Reporter.get();
            if (r == null || !r.hasPlan()) {
                return methods;
            }
            final List<IMethodInstance> kept = new ArrayList<>();
            for (final IMethodInstance m : methods) {
                final ITestNGMethod tm = m.getMethod();
                final String cls = tm.getRealClass().getName();
                final String types = types(tm);
                final List<String> groups = Arrays.asList(tm.getGroups());
                if (r.selected(TestPlan.candidates(cls, tm.getMethodName(), types), TestPlan.ids(groups))) {
                    kept.add(m);
                }
            }
            return kept;
        } catch (RuntimeException | LinkageError e) {
            return methods;
        }
    }

    // ---- test events ------------------------------------------------------------------------------------

    @Override
    public void onTestStart(final ITestResult result) {
        final Reporter r = Reporter.get();
        if (r == null) {
            return;
        }
        guard(r, () -> {
            final Slot slot = slot(r, result);
            final boolean repeated = result.getMethod().getInvocationCount() > 1;
            synchronized (slot) {
                if (!slot.started) {
                    slot.invocation = 0;
                    slot.attempt = 0;
                } else if (slot.retried) {
                    slot.attempt++;
                } else if (repeated) {
                    slot.invocation++;
                    slot.attempt = 0;
                } else {
                    slot.attempt++;
                }
                slot.started = true;
                slot.retried = false;
                result.setAttribute(ATTEMPT, r.begin(describe(result, repeated ? slot.invocation : -1), slot.attempt));
            }
        });
    }

    @Override
    public void onTestSuccess(final ITestResult result) {
        finish(result, "passed", null);
    }

    @Override
    public void onTestFailure(final ITestResult result) {
        finish(result, null, result.getThrowable());
    }

    @Override
    public void onTestFailedButWithinSuccessPercentage(final ITestResult result) {
        finish(result, null, result.getThrowable());
    }

    @Override
    public void onTestFailedWithTimeout(final ITestResult result) {
        finish(result, null, result.getThrowable());
    }

    @Override
    public void onTestSkipped(final ITestResult result) {
        final Throwable t = result.getThrowable();
        final boolean retried = wasRetried(result);
        if (retried) {
            final Reporter rep = Reporter.get();
            final Slot s = rep == null ? null : SLOTS.getOrDefault(rep, Map.of()).get(baseKey(result));
            if (s != null) {
                s.retried = true;
            }
            finish(result, null, t);
        } else {
            finish(result, "skipped", t);
        }
    }

    private void finish(final ITestResult result, final String status, final Throwable t) {
        final Reporter r = Reporter.get();
        if (r == null) {
            return;
        }
        guard(r, () -> {
            final boolean skipped = "skipped".equals(status);
            final Throwable cause = skipped || t instanceof SkipException ? null : t;
            final String st = status != null ? status : cause == null ? "failed" : Attempt.statusOf(cause);
            final Object o = result.getAttribute(ATTEMPT);
            if (o instanceof Attempt a) {
                result.removeAttribute(ATTEMPT);
                try {
                    for (final String line : org.testng.Reporter.getOutput(result)) {
                        a.log("info", line);
                    }
                } catch (RuntimeException e) {
                    // the TestNG report log is optional
                }
                if (skipped) {
                    a.finish("skipped", t == null ? null : t.getMessage());
                } else {
                    a.finish(st, cause);
                }
                return;
            }
            final TestDescriptor d = describe(result, -1);
            final Slot s = slot(r, result);
            final int attempt;
            synchronized (s) {
                if (s.started) {
                    s.attempt++;
                }
                s.started = true;
                attempt = s.attempt;
            }
            if (skipped) {
                r.record(d, attempt, "skipped", t == null ? null : t.getMessage());
            } else {
                r.record(d, attempt, st, cause);
            }
        });
    }

    @Override
    public void onConfigurationFailure(final ITestResult result) {
        final Reporter r = Reporter.get();
        if (r == null) {
            return;
        }
        guard(r, () -> {
            final ITestNGMethod m = result.getMethod();
            final TestDescriptor d = new TestDescriptor("tn", "config:" + m.getRealClass().getName() + "#" + m.getMethodName(),
                    m.getMethodName() + " (configuration)").fullName(m.getRealClass().getName() + "#" + m.getMethodName() + "(configuration)");
            labels(d, result, m);
            final Throwable t = result.getThrowable();
            r.record(d, r.nextRetry(d.key()), t == null ? "broken" : Attempt.statusOf(t), t);
        });
    }

    // ---- description ------------------------------------------------------------------------------------

    private static String types(final ITestNGMethod m) {
        final Method method = m.getConstructorOrMethod().getMethod();
        if (method == null) {
            return "";
        }
        final StringBuilder sb = new StringBuilder();
        for (final Class<?> c : method.getParameterTypes()) {
            if (sb.length() > 0) {
                sb.append(',');
            }
            sb.append(c.getName());
        }
        return sb.toString();
    }

    private static String baseKey(final ITestResult result) {
        final ITestNGMethod m = result.getMethod();
        return m.getRealClass().getName() + "#" + m.getMethodName() + "(" + types(m) + ")|" + values(result.getParameters());
    }

    private static String values(final Object[] params) {
        if (params == null || params.length == 0) {
            return "";
        }
        return Arrays.deepToString(params);
    }

    private static void labels(final TestDescriptor d, final ITestResult result, final ITestNGMethod m) {
        final String cls = m.getRealClass().getName();
        final int dot = cls.lastIndexOf('.');
        final String pkg = dot < 0 ? "" : cls.substring(0, dot);
        final String[] chain = cls.substring(dot + 1).split("\\$");
        d.label("framework", "testng").label("language", "java");
        if (!pkg.isEmpty()) {
            d.label("package", pkg);
        }
        d.label("testClass", String.join(".", chain)).label("suite", chain[0]);
        if (chain.length > 1) {
            d.label("subSuite", String.join(" › ", Arrays.copyOfRange(chain, 1, chain.length)));
        }
        if (result.getTestContext() != null) {
            if (result.getTestContext().getSuite() != null && result.getTestContext().getSuite().getName() != null) {
                d.label("parentSuite", result.getTestContext().getSuite().getName());
            }
            d.label("testng.test", result.getTestContext().getName());
        }
        d.label("testMethod", m.getMethodName());
        d.tags(Arrays.asList(m.getGroups()));
        d.source(cls.split("\\$")[0].replace('.', '/') + ".java", null);
    }

    private static Slot slot(final Reporter r, final ITestResult result) {
        return SLOTS.computeIfAbsent(r, k -> new ConcurrentHashMap<>()).computeIfAbsent(baseKey(result), k -> new Slot());
    }

    private TestDescriptor describe(final ITestResult result, final int invocation) {
        final ITestNGMethod m = result.getMethod();
        final String cls = m.getRealClass().getName();
        final String name = result.getTestName() != null && !result.getTestName().isBlank() ? result.getTestName() : m.getMethodName();
        final String key = baseKey(result) + (invocation >= 0 ? "|" + invocation : "");
        final TestDescriptor d = new TestDescriptor("tn", key, name)
                .fullName(cls.replace('$', '.') + "#" + m.getMethodName() + "(" + types(m) + ")");
        final Method method = m.getConstructorOrMethod().getMethod();
        final Object[] params = result.getParameters();
        if (params != null) {
            final Parameter[] names = method == null ? new Parameter[0] : method.getParameters();
            for (int i = 0; i < params.length; i++) {
                final Object v = params[i];
                d.parameter(i < names.length ? names[i].getName() : "arg" + i,
                        v instanceof Object[] arr ? Arrays.deepToString(arr) : String.valueOf(v));
            }
        }
        if (invocation >= 0) {
            d.parameter("invocation", String.valueOf(invocation + 1));
        }
        if (m.getDescription() != null && !m.getDescription().isBlank()) {
            d.description(m.getDescription());
        }
        labels(d, result, m);
        return d;
    }

    private static boolean wasRetried(final ITestResult result) {
        try {
            return result.wasRetried();
        } catch (NoSuchMethodError e) {
            return false;
        }
    }

    private static void guard(final Reporter r, final Runnable body) {
        try {
            body.run();
        } catch (RuntimeException | LinkageError e) {
            r.warnOnce("hook:" + e.getClass().getSimpleName(), "reporter error (ignored): " + e);
        }
    }
}
