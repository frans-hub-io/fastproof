package sample;

import static org.testng.Assert.assertEquals;
import static org.testng.Assert.assertTrue;

import io.qameta.allure.Allure;

import java.nio.file.Files;
import java.nio.file.Path;

import org.testng.annotations.DataProvider;
import org.testng.annotations.Test;

public class TestNgSampleTest {
    public static class TwoRetries implements org.testng.IRetryAnalyzer {
        private int used;

        @Override
        public boolean retry(final org.testng.ITestResult result) {
            return used++ < 2;
        }
    }

    private static final String SECRET = System.getenv().getOrDefault("SAMPLE_API_TOKEN", "no-secret");

    private static Path state(final String name) {
        return Path.of(System.getenv().getOrDefault("SAMPLE_STATE_DIR", "target")).resolve(name);
    }

    @Test
    public void passesWithSteps() {
        Allure.step("outer", () -> Allure.step("inner", () -> System.out.println("using " + SECRET)));
        Allure.attachment("note-" + SECRET + ".txt", "text/plain", "body with " + SECRET);
    }

    @Test
    public void failsAssertion() {
        assertEquals(1, 2, "numbers differ");
    }

    @Test(retryAnalyzer = TwoRetries.class)
    public void flakyPassesOnRerun() throws Exception {
        final Path p = state("flaky-testng.count");
        final int n = Files.exists(p) ? Integer.parseInt(Files.readString(p).strip()) : 0;
        Files.writeString(p, String.valueOf(n + 1));
        assertTrue(n >= 1, "attempt " + n);
    }

    @DataProvider(name = "users")
    public Object[][] users() {
        return new Object[][] {{"alice"}, {"bob"}};
    }

    @Test(dataProvider = "users")
    public void parametrized(final String user) {
        assertTrue(!user.isEmpty());
    }
}
