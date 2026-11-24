package io.franshub.fastproof.testng;

import java.lang.reflect.Constructor;
import java.lang.reflect.Method;

import org.testng.IAnnotationTransformer;
import org.testng.annotations.ITestAnnotation;

/**
 * Retries every test that has no retry analyzer when {@code fastproof.testng.retries} (or {@code FASTPROOF_TESTNG_RETRIES})
 * is set above zero; without it nothing changes.
 */
public final class RetryTransformer implements IAnnotationTransformer {
    @Override
    public void transform(final ITestAnnotation annotation, final Class testClass, final Constructor testConstructor, final Method testMethod) {
        final String v = System.getProperty("fastproof.testng.retries", System.getenv("FASTPROOF_TESTNG_RETRIES"));
        if (v == null || v.isBlank() || Retry.max() <= 0) {
            return;
        }
        if (annotation.getRetryAnalyzerClass() == null || annotation.getRetryAnalyzerClass().getName().equals("org.testng.internal.annotations.DisabledRetryAnalyzer")) {
            annotation.setRetryAnalyzer(Retry.class);
        }
    }
}
