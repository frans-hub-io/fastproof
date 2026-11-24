package io.franshub.fastproof.junit;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assumptions.assumeTrue;

import io.franshub.fastproof.FastProof;

import java.nio.charset.StandardCharsets;
import java.util.logging.Logger;
import java.util.stream.Stream;

import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Disabled;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Nested;
import org.junit.jupiter.api.RepeatedTest;
import org.junit.jupiter.api.Tag;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.TestFactory;
import org.junit.jupiter.api.DynamicTest;
import org.junit.jupiter.api.extension.ExtendWith;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

final class Fixtures {
    private Fixtures() {
    }

    static final String SECRET = "junit-fixture-secret-77";

    static class Basic {
        @Test
        @DisplayName("pays with a card")
        @Tag("smoke")
        @Tag("fastproof.id:PAY-1")
        @Tag("fastproof.label.owner:team-a")
        void pays() {
            System.out.println("paying " + SECRET);
            System.err.println("warning line");
            Logger.getLogger("shop").info("log record");
            FastProof.step("outer", () -> FastProof.step("inner", () -> System.out.println("inside")));
            FastProof.attach("note.txt", "body " + SECRET);
        }

        @Test
        void failsAssertion() {
            assertEquals(1, 2, "one is not two");
        }

        @Test
        void breaks() {
            throw new IllegalStateException("db is down");
        }

        @Test
        @Disabled("not today")
        void skipped() {
        }

        @Test
        void aborted() {
            assumeTrue(false, "needs linux");
        }

        @Nested
        class Inner {
            @Test
            void nestedPasses() {
                System.out.println("nested");
            }
        }
    }

    @ExtendWith(FastProofExtension.class)
    static class Params {
        @ParameterizedTest
        @ValueSource(ints = {1, 2, 3})
        void doubles(final int value) {
            assertEquals(value * 2, value + value);
        }

        @RepeatedTest(2)
        void repeated() {
        }

        @TestFactory
        Stream<DynamicTest> dynamic() {
            return Stream.of(DynamicTest.dynamicTest("first", () -> {
            }), DynamicTest.dynamicTest("second", () -> {
                throw new AssertionError("dyn");
            }));
        }
    }

    static class BrokenSetup {
        @BeforeAll
        static void boom() {
            throw new IllegalStateException("no database");
        }

        @Test
        void neverRuns() {
        }
    }

    @Disabled("whole class")
    static class DisabledClass {
        @Test
        void a() {
        }

        @Test
        void b() {
        }
    }

    static class Parallel {
        @Test
        void p1() {
            run("p1");
        }

        @Test
        void p2() {
            run("p2");
        }

        @Test
        void p3() {
            run("p3");
        }

        @Test
        void p4() {
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

    static class Retried {
        static int calls;

        @Test
        void flaky() {
            calls++;
            if (calls == 1) {
                throw new AssertionError("first time");
            }
        }
    }

    static byte[] bytes(final String s) {
        return s.getBytes(StandardCharsets.UTF_8);
    }
}
