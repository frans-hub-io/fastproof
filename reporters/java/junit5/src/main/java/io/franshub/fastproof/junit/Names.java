package io.franshub.fastproof.junit;

import io.franshub.fastproof.TestDescriptor;
import io.franshub.fastproof.TestPlan;

import java.util.List;
import java.util.Set;

import org.junit.platform.engine.TestSource;
import org.junit.platform.engine.support.descriptor.ClassSource;
import org.junit.platform.engine.support.descriptor.MethodSource;

/** Names, labels and selector candidates of a JUnit Platform test. */
final class Names {
    private Names() {
    }

    static String readable(final String binaryName) {
        return binaryName.replace('$', '.');
    }

    static String types(final MethodSource m) {
        final String t = m.getMethodParameterTypes();
        return t == null ? "" : t.replace(" ", "");
    }

    static String fullName(final MethodSource m) {
        return readable(m.getClassName()) + "#" + m.getMethodName() + "(" + types(m) + ")";
    }

    static String className(final TestSource source) {
        if (source instanceof MethodSource m) {
            return m.getClassName();
        }
        if (source instanceof ClassSource c) {
            return c.getClassName();
        }
        return null;
    }

    static List<String> candidates(final MethodSource m) {
        return TestPlan.candidates(m.getClassName(), m.getMethodName(), types(m));
    }

    static List<String> ids(final Set<String> tags) {
        return TestPlan.ids(tags);
    }

    /** Adds package, class, suite and tag labels. */
    static void labels(final TestDescriptor d, final String className, final String method, final Set<String> tags) {
        d.label("framework", "junit5").label("language", "java");
        if (className != null) {
            final int dot = className.lastIndexOf('.');
            final String pkg = dot < 0 ? "" : className.substring(0, dot);
            final String simple = className.substring(dot + 1);
            final String[] chain = simple.split("\\$");
            if (!pkg.isEmpty()) {
                d.label("package", pkg);
            }
            d.label("testClass", String.join(".", chain));
            d.label("suite", chain[0]);
            if (chain.length > 1) {
                d.label("subSuite", String.join(" › ", java.util.Arrays.copyOfRange(chain, 1, chain.length)));
            }
            d.source(className.split("\\$")[0].replace('.', '/') + ".java", null);
        }
        if (method != null) {
            d.label("testMethod", method);
        }
        d.tags(tags);
    }
}
