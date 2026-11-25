package example;

import org.testng.IRetryAnalyzer;
import org.testng.ITestResult;

public class RetryOnce implements IRetryAnalyzer {
    private int count = 0;

    @Override
    public boolean retry(ITestResult result) {
        return count++ < 2;
    }
}
