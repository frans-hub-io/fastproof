package example;

import org.testng.annotations.Test;

public class OuterTest {
    @Test
    public void outerPass() {
    }

    public static class InnerTest {
        @Test
        public void innerPass() {
        }
    }
}
