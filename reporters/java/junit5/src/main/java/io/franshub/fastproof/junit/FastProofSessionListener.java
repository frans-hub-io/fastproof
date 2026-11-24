package io.franshub.fastproof.junit;

import io.franshub.fastproof.Reporter;

import org.junit.platform.launcher.LauncherSession;
import org.junit.platform.launcher.LauncherSessionListener;

/** Starts the reporter with the Launcher session and sends what is queued when it ends; the process exit closes the run. */
public final class FastProofSessionListener implements LauncherSessionListener {
    @Override
    public void launcherSessionOpened(final LauncherSession session) {
        try {
            Reporter.get();
        } catch (RuntimeException | LinkageError e) {
            System.err.println("[fastproof] reporter error (ignored): " + e);
        }
    }

    @Override
    public void launcherSessionClosed(final LauncherSession session) {
        try {
            final Reporter r = Reporter.get();
            if (r != null) {
                r.drain();
            }
        } catch (RuntimeException | LinkageError e) {
            System.err.println("[fastproof] reporter error (ignored): " + e);
        }
    }
}
