package io.franshub.fastproof;

import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.security.SecureRandom;
import java.util.Collection;
import java.util.HexFormat;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;

/**
 * The reporter of one test process: owns the sender, the spool and the run id. Framework adapters call
 * {@link #begin} when an attempt starts and {@link Attempt#finish} when it ends; nothing here throws into a test.
 */
public final class Reporter {
    private static final Object INIT = new Object();
    private static volatile Reporter instance;
    private static volatile boolean initialized;

    private final Config cfg;
    private final Client client;
    private final PrintStream err;
    private final String runId;
    private final TestPlan plan;
    private final Map<String, Integer> counts = new ConcurrentHashMap<>();
    private volatile boolean closed;
    private volatile int lastPrinted = -1;

    private Reporter(final Config cfg, final Client client, final PrintStream err, final String runId, final TestPlan plan) {
        this.cfg = cfg;
        this.client = client;
        this.err = err;
        this.runId = runId;
        this.plan = plan;
    }

    /** The reporter of this process, created from {@code FASTPROOF_*} variables and {@code fastproof.*} properties; {@code null} when reporting is off. */
    public static Reporter get() {
        if (!initialized) {
            synchronized (INIT) {
                if (!initialized) {
                    try {
                        instance = start(System.getenv(), Path.of("").toAbsolutePath(), System.err, true);
                    } catch (RuntimeException | LinkageError e) {
                        System.err.println("[fastproof] reporting is off: " + e);
                    }
                    initialized = true;
                }
            }
        }
        return instance;
    }

    /** Replaces the process reporter (none for {@code null}); for tests of adapters. */
    public static void install(final Reporter r) {
        synchronized (INIT) {
            instance = r;
            initialized = true;
        }
    }

    /** Creates a reporter; {@code null} when it is off (not configured or disabled). */
    public static Reporter start(final Map<String, String> env, final Path cwd, final PrintStream err, final boolean shutdownHook) {
        final Config cfg = Config.resolve(env, cwd);
        if (!cfg.enabled) {
            if (!cfg.reason.equals("disabled") && !cfg.quiet) {
                err.println("[fastproof] reporting is off: " + cfg.reason);
            }
            return null;
        }
        final byte[] rnd = new byte[6];
        new SecureRandom().nextBytes(rnd);
        final Client client = new Client(cfg, err);
        client.start();
        TestPlan plan = null;
        if (cfg.testPlan != null) {
            plan = TestPlan.load(cfg.testPlan);
            if (plan == null) {
                client.warnOnce("plan", "the test plan " + cfg.testPlan + " cannot be read; running everything");
            }
        }
        final Reporter r = new Reporter(cfg, client, err, HexFormat.of().formatHex(rnd), plan);
        LogCapture.install(cfg);
        if (shutdownHook) {
            Runtime.getRuntime().addShutdownHook(new Thread(r::close, "fastproof-close"));
        }
        return r;
    }

    /** A warning printed once per key; the message is masked. */
    public void warnOnce(final String key, final String message) {
        client.warnOnce(key, message);
    }

    public String runId() {
        return runId;
    }

    public String launchExternalId() {
        return String.valueOf(cfg.launch.get("externalId"));
    }

    /** Fails open: without a plan, or when the plan could not be read, everything is selected. */
    public boolean selected(final Collection<String> candidates, final Collection<String> ids) {
        return plan == null || plan.matches(candidates, ids);
    }

    public boolean hasPlan() {
        return plan != null;
    }

    /** Zero-based attempt number for a test key: 0 the first time the key is seen in this run, then 1, 2... */
    public int nextRetry(final String key) {
        return counts.merge(key, 1, Integer::sum) - 1;
    }

    /** Starts an attempt: sends the {@code running} line and makes the attempt current on this thread. */
    public Attempt begin(final TestDescriptor d, final int retry) {
        final Attempt a = create(d, retry, System.currentTimeMillis());
        a.open();
        return a.enter();
    }

    private Attempt create(final TestDescriptor d, final int retry, final long start) {
        final Trace trace = new Trace(launchExternalId(), d.fullName != null ? d.fullName : d.name, retry);
        final String id = d.idPrefix + "-" + runId + "-" + sha1(d.key).substring(0, 16) + "-" + retry;
        return new Attempt(this, client, d, id, retry, trace, start);
    }

    /** Reports an attempt that never ran (skipped, or a failure of a class or suite setup) in one step. */
    public void record(final TestDescriptor d, final int retry, final String status, final String message) {
        final Attempt a = create(d, retry, System.currentTimeMillis());
        a.finish(status, message);
    }

    /** Reports an attempt that never ran, with the exception that stopped it. */
    public void record(final TestDescriptor d, final int retry, final String status, final Throwable error) {
        final Attempt a = create(d, retry, System.currentTimeMillis());
        a.finish(status, error);
    }

    /** Sends what is queued and prints a progress line, for the end of a test session that another may follow. */
    public void drain() {
        client.drain(cfg.flushTimeoutMs);
        final Client.Summary sum = client.progress();
        if (!cfg.quiet && !closed && sum.sent() != lastPrinted) {
            lastPrinted = sum.sent();
            err.println("[fastproof] " + (sum.launchNumber() == null ? "launch" : "launch #" + sum.launchNumber()) + ": "
                    + sum.sent() + " results, " + sum.attachments() + " attachments sent");
        }
    }

    /** Ends the run: sends or spools what is left, finishes an own launch, prints one summary line. Idempotent. */
    public void close() {
        synchronized (this) {
            if (closed) {
                return;
            }
            closed = true;
        }
        final Client.Summary sum = client.close("finished");
        if (sum.spooled() > 0) {
            err.println("[fastproof] " + sum.sent() + " results sent, " + sum.spooled() + " saved to " + sum.spoolDir()
                    + (sum.offlineReason().isEmpty() ? "" : " (" + sum.offlineReason() + ")"));
            err.println("[fastproof] the next run sends them, or now: java -jar fastproof-core.jar flush --spool " + cfg.spoolDir);
        } else if (!cfg.quiet && sum.sent() != lastPrinted) {
            err.println("[fastproof] " + (sum.launchNumber() == null ? "launch" : "launch #" + sum.launchNumber()) + ": "
                    + sum.sent() + " results, " + sum.attachments() + " attachments sent");
        }
        if (sum.lost() > 0) {
            err.println("[fastproof] " + sum.lost() + " results could not be reported");
        }
    }

    Client client() {
        return client;
    }

    private static String sha1(final String s) {
        try {
            return HexFormat.of().formatHex(MessageDigest.getInstance("SHA-1").digest(s.getBytes(StandardCharsets.UTF_8)));
        } catch (NoSuchAlgorithmException e) {
            throw new IllegalStateException(e);
        }
    }
}
