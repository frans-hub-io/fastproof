package io.franshub.fastproof;

import java.net.URLEncoder;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.TreeSet;
import java.util.regex.Pattern;

/** Replaces secrets with {@code ****} before anything leaves the process. */
final class Masker {
    static final String MASK = "****";
    static final List<String> DEFAULT_ENV = List.of(
            "FASTPROOF_TOKEN", "*TOKEN*", "*SECRET*", "*PASSWORD*", "*PASSWD*", "*_KEY", "*APIKEY*", "*API_KEY*", "*CREDENTIAL*");
    private static final Set<String> NOT_SECRET = Set.of("true", "false", "yes", "no", "on", "off", "null", "none", "undefined");

    private final Pattern literal;
    private final List<Pattern> patterns = new ArrayList<>();

    Masker(final List<String> envNames, final List<String> values, final List<String> patterns, final Map<String, String> env) {
        final Set<String> found = new TreeSet<>();
        final List<Pattern> globs = new ArrayList<>();
        for (final String g : envNames) {
            globs.add(Pattern.compile("^" + Pattern.quote(g.toUpperCase(Locale.ROOT)).replace("*", "\\E.*\\Q") + "$"));
        }
        for (final Map.Entry<String, String> e : env.entrySet()) {
            for (final Pattern g : globs) {
                if (g.matcher(e.getKey().toUpperCase(Locale.ROOT)).matches()) {
                    add(found, e.getValue());
                    break;
                }
            }
        }
        for (final String v : values) {
            add(found, v);
        }
        if (found.isEmpty()) {
            this.literal = null;
        } else {
            final List<String> ordered = new ArrayList<>(found);
            ordered.sort(Comparator.comparingInt(String::length).reversed().thenComparing(Comparator.naturalOrder()));
            final StringBuilder sb = new StringBuilder();
            for (final String v : ordered) {
                if (sb.length() > 0) {
                    sb.append('|');
                }
                sb.append(Pattern.quote(v));
            }
            this.literal = Pattern.compile(sb.toString());
        }
        for (final String p : patterns) {
            try {
                this.patterns.add(Pattern.compile(p));
            } catch (RuntimeException e) {
                // an invalid pattern is ignored
            }
        }
    }

    private static void add(final Set<String> found, final String v) {
        if (v == null) {
            return;
        }
        final String t = v.strip();
        if (t.length() < 4 || NOT_SECRET.contains(t.toLowerCase(Locale.ROOT)) || t.matches("\\d{1,6}")) {
            return;
        }
        found.add(t);
        final String enc = URLEncoder.encode(t, StandardCharsets.UTF_8).replace("+", "%20");
        if (!enc.equals(t)) {
            found.add(enc);
        }
    }

    boolean active() {
        return literal != null || !patterns.isEmpty();
    }

    String mask(final String s) {
        if (s == null || s.isEmpty()) {
            return s;
        }
        String out = s;
        if (literal != null) {
            out = literal.matcher(out).replaceAll(java.util.regex.Matcher.quoteReplacement(MASK));
        }
        for (final Pattern p : patterns) {
            out = p.matcher(out).replaceAll(java.util.regex.Matcher.quoteReplacement(MASK));
        }
        return out;
    }
}
