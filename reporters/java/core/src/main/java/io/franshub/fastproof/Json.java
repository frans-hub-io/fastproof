package io.franshub.fastproof;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** A minimal JSON writer and parser: maps, lists, strings, numbers, booleans and null. */
final class Json {
    private Json() {
    }

    static String write(final Object v) {
        final StringBuilder sb = new StringBuilder();
        write(sb, v);
        return sb.toString();
    }

    @SuppressWarnings("unchecked")
    private static void write(final StringBuilder sb, final Object v) {
        if (v == null) {
            sb.append("null");
        } else if (v instanceof String s) {
            string(sb, s);
        } else if (v instanceof Number || v instanceof Boolean) {
            sb.append(v);
        } else if (v instanceof Map<?, ?> m) {
            sb.append('{');
            boolean first = true;
            for (final Map.Entry<String, Object> e : ((Map<String, Object>) m).entrySet()) {
                if (e.getValue() == null) {
                    continue;
                }
                if (!first) {
                    sb.append(',');
                }
                first = false;
                string(sb, e.getKey());
                sb.append(':');
                write(sb, e.getValue());
            }
            sb.append('}');
        } else if (v instanceof Iterable<?> it) {
            sb.append('[');
            boolean first = true;
            for (final Object o : it) {
                if (!first) {
                    sb.append(',');
                }
                first = false;
                write(sb, o);
            }
            sb.append(']');
        } else {
            string(sb, v.toString());
        }
    }

    private static void string(final StringBuilder sb, final String s) {
        sb.append('"');
        for (int i = 0; i < s.length(); i++) {
            final char c = s.charAt(i);
            switch (c) {
                case '"' -> sb.append("\\\"");
                case '\\' -> sb.append("\\\\");
                case '\n' -> sb.append("\\n");
                case '\r' -> sb.append("\\r");
                case '\t' -> sb.append("\\t");
                default -> {
                    if (c < 0x20) {
                        sb.append(String.format("\\u%04x", (int) c));
                    } else {
                        sb.append(c);
                    }
                }
            }
        }
        sb.append('"');
    }

    /** Parses a JSON document; returns {@code null} when it is not valid. */
    static Object parse(final String text) {
        try {
            final Parser p = new Parser(text);
            final Object v = p.value();
            p.ws();
            return p.i == text.length() ? v : null;
        } catch (RuntimeException e) {
            return null;
        }
    }

    @SuppressWarnings("unchecked")
    static Map<String, Object> object(final Object v) {
        return v instanceof Map ? (Map<String, Object>) v : Map.of();
    }

    @SuppressWarnings("unchecked")
    static List<Object> array(final Object v) {
        return v instanceof List ? (List<Object>) v : List.of();
    }

    /** Deeper documents are rejected, so a test plan, spool line or server answer cannot overflow the stack. */
    static final int MAX_DEPTH = 256;

    private static final class Parser {
        private final String s;
        private int i;
        private int depth;

        void enter() {
            if (++depth > MAX_DEPTH) {
                throw new IllegalStateException("nested too deeply");
            }
        }

        Parser(final String s) {
            this.s = s;
        }

        void ws() {
            while (i < s.length() && Character.isWhitespace(s.charAt(i))) {
                i++;
            }
        }

        Object value() {
            ws();
            final char c = s.charAt(i);
            if (c == '{') {
                i++;
                enter();
                final Map<String, Object> m = new LinkedHashMap<>();
                ws();
                if (s.charAt(i) == '}') {
                    i++;
                    depth--;
                    return m;
                }
                while (true) {
                    ws();
                    final String k = str();
                    ws();
                    expect(':');
                    m.put(k, value());
                    ws();
                    if (s.charAt(i) == ',') {
                        i++;
                    } else {
                        expect('}');
                        depth--;
                        return m;
                    }
                }
            }
            if (c == '[') {
                i++;
                enter();
                final List<Object> l = new ArrayList<>();
                ws();
                if (s.charAt(i) == ']') {
                    i++;
                    depth--;
                    return l;
                }
                while (true) {
                    l.add(value());
                    ws();
                    if (s.charAt(i) == ',') {
                        i++;
                    } else {
                        expect(']');
                        depth--;
                        return l;
                    }
                }
            }
            if (c == '"') {
                return str();
            }
            if (s.startsWith("true", i)) {
                i += 4;
                return Boolean.TRUE;
            }
            if (s.startsWith("false", i)) {
                i += 5;
                return Boolean.FALSE;
            }
            if (s.startsWith("null", i)) {
                i += 4;
                return null;
            }
            final int start = i;
            while (i < s.length() && "+-0123456789.eE".indexOf(s.charAt(i)) >= 0) {
                i++;
            }
            final String num = s.substring(start, i);
            if (num.matches("-?\\d{1,18}")) {
                return Long.valueOf(num);
            }
            return Double.valueOf(num);
        }

        void expect(final char c) {
            if (s.charAt(i) != c) {
                throw new IllegalStateException("expected " + c);
            }
            i++;
        }

        String str() {
            expect('"');
            final StringBuilder sb = new StringBuilder();
            while (true) {
                final char c = s.charAt(i++);
                if (c == '"') {
                    return sb.toString();
                }
                if (c != '\\') {
                    sb.append(c);
                    continue;
                }
                final char e = s.charAt(i++);
                switch (e) {
                    case 'n' -> sb.append('\n');
                    case 'r' -> sb.append('\r');
                    case 't' -> sb.append('\t');
                    case 'b' -> sb.append('\b');
                    case 'f' -> sb.append('\f');
                    case 'u' -> {
                        sb.append((char) Integer.parseInt(s.substring(i, i + 4), 16));
                        i += 4;
                    }
                    default -> sb.append(e);
                }
            }
        }
    }
}
