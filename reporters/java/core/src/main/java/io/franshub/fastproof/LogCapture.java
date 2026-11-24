package io.franshub.fastproof;

import java.io.ByteArrayOutputStream;
import java.io.OutputStream;
import java.io.PrintStream;
import java.nio.charset.Charset;
import java.util.logging.Handler;
import java.util.logging.Level;
import java.util.logging.LogRecord;
import java.util.logging.Logger;
import java.util.logging.SimpleFormatter;

/** Copies System.out, System.err and java.util.logging records (and so System.Logger) of the running attempt. */
final class LogCapture {
    private static final int MAX_LINE = 64 * 1024;
    private static final ThreadLocal<ByteArrayOutputStream> OUT_BUF = ThreadLocal.withInitial(ByteArrayOutputStream::new);
    private static final ThreadLocal<ByteArrayOutputStream> ERR_BUF = ThreadLocal.withInitial(ByteArrayOutputStream::new);
    private static boolean installed;
    private static Charset charset = Charset.defaultCharset();

    private LogCapture() {
    }

    static synchronized void install(final Config cfg) {
        if (installed) {
            return;
        }
        installed = true;
        final Logger root = Logger.getLogger("");
        root.getHandlers();
        final String enc = System.getProperty("stdout.encoding", System.getProperty("sun.stdout.encoding"));
        try {
            charset = enc != null ? Charset.forName(enc) : Charset.defaultCharset();
        } catch (RuntimeException e) {
            charset = Charset.defaultCharset();
        }
        if (cfg.captureOutput) {
            System.setOut(new PrintStream(new Tee(System.out, "stdout", OUT_BUF), true, charset));
            System.setErr(new PrintStream(new Tee(System.err, "stderr", ERR_BUF), true, charset));
        }
        final Level level = julLevel(cfg.logLevel);
        if (cfg.logLevelSet && (root.getLevel() == null || root.getLevel().intValue() > level.intValue())) {
            root.setLevel(level);
        }
        final Handler h = new Records();
        h.setLevel(level);
        root.addHandler(h);
    }

    static Level julLevel(final String name) {
        return switch (name) {
            case "TRACE" -> Level.FINEST;
            case "DEBUG" -> Level.FINE;
            case "WARN", "WARNING" -> Level.WARNING;
            case "ERROR", "SEVERE" -> Level.SEVERE;
            default -> Level.INFO;
        };
    }

    /** Emits what the current thread printed without a final newline. */
    static void flushThread() {
        flush(OUT_BUF.get(), "stdout");
        flush(ERR_BUF.get(), "stderr");
    }

    private static void flush(final ByteArrayOutputStream buf, final String stream) {
        if (buf.size() > 0) {
            emit(stream, buf);
        }
    }

    private static void emit(final String stream, final ByteArrayOutputStream buf) {
        String line = buf.toString(charset);
        buf.reset();
        if (line.endsWith("\r")) {
            line = line.substring(0, line.length() - 1);
        }
        final Attempt a = Attempt.current();
        if (a != null) {
            try {
                a.output(stream, line);
            } catch (RuntimeException e) {
                // capturing must never reach the test
            }
        }
    }

    private static final class Tee extends OutputStream {
        private final PrintStream original;
        private final String stream;
        private final ThreadLocal<ByteArrayOutputStream> buffers;

        Tee(final PrintStream original, final String stream, final ThreadLocal<ByteArrayOutputStream> buffers) {
            this.original = original;
            this.stream = stream;
            this.buffers = buffers;
        }

        @Override
        public void write(final int b) {
            original.write(b);
            feed((byte) b);
        }

        @Override
        public void write(final byte[] b, final int off, final int len) {
            original.write(b, off, len);
            for (int i = off; i < off + len; i++) {
                feed(b[i]);
            }
        }

        @Override
        public void flush() {
            original.flush();
        }

        private void feed(final byte b) {
            final ByteArrayOutputStream buf = buffers.get();
            if (b == '\n') {
                emit(stream, buf);
            } else {
                buf.write(b);
                if (buf.size() >= MAX_LINE) {
                    emit(stream, buf);
                }
            }
        }
    }

    private static final class Records extends Handler {
        private final SimpleFormatter formatter = new SimpleFormatter();

        @Override
        public void publish(final LogRecord r) {
            final Attempt a = Attempt.current();
            if (a == null || r == null || !isLoggable(r)) {
                return;
            }
            try {
                final int v = r.getLevel().intValue();
                final String level = v >= Level.SEVERE.intValue() ? "error" : v >= Level.WARNING.intValue() ? "warn"
                        : v >= Level.INFO.intValue() ? "info" : v >= Level.FINE.intValue() ? "debug" : "trace";
                final StringBuilder msg = new StringBuilder();
                if (r.getLoggerName() != null && !r.getLoggerName().isEmpty()) {
                    msg.append('[').append(r.getLoggerName()).append("] ");
                }
                msg.append(formatter.formatMessage(r));
                if (r.getThrown() != null) {
                    final java.io.StringWriter sw = new java.io.StringWriter();
                    r.getThrown().printStackTrace(new java.io.PrintWriter(sw));
                    msg.append('\n').append(sw);
                }
                a.log(level, msg.toString());
            } catch (RuntimeException e) {
                // capturing must never reach the test
            }
        }

        @Override
        public void flush() {
        }

        @Override
        public void close() {
        }
    }
}
