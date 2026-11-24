package sample;

import static org.testng.Assert.assertEquals;
import static org.testng.Assert.assertTrue;

import io.franshub.fastproof.FastProof;
import io.franshub.fastproof.testng.Retry;

import java.nio.file.Files;
import java.nio.file.Path;

import org.testng.SkipException;
import org.testng.annotations.DataProvider;
import org.testng.annotations.Test;

public class TestNgSampleTest {
    private static final String SECRET = System.getenv().getOrDefault("SAMPLE_API_TOKEN", "no-secret");

    private static Path state(final String name) {
        return Path.of(System.getenv().getOrDefault("SAMPLE_STATE_DIR", "target")).resolve(name);
    }

    @Test(groups = "smoke")
    public void passesWithSteps() {
        FastProof.step("outer", () -> FastProof.step("inner", () -> System.out.println("using " + SECRET)));
        System.err.println("stderr line");
        FastProof.attach("note-" + SECRET + ".txt", "body with " + SECRET);
    }

    @Test
    public void failsAssertion() {
        assertEquals(1, 2, "numbers differ");
    }

    @Test(retryAnalyzer = Retry.class)
    public void flakyPassesOnRetry() throws Exception {
        final Path p = state("flaky-testng.count");
        final int n = Files.exists(p) ? Integer.parseInt(Files.readString(p).strip()) : 0;
        Files.writeString(p, String.valueOf(n + 1));
        assertTrue(n >= 1, "attempt " + n);
    }

    @Test(invocationCount = 3)
    public void repeats() {
        System.out.println("again");
    }

    @DataProvider(name = "users")
    public Object[][] users() {
        return new Object[][] {{"alice"}, {"bob"}};
    }

    @Test(dataProvider = "users")
    public void parametrized(final String user) {
        assertTrue(!user.isEmpty());
    }

    @Test
    public void skipped() {
        throw new SkipException("needs linux");
    }
}
