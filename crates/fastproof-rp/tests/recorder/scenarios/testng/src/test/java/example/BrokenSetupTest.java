package example;

import org.testng.annotations.BeforeClass;
import org.testng.annotations.Test;

public class BrokenSetupTest {
    @BeforeClass
    public void setUp() {
        throw new IllegalStateException("class setup exploded");
    }

    @Test
    public void neverRuns() {
    }

    @Test
    public void neverRunsEither() {
    }
}
