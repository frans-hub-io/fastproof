package io.franshub.fastproof;

import java.util.ArrayList;
import java.util.Collection;
import java.util.List;

/** What an adapter knows about a test when an attempt begins. */
public final class TestDescriptor {
    static final List<String> ID_PREFIXES = List.of("fastproof.id:", "allure.id:", "allure_id:");
    static final String LABEL_PREFIX = "fastproof.label.";

    final String idPrefix;
    final String key;
    String name;
    String fullName;
    String testCaseId;
    String description;
    String sourceFile;
    Integer sourceLine;
    final List<String[]> labels = new ArrayList<>();
    final List<String[]> links = new ArrayList<>();
    final List<String[]> parameters = new ArrayList<>();

    /**
     * @param idPrefix two letters of the client id ({@code ju}, {@code tn})
     * @param key identifies the test (and its invocation) in this run; the same key again is the next attempt
     * @param name short name shown in the UI
     */
    public TestDescriptor(final String idPrefix, final String key, final String name) {
        this.idPrefix = idPrefix;
        this.key = key;
        this.name = name;
    }

    public String key() {
        return key;
    }

    public TestDescriptor fullName(final String value) {
        this.fullName = value;
        return this;
    }

    public TestDescriptor testCaseId(final String value) {
        this.testCaseId = value;
        return this;
    }

    public TestDescriptor description(final String value) {
        this.description = value;
        return this;
    }

    public TestDescriptor source(final String file, final Integer line) {
        this.sourceFile = file;
        this.sourceLine = line;
        return this;
    }

    public TestDescriptor label(final String name, final String value) {
        if (name != null && value != null) {
            labels.add(new String[] {name, value});
        }
        return this;
    }

    public TestDescriptor link(final String name, final String url, final String type) {
        if (url != null) {
            links.add(new String[] {name, url, type});
        }
        return this;
    }

    /**
     * Tags or groups become {@code tag} labels; {@code fastproof.id:X} (or {@code allure.id:X}) sets the test case id and
     * {@code fastproof.label.name:value} adds a label.
     */
    public TestDescriptor tags(final Collection<String> tags) {
        for (final String t : tags) {
            boolean id = false;
            for (final String prefix : ID_PREFIXES) {
                if (t.startsWith(prefix)) {
                    id = true;
                    this.testCaseId = t.substring(prefix.length());
                }
            }
            if (id) {
                continue;
            }
            final int colon = t.indexOf(':');
            if (t.startsWith(LABEL_PREFIX) && colon > LABEL_PREFIX.length()) {
                label(t.substring(LABEL_PREFIX.length(), colon), t.substring(colon + 1));
            } else {
                label("tag", t);
            }
        }
        return this;
    }

    public TestDescriptor parameter(final String name, final String value) {
        parameters.add(new String[] {name, value});
        return this;
    }
}
