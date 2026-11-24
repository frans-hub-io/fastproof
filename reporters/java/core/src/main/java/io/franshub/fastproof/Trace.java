package io.franshub.fastproof;

import java.net.URLEncoder;
import java.nio.charset.StandardCharsets;
import java.security.SecureRandom;
import java.util.HexFormat;
import java.util.LinkedHashMap;
import java.util.Map;

/** W3C trace context of one attempt ({@code traceparent} and {@code baggage}) for FastMock and traced backends. */
public final class Trace {
    private static final SecureRandom RANDOM = new SecureRandom();

    private final String traceId;
    private final String spanId;
    private final String baggage;

    Trace(final String launch, final String test, final int attempt) {
        this.traceId = nonZero(random(16));
        this.spanId = nonZero(random(8));
        this.baggage = "fastproof.launch=" + enc(launch) + ",fastproof.test=" + enc(test) + ",fastproof.attempt=" + attempt;
    }

    private static String random(final int bytes) {
        final byte[] b = new byte[bytes];
        RANDOM.nextBytes(b);
        return HexFormat.of().formatHex(b);
    }

    private static String nonZero(final String hex) {
        return hex.chars().allMatch(c -> c == '0') ? hex.substring(0, hex.length() - 1) + "1" : hex;
    }

    private static String enc(final String v) {
        final String e = URLEncoder.encode(v, StandardCharsets.UTF_8).replace("+", "%20");
        return e.length() > 256 ? e.substring(0, 256) : e;
    }

    public String traceId() {
        return traceId;
    }

    public String traceparent() {
        return "00-" + traceId + "-" + spanId + "-01";
    }

    public String baggage() {
        return baggage;
    }

    /** Headers for an HTTP client; {@code newSpan} makes a fresh span id under the same trace. */
    public Map<String, String> headers(final boolean newSpan) {
        final Map<String, String> h = new LinkedHashMap<>();
        h.put("traceparent", newSpan ? "00-" + traceId + "-" + nonZero(random(8)) + "-01" : traceparent());
        h.put("baggage", baggage);
        return h;
    }

    /** A copy of the environment with {@code TRACEPARENT} and {@code BAGGAGE}, for child processes. */
    public Map<String, String> env() {
        final Map<String, String> e = new LinkedHashMap<>(System.getenv());
        e.put("TRACEPARENT", traceparent());
        e.put("BAGGAGE", baggage);
        return e;
    }
}
