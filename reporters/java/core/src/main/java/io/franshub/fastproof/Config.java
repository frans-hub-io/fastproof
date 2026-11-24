package io.franshub.fastproof;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.UUID;
import java.util.regex.Pattern;

/** Settings from the environment ({@code FASTPROOF_*}) or system properties ({@code fastproof.*}, which win). */
final class Config {
    private static final Pattern PROJECT_KEY = Pattern.compile("^[a-z0-9][a-z0-9-]{1,62}$");

    boolean enabled;
    String reason = "";
    String url = "";
    String token = "";
    String project = "";
    boolean offline;
    Map<String, Object> launch = new LinkedHashMap<>();
    boolean ownLaunch;
    Path spoolDir;
    long spoolMaxBytes = 200L * 1024 * 1024;
    long flushTimeoutMs = 30_000;
    long requestTimeoutMs = 15_000;
    long batchIntervalMs = 200;
    boolean runningResults = true;
    boolean liveLogs = true;
    boolean captureOutput = true;
    String logLevel = "INFO";
    boolean logLevelSet;
    String finishLaunch = "auto";
    long maxAttachmentBytes = 512L * 1024 * 1024;
    boolean directUploads = true;
    long directUploadMinBytes = 1024L * 1024;
    boolean quiet;
    Path testPlan;
    List<String> secretEnv = new ArrayList<>(Masker.DEFAULT_ENV);
    List<String> secretValues = new ArrayList<>();
    List<String> secretPatterns = new ArrayList<>();
    Map<String, String> env;

    private static String get(final Map<String, String> env, final String name) {
        final String prop = System.getProperty(name.toLowerCase(Locale.ROOT).replace('_', '.'));
        final String v = prop != null ? prop : env.get(name);
        return v == null || v.isBlank() ? null : v.strip();
    }

    private static Boolean flag(final String v) {
        return v == null ? null : !List.of("0", "false", "no", "off").contains(v.toLowerCase(Locale.ROOT));
    }

    private static List<String> list(final String v) {
        final List<String> out = new ArrayList<>();
        if (v != null) {
            for (final String s : v.split(",")) {
                if (!s.isBlank()) {
                    out.add(s.strip());
                }
            }
        }
        return out;
    }

    private static long number(final String v, final long d) {
        try {
            return v == null ? d : Math.max(0, Long.parseLong(v));
        } catch (NumberFormatException e) {
            return d;
        }
    }

    static Config resolve(final Map<String, String> env, final Path cwd) {
        final Config c = new Config();
        c.env = env;
        String token = get(env, "FASTPROOF_TOKEN");
        final String tokenFile = get(env, "FASTPROOF_TOKEN_FILE");
        if (token == null && tokenFile != null) {
            try {
                token = Files.readString(Path.of(tokenFile), StandardCharsets.UTF_8).strip();
            } catch (java.io.IOException | RuntimeException e) {
                token = null;
            }
        }
        c.token = token == null ? "" : token;
        c.url = orEmpty(get(env, "FASTPROOF_URL"));
        c.project = orEmpty(get(env, "FASTPROOF_PROJECT"));
        final Ci ci = Ci.detect(env);
        final String explicit = get(env, "FASTPROOF_LAUNCH");
        final String external = explicit != null ? explicit : ci != null && ci.externalId != null ? ci.externalId : "local-" + UUID.randomUUID();
        c.ownLaunch = explicit == null && (ci == null || ci.externalId == null);
        String name = get(env, "FASTPROOF_LAUNCH_NAME");
        if (name == null) {
            name = ci != null && ci.name != null ? ci.name : explicit != null ? explicit : (c.project.isEmpty() ? "tests" : c.project) + " " + java.time.OffsetDateTime.now(java.time.ZoneOffset.UTC).toString().substring(0, 16).replace('T', ' ');
        }
        c.launch.put("externalId", external);
        c.launch.put("name", name);
        if (ci != null && !ci.ci.isEmpty()) {
            c.launch.put("ci", ci.ci);
        }
        if (ci != null && !ci.vcs.isEmpty()) {
            c.launch.put("vcs", ci.vcs);
        }
        final String environment = get(env, "FASTPROOF_ENVIRONMENT");
        if (environment != null) {
            final Map<String, Object> e = new LinkedHashMap<>();
            e.put("key", environment);
            e.put("values", Map.of());
            c.launch.put("environment", e);
        }
        c.secretEnv.addAll(list(get(env, "FASTPROOF_SECRET_ENV")));
        c.secretPatterns.addAll(list(get(env, "FASTPROOF_SECRET_PATTERNS")));
        if (!c.token.isEmpty()) {
            c.secretValues.add(c.token);
        }
        final boolean envOff = Boolean.FALSE.equals(flag(env.get("FASTPROOF_ENABLED")));
        c.enabled = !envOff && !Boolean.FALSE.equals(flag(get(env, "FASTPROOF_ENABLED")));
        c.offline = Boolean.TRUE.equals(flag(get(env, "FASTPROOF_OFFLINE")));
        if (!c.enabled) {
            c.reason = "disabled";
        } else if (c.url.isEmpty() && !c.offline) {
            c.enabled = false;
            c.reason = "FASTPROOF_URL is not set";
        } else if (!PROJECT_KEY.matcher(c.project).matches()) {
            c.enabled = false;
            c.reason = "FASTPROOF_PROJECT is missing or not a valid project key";
        }
        final String spool = get(env, "FASTPROOF_SPOOL_DIR");
        c.spoolDir = cwd.resolve(spool != null ? spool : ".fastproof/spool").toAbsolutePath().normalize();
        c.spoolMaxBytes = number(get(env, "FASTPROOF_SPOOL_MAX_BYTES"), c.spoolMaxBytes);
        c.flushTimeoutMs = number(get(env, "FASTPROOF_FLUSH_TIMEOUT_MS"), c.flushTimeoutMs);
        c.liveLogs = !Boolean.FALSE.equals(flag(get(env, "FASTPROOF_LIVE_LOGS")));
        c.captureOutput = !Boolean.FALSE.equals(flag(get(env, "FASTPROOF_CAPTURE_OUTPUT")));
        c.logLevelSet = get(env, "FASTPROOF_LOG_LEVEL") != null;
        c.logLevel = orDefault(get(env, "FASTPROOF_LOG_LEVEL"), "INFO").toUpperCase(Locale.ROOT);
        c.finishLaunch = orDefault(get(env, "FASTPROOF_FINISH_LAUNCH"), "auto");
        c.quiet = Boolean.TRUE.equals(flag(get(env, "FASTPROOF_QUIET")));
        c.directUploads = !"off".equalsIgnoreCase(get(env, "FASTPROOF_DIRECT_UPLOAD_MODE"));
        c.directUploadMinBytes = number(get(env, "FASTPROOF_DIRECT_UPLOAD_MIN_BYTES"), c.directUploadMinBytes);
        final String plan = orDefault(get(env, "FASTPROOF_TESTPLAN"), get(env, "ALLURE_TESTPLAN_PATH"));
        c.testPlan = plan == null || plan.isEmpty() ? null : cwd.resolve(plan);
        return c;
    }

    private static String orEmpty(final String v) {
        return v == null ? "" : v;
    }

    private static String orDefault(final String v, final String d) {
        return v == null ? d : v;
    }
}
