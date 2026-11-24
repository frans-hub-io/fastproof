package io.franshub.fastproof.testng;

import io.franshub.fastproof.FastProof;

import java.util.logging.Logger;

import org.testng.Assert;
import org.testng.SkipException;
import org.testng.annotations.BeforeClass;
import org.testng.annotations.DataProvider;
import org.testng.annotations.Test;

final class Fixtures {
    private Fixtures() {
    }

    static final String SECRET = "testng-fixture-secret-55";

    public static class Basic {
        @Test(groups = {"smoke", "fastproof.id:PAY-9"}, description = "pays by card")
        public void pays() {
            System.out.println("paying " + SECRET);
            System.err.println("warning line");
            Logger.getLogger("shop").info("log record");
            FastProof.step("outer", () -> FastProof.step("inner", () -> System.out.println("inside")));
            FastProof.attach("note.txt", "body " + SECRET);
        }

        @Test
        public void failsAssertion() {
            Assert.assertEquals(1, 2, "one is not two");
        }

        @Test
        public void breaks() {
            throw new IllegalStateException("db is down");
        }

        @Test
        public void skips() {
            throw new SkipException("needs linux");
        }

        @Test(enabled = false)
        public void disabled() {
        }
    }

    public static class Flaky {
        static int calls;

        @Test(retryAnalyzer = Retry.class)
        public void flaky() {
            calls++;
            if (calls < 3) {
                Assert.fail("attempt " + calls);
            }
        }
    }

    public static class Repeated {
        @Test(invocationCount = 3)
        public void repeats() {
            System.out.println("again");
        }

        @DataProvider(name = "rows")
        public Object[][] rows() {
            return new Object[][] {{1, "a"}, {2, "b"}, {3, "c"}};
        }

        @Test(dataProvider = "rows")
        public void rowsTest(final int number, final String letter) {
            Assert.assertTrue(number > 0 && !letter.isEmpty());
        }
    }

    public static class BrokenSetup {
        @BeforeClass
        public void boom() {
            throw new IllegalStateException("no database");
        }

        @Test
        public void neverRuns() {
        }
    }

    public static class Parallel {
        @Test
        public void p1() {
            run("p1");
        }

        @Test
        public void p2() {
            run("p2");
        }

        @Test
        public void p3() {
            run("p3");
        }

        @Test
        public void p4() {
            run("p4");
        }

        private static void run(final String name) {
            for (int i = 0; i < 25; i++) {
                System.out.println(name + " line " + i);
                try {
                    Thread.sleep(2);
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
            }
        }
    }
}
