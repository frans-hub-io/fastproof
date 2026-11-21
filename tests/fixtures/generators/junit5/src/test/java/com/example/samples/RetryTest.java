package com.example.samples;

import static org.junit.jupiter.api.Assertions.assertEquals;

import io.qameta.allure.Allure;
import io.qameta.allure.AllureId;
import io.qameta.allure.Epic;
import io.qameta.allure.Feature;
import io.qameta.allure.model.Parameter;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

@Epic("Shop")
@Feature("Payments")
class RetryTest {

    static int nextAttempt(String name) throws IOException {
        Path counter = Paths.get("target", "attempts", name);
        Files.createDirectories(counter.getParent());
        int attempt = Files.exists(counter) ? Integer.parseInt(Files.readString(counter).trim()) + 1 : 1;
        Files.writeString(counter, Integer.toString(attempt), StandardCharsets.UTF_8);
        return attempt;
    }

    @Test
    @AllureId("2001")
    @DisplayName("Payment gateway answers on retry")
    void flakyGateway() throws IOException {
        int attempt = nextAttempt("flakyGateway");
        Allure.parameter("attempt", attempt, true, Parameter.Mode.HIDDEN);
        System.out.println("flakyGateway attempt " + attempt);
        Allure.step("Call payment gateway", () -> assertEquals("OK", attempt == 1 ? "TIMEOUT" : "OK"));
    }

    @Test
    @AllureId("2002")
    @DisplayName("Refund is always rejected")
    void refundRejected() throws IOException {
        int attempt = nextAttempt("refundRejected");
        System.out.println("refundRejected attempt " + attempt);
        assertEquals("APPROVED", "REJECTED", "refund status");
    }

    @Test
    @AllureId("2003")
    @DisplayName("Card vault is unreachable")
    void cardVaultUnreachable() throws IOException {
        int attempt = nextAttempt("cardVaultUnreachable");
        System.err.println("cardVaultUnreachable attempt " + attempt);
        throw new IllegalStateException("Card vault connection refused (attempt " + attempt + ")");
    }
}
