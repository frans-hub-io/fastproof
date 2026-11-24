package io.franshub.fastproof;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Collection;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

/** An Allure-format test plan ({@code {"version": "1.0", "tests": [{"id": "...", "selector": "..."}]}}). */
public final class TestPlan {
    private final Set<String> ids = new HashSet<>();
    private final Set<String> selectors = new HashSet<>();

    private TestPlan() {
    }

    /** Reads a plan; {@code null} when the file cannot be read or is not a plan (a plan problem never stops the run). */
    public static TestPlan load(final Path file) {
        try {
            final Object parsed = Json.parse(Files.readString(file, StandardCharsets.UTF_8));
            if (!(parsed instanceof Map) || !(Json.object(parsed).get("tests") instanceof java.util.List)) {
                return null;
            }
            final TestPlan plan = new TestPlan();
            for (final Object t : Json.array(Json.object(parsed).get("tests"))) {
                final Map<String, Object> m = Json.object(t);
                if (m.get("id") != null) {
                    plan.ids.add(String.valueOf(m.get("id")));
                }
                if (m.get("selector") instanceof String s && !s.isBlank()) {
                    plan.selectors.add(s.strip());
                }
            }
            return plan;
        } catch (IOException | RuntimeException e) {
            return null;
        }
    }

    /** Names a plan may use for a method: Allure style ({@code pkg.Cls#method}) and dotted, with and without parameter types. */
    public static List<String> candidates(final String className, final String method, final String types) {
        final String t = types == null ? "" : types.replace(" ", "");
        final List<String> out = new ArrayList<>();
        for (final String c : new String[] {className, className.replace('$', '.')}) {
            out.add(c + "#" + method);
            out.add(c + "." + method);
            out.add(c + "#" + method + "(" + t + ")");
            out.add(c + "." + method + "(" + t + ")");
        }
        return out;
    }

    /** Test case ids carried by tags or groups: {@code fastproof.id:X} and {@code allure.id:X}. */
    public static List<String> ids(final Collection<String> tags) {
        final List<String> out = new ArrayList<>();
        for (final String t : tags) {
            for (final String prefix : TestDescriptor.ID_PREFIXES) {
                if (t.startsWith(prefix)) {
                    out.add(t.substring(prefix.length()));
                }
            }
        }
        return out;
    }

    public int size() {
        return ids.size() + selectors.size();
    }

    /** True when one of the explicit ids or one of the name candidates is selected. */
    public boolean matches(final Collection<String> candidates, final Collection<String> explicitIds) {
        for (final String id : explicitIds) {
            if (ids.contains(id)) {
                return true;
            }
        }
        for (final String c : candidates) {
            if (selectors.contains(c)) {
                return true;
            }
        }
        return false;
    }
}
