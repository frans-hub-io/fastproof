package sample;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.sun.net.httpserver.HttpServer;
import io.franshub.fastproof.FastProof;

import java.net.InetSocketAddress;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.concurrent.atomic.AtomicReference;

import org.junit.jupiter.api.Assumptions;
import org.junit.jupiter.api.Disabled;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Nested;
import org.junit.jupiter.api.RepeatedTest;
import org.junit.jupiter.api.Tag;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

class JUnitSampleTest {
    private static final String SECRET = System.getenv().getOrDefault("SAMPLE_API_TOKEN", "no-secret");
    private static final System.Logger LOG = System.getLogger("sample.checkout");

    private static Path state(final String name) {
        return Path.of(System.getenv().getOrDefault("SAMPLE_STATE_DIR", "target")).resolve(name);
    }

    @Test
    @DisplayName("passes with steps, logs and attachments")
    @Tag("smoke")
    void passesWithStepsAndAttachments() throws Exception {
        FastProof.step("outer", () -> FastProof.step("inner", () -> System.out.println("stdout line with " + SECRET)));
        LOG.log(System.Logger.Level.INFO, "system logger record {0}", "x");
        System.err.println("stderr line");
        FastProof.attach("note-" + SECRET + ".txt", "body with " + SECRET);
        FastProof.attach("shared.bin", "application/octet-stream", new byte[2048]);
        assertEquals(2, 1 + 1);
    }

    @Test
    void failsAssertion() {
        assertEquals(1, 2, "numbers differ");
    }

    @Test
    void brokenByException() {
        throw new IllegalStateException("backend exploded");
    }

    @Test
    void flakyPassesOnRerun() throws Exception {
        final Path p = state("flaky-junit.count");
        final int n = Files.exists(p) ? Integer.parseInt(Files.readString(p).strip()) : 0;
        Files.writeString(p, String.valueOf(n + 1));
        assertTrue(n >= 1, "attempt " + n);
    }

    @ParameterizedTest
    @ValueSource(strings = {"alice", "bob"})
    void parametrized(final String user) {
        assertTrue(!user.isEmpty());
    }

    @RepeatedTest(2)
    void repeated() {
    }

    @Test
    @Disabled("not today")
    void disabled() {
    }

    @Test
    void skippedByAssumption() {
        Assumptions.assumeTrue(false, "needs linux");
    }

    @Nested
    class Basket {
        @Test
        void nestedPasses() {
            System.out.println("nested");
        }
    }

    @Test
    void traceparentReachesBackend() throws Exception {
        final AtomicReference<String> seen = new AtomicReference<>();
        final HttpServer server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext("/", ex -> {
            seen.set(ex.getRequestHeaders().getFirst("traceparent"));
            ex.sendResponseHeaders(200, 2);
            ex.getResponseBody().write("ok".getBytes(StandardCharsets.UTF_8));
            ex.close();
        });
        server.start();
        try {
            final HttpRequest.Builder b = HttpRequest.newBuilder(URI.create("http://127.0.0.1:" + server.getAddress().getPort() + "/"));
            FastProof.trace().orElseThrow().headers(false).forEach(b::header);
            HttpClient.newHttpClient().send(b.build(), HttpResponse.BodyHandlers.ofString());
        } finally {
            server.stop(0);
        }
        FastProof.attach("seen-headers.json", "application/json", ("{\"traceparent\":\"" + seen.get() + "\"}").getBytes(StandardCharsets.UTF_8));
        assertEquals(FastProof.trace().orElseThrow().traceparent(), seen.get());
    }
}
