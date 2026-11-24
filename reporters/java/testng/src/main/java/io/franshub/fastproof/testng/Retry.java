package io.franshub.fastproof.testng;

import java.util.Arrays;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicInteger;

import org.testng.IRetryAnalyzer;
import org.testng.ITestResult;

/**
 * A retry analyzer for {@code @Test(retryAnalyzer = Retry.class)}: reruns a failed test up to {@code fastproof.testng.retries}
 * times (default 2), counted per test and data provider row. Every rerun is reported as an attempt of the same result.
 */
public final class Retry implements IRetryAnalyzer {
    private static final Map<String, AtomicInteger> USED = new ConcurrentHashMap<>();

    static int max() {
        final String v = System.getProperty("fastproof.testng.retries", System.getenv("FASTPROOF_TESTNG_RETRIES"));
        try {
            return v == null || v.isBlank() ? 2 : Math.max(0, Integer.parseInt(v.strip()));
        } catch (NumberFormatException e) {
            return 2;
        }
    }

    @Override
    public boolean retry(final ITestResult result) {
        final String key = result.getMethod().getRealClass().getName() + "#" + result.getMethod().getMethodName() + "|"
                + Arrays.deepToString(result.getParameters());
        return USED.computeIfAbsent(key, k -> new AtomicInteger()).getAndIncrement() < max();
    }
}
