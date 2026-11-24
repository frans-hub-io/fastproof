package example;

import com.epam.reportportal.testng.ReportPortalTestNGListener;
import com.epam.reportportal.service.ReportPortal;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.testng.Assert;
import org.testng.SkipException;
import org.testng.annotations.AfterMethod;
import org.testng.annotations.BeforeClass;
import org.testng.annotations.BeforeMethod;
import org.testng.annotations.DataProvider;
import org.testng.annotations.Test;

import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.Calendar;

public class CartTest {
    private static final Logger LOG = LoggerFactory.getLogger(CartTest.class);
    private static int flaky = 0;

    @BeforeClass
    public void setUpClass() {
        LOG.info("class setup");
    }

    @BeforeMethod
    public void before() {
        LOG.debug("before method");
    }

    @AfterMethod
    public void after() {
        LOG.debug("after method");
    }

    @Test(description = "adds an item")
    public void addsItem() {
        LOG.info("adding item");
        Assert.assertEquals(1 + 1, 2);
    }

    @Test
    public void failsWithAssertion() {
        LOG.warn("about to fail");
        Assert.assertEquals(1 + 1, 3, "sum is wrong");
    }

    @Test(retryAnalyzer = RetryOnce.class)
    public void flakyThenPasses() {
        Assert.assertTrue(flaky++ >= 2);
    }

    @Test
    public void skippedByException() {
        throw new SkipException("skipped on purpose");
    }

    @Test(enabled = false)
    public void disabled() {
    }

    @Test(dependsOnMethods = "failsWithAssertion")
    public void skippedByDependency() {
    }

    @DataProvider(name = "pairs")
    public Object[][] pairs() {
        return new Object[][] {{1, 2}, {3, 4}, {5, 5}};
    }

    @Test(dataProvider = "pairs")
    public void parametrized(int a, int b) {
        LOG.info("a={} b={}", a, b);
        Assert.assertNotEquals(a, b);
    }

    @Test(invocationCount = 2)
    public void repeated() {
        LOG.info("repeat");
    }

    @Test
    public void logsWithAttachment() throws Exception {
        File f = File.createTempFile("note", ".txt");
        Files.write(f.toPath(), "hello attachment\n".getBytes(StandardCharsets.UTF_8));
        ReportPortal.emitLog("with file", "INFO", Calendar.getInstance().getTime(), f);
        ReportPortal.emitLog("plain emit", "WARN", Calendar.getInstance().getTime());
        LOG.error("error with exception", new IllegalStateException("boom"));
        f.deleteOnExit();
    }

    @Test
    public void throwsException() {
        throw new IllegalStateException("unexpected");
    }
}
