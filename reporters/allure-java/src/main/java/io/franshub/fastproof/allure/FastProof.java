package io.franshub.fastproof.allure;

import java.util.Optional;

/** Access to FastProof data from test code. */
public final class FastProof {
    private FastProof() {
    }

    /** Trace context of the attempt running on this thread; empty outside a test or with reporting off. */
    public static Optional<Trace> trace() {
        return Optional.ofNullable(FastProofListener.currentTrace());
    }
}
