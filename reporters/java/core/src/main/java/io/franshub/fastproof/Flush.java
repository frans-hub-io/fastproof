package io.franshub.fastproof;

import java.nio.file.Path;
import java.util.Map;

/** {@code java -jar fastproof-core.jar flush|list [--spool DIR]}: sends or shows what earlier runs left in the spool. */
public final class Flush {
    private Flush() {
    }

    public static void main(final String[] args) {
        System.exit(run(args, System.getenv(), System.out, System.err));
    }

    static int run(final String[] args, final Map<String, String> env, final java.io.PrintStream out, final java.io.PrintStream err) {
        if (args.length == 0 || !(args[0].equals("flush") || args[0].equals("list"))) {
            err.println("usage: fastproof-core.jar flush|list [--spool DIR] [--timeout SECONDS]");
            return 2;
        }
        String spool = null;
        long timeout = 600;
        for (int i = 1; i < args.length; i++) {
            if (args[i].equals("--spool") && i + 1 < args.length) {
                spool = args[++i];
            } else if (args[i].equals("--timeout") && i + 1 < args.length) {
                timeout = Long.parseLong(args[++i]);
            }
        }
        final Map<String, String> merged = new java.util.HashMap<>(env);
        if (spool != null) {
            merged.put("FASTPROOF_SPOOL_DIR", spool);
        }
        final Config cfg = Config.resolve(merged, Path.of("").toAbsolutePath());
        if (args[0].equals("list")) {
            for (final SpoolFlush.Session s : SpoolFlush.list(cfg.spoolDir, null)) {
                final Map<String, Object> launch = Json.object(s.meta().get("launch"));
                out.println(s.id() + "\t" + s.meta().get("project") + "\t" + launch.get("externalId") + "\t"
                        + (Boolean.TRUE.equals(s.meta().get("ended")) ? "ended" : "interrupted"));
            }
            return 0;
        }
        if (cfg.url.isEmpty()) {
            err.println("FASTPROOF_URL is required");
            return 2;
        }
        final SpoolFlush.Result r = new SpoolFlush(new Http(cfg), cfg).run(cfg.spoolDir, null, System.currentTimeMillis() + timeout * 1000, cfg.project);
        out.println("sent " + r.results() + " results from " + r.sessions() + " sessions; " + r.remaining() + " left, " + r.failed() + " rejected");
        for (final String e : r.errors().subList(0, Math.min(5, r.errors().size()))) {
            err.println("  " + e);
        }
        return r.remaining() > 0 ? 1 : 0;
    }
}
