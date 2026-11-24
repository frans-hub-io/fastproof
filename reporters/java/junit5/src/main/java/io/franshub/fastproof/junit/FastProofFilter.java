package io.franshub.fastproof.junit;

import io.franshub.fastproof.Reporter;

import java.util.Set;
import java.util.TreeSet;

import org.junit.platform.engine.FilterResult;
import org.junit.platform.engine.TestDescriptor;
import org.junit.platform.engine.support.descriptor.MethodSource;
import org.junit.platform.launcher.PostDiscoveryFilter;

/** Runs only the tests a {@code testplan.json} (Allure format) selects; no plan, or a plan that cannot be read, selects everything. */
public final class FastProofFilter implements PostDiscoveryFilter {
    @Override
    public FilterResult apply(final TestDescriptor descriptor) {
        final boolean method = descriptor.getSource().isPresent() && descriptor.getSource().get() instanceof MethodSource;
        if (!descriptor.isTest() && !method) {
            return FilterResult.included("a container");
        }
        try {
            final Reporter r = Reporter.get();
            if (r == null || !r.hasPlan()) {
                return FilterResult.included("no test plan");
            }
            final Set<String> tags = new TreeSet<>();
            descriptor.getTags().forEach(t -> tags.add(t.getName()));
            final var source = descriptor.getSource();
            final boolean in;
            if (source.isPresent() && source.get() instanceof MethodSource m) {
                in = r.selected(Names.candidates(m), Names.ids(tags));
            } else {
                in = r.selected(java.util.List.of(descriptor.getDisplayName()), Names.ids(tags));
            }
            return in ? FilterResult.included("in the test plan") : FilterResult.excluded("not in the test plan");
        } catch (RuntimeException | LinkageError e) {
            return FilterResult.included("the test plan cannot be applied");
        }
    }
}
