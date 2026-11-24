package io.franshub.fastproof;

import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.util.Locale;
import java.util.Optional;

/** Access to FastProof from test code; every call does nothing outside a test or with reporting off. */
public final class FastProof {
    private FastProof() {
    }

    /** A step body that may throw. */
    @FunctionalInterface
    public interface ThrowingRunnable<E extends Throwable> {
        void run() throws E;
    }

    /** A step body that returns a value and may throw. */
    @FunctionalInterface
    public interface ThrowingSupplier<T, E extends Throwable> {
        T get() throws E;
    }

    /** Trace context of the attempt running on this thread. */
    public static Optional<Trace> trace() {
        final Attempt a = Attempt.current();
        return a == null ? Optional.empty() : Optional.of(a.trace());
    }

    /** Runs a body as a named step: shown in the step tree and streamed live. */
    public static <E extends Throwable> void step(final String name, final ThrowingRunnable<E> body) throws E {
        step(name, () -> {
            body.run();
            return null;
        });
    }

    public static <T, E extends Throwable> T step(final String name, final ThrowingSupplier<T, E> body) throws E {
        final Attempt a = Attempt.current();
        final Attempt.Step s = a == null ? null : a.startStep(name);
        try {
            final T v = body.get();
            if (a != null) {
                a.endStep(s, null);
            }
            return v;
        } catch (Throwable t) {
            if (a != null) {
                a.endStep(s, t);
            }
            throw t;
        }
    }

    /** A log record in the attempt's live log ({@code trace|debug|info|warn|error}). */
    public static void log(final String level, final String message) {
        final Attempt a = Attempt.current();
        if (a != null) {
            a.log(level, message);
        }
    }

    public static void attach(final String name, final String contentType, final byte[] body) {
        final Attempt a = Attempt.current();
        if (a != null) {
            a.attach(name, contentType, body);
        }
    }

    public static void attach(final String name, final String text) {
        attach(name, "text/plain", text == null ? null : text.getBytes(StandardCharsets.UTF_8));
    }

    /** Attaches a file; it is streamed from disk after the test, so keep it until the run ends. */
    public static void attachFile(final Path file) {
        attachFile(null, null, file);
    }

    public static void attachFile(final String name, final String contentType, final Path file) {
        final Attempt a = Attempt.current();
        if (a != null && file != null) {
            a.attachFile(name, contentType != null ? contentType : contentType(file), file);
        }
    }

    public static void label(final String name, final String value) {
        final Attempt a = Attempt.current();
        if (a != null) {
            a.label(name, value);
        }
    }

    /** A content type from the file extension for the common attachment kinds. */
    static String contentType(final Path file) {
        final String n = file.getFileName().toString().toLowerCase(Locale.ROOT);
        final int dot = n.lastIndexOf('.');
        return switch (dot < 0 ? "" : n.substring(dot + 1)) {
            case "png" -> "image/png";
            case "jpg", "jpeg" -> "image/jpeg";
            case "gif" -> "image/gif";
            case "webp" -> "image/webp";
            case "svg" -> "image/svg+xml";
            case "webm" -> "video/webm";
            case "mp4" -> "video/mp4";
            case "txt", "log" -> "text/plain";
            case "html", "htm" -> "text/html";
            case "csv" -> "text/csv";
            case "json" -> "application/json";
            case "xml" -> "application/xml";
            case "zip" -> "application/zip";
            case "pdf" -> "application/pdf";
            default -> "application/octet-stream";
        };
    }
}
