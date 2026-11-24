package io.franshub.fastproof;

import java.io.IOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.security.SecureRandom;
import java.time.Duration;
import java.util.Map;
import java.util.Set;

/** The HTTP calls of PROTOCOL.md: bearer token, no redirects, backoff with full jitter, nothing is thrown. */
final class Http {
    static final String VERSION = "0.1.0";
    private static final SecureRandom RANDOM = new SecureRandom();
    private static final Set<String> RESTRICTED = Set.of("content-length", "host", "connection", "expect", "upgrade", "transfer-encoding");

    record Resp(int status, Object body, long retryAfterMs, String error) {
        boolean ok() {
            return status >= 200 && status < 300;
        }

        boolean retryable() {
            return status == 0 || status == 408 || status == 429 || status >= 500;
        }

        String code() {
            return Json.object(body).get("code") instanceof String s ? s : "";
        }
    }

    private final Config cfg;
    private final HttpClient http;

    Http(final Config cfg) {
        this.cfg = cfg;
        this.http = HttpClient.newBuilder().followRedirects(HttpClient.Redirect.NEVER).connectTimeout(Duration.ofSeconds(10)).build();
    }

    static long backoff(final int attempt) {
        final long exp = Math.min(8000, 250L << Math.min(attempt, 16));
        return exp / 2 + (long) (RANDOM.nextDouble() * (exp / 2));
    }

    String launches(final String project, final String suffix) {
        return cfg.url.replaceAll("/+$", "") + "/api/v1/projects/" + project + "/launches" + suffix;
    }

    Resp post(final String project, final String suffix, final String contentType, final HttpRequest.BodyPublisher body,
            final Map<String, String> headers, final int retries) {
        Resp last = new Resp(0, null, 0, "no request");
        for (int attempt = 0; attempt <= retries; attempt++) {
            last = once(launches(project, suffix), "POST", contentType, body, headers, true);
            if (!last.retryable() || attempt == retries) {
                return last;
            }
            try {
                Thread.sleep(last.retryAfterMs() > 0 ? last.retryAfterMs() : backoff(attempt));
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                return last;
            }
        }
        return last;
    }

    /** A pre-signed PUT: no bearer token, no redirects, never retried here. */
    Resp put(final String url, final Map<String, String> headers, final HttpRequest.BodyPublisher body, final long timeoutMs) {
        return once(url, "PUT", null, body, headers, false, timeoutMs);
    }

    private Resp once(final String url, final String method, final String contentType, final HttpRequest.BodyPublisher body,
            final Map<String, String> headers, final boolean auth) {
        return once(url, method, contentType, body, headers, auth, cfg.requestTimeoutMs);
    }

    private Resp once(final String url, final String method, final String contentType, final HttpRequest.BodyPublisher body,
            final Map<String, String> headers, final boolean auth, final long timeoutMs) {
        try {
            final HttpRequest.Builder b = HttpRequest.newBuilder(URI.create(url)).timeout(Duration.ofMillis(timeoutMs));
            if (auth) {
                b.header("accept", "application/json").header("user-agent", "fastproof-java/" + VERSION);
                if (contentType != null) {
                    b.header("content-type", contentType);
                }
                if (!cfg.token.isEmpty()) {
                    b.header("authorization", "Bearer " + cfg.token);
                }
            }
            for (final Map.Entry<String, String> h : headers.entrySet()) {
                if (!RESTRICTED.contains(h.getKey().toLowerCase())) {
                    b.header(h.getKey(), h.getValue());
                }
            }
            b.method(method, body);
            final HttpResponse<String> r = http.send(b.build(), HttpResponse.BodyHandlers.ofString());
            long ra = 0;
            final String h = r.headers().firstValue("retry-after").orElse(null);
            if (h != null) {
                try {
                    ra = Math.min(Math.max(Long.parseLong(h.strip()), 0), 60) * 1000;
                } catch (NumberFormatException e) {
                    ra = 0;
                }
            }
            return new Resp(r.statusCode(), r.body().isEmpty() ? null : Json.parse(r.body()), ra, "");
        } catch (IOException e) {
            return new Resp(0, null, 0, String.valueOf(e));
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return new Resp(0, null, 0, "interrupted");
        } catch (RuntimeException e) {
            return new Resp(-1, null, 0, String.valueOf(e));
        }
    }
}
