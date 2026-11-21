package com.example.samples;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.qameta.allure.AllureId;
import io.qameta.allure.Allure;
import io.qameta.allure.AttachmentOptions;
import io.qameta.allure.Description;
import io.qameta.allure.Epic;
import io.qameta.allure.Feature;
import io.qameta.allure.Issue;
import io.qameta.allure.Link;
import io.qameta.allure.Owner;
import io.qameta.allure.Severity;
import io.qameta.allure.SeverityLevel;
import io.qameta.allure.Story;
import io.qameta.allure.TmsLink;
import io.qameta.allure.http.HttpExchange;
import io.qameta.allure.http.HttpExchangeBody;
import java.io.ByteArrayInputStream;
import java.util.Base64;
import java.util.HashMap;
import java.util.Map;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Assumptions;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Disabled;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Tag;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.CsvSource;

@Epic("Shop")
@Feature("Checkout")
@DisplayName("Checkout")
class CheckoutTest {

    static final byte[] PNG = Base64.getDecoder().decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==");

    static final Map<String, Integer> PRICES = new HashMap<>();

    private Map<String, Integer> cart;

    @BeforeAll
    static void loadPrices() {
        Allure.step("Load price list", () -> {
            PRICES.put("apple", 3);
            PRICES.put("pear", 4);
            PRICES.put("plum", 5);
        });
    }

    @BeforeEach
    void createCart() {
        Allure.step("Create empty cart", () -> cart = new HashMap<>());
    }

    @AfterEach
    void clearCart() {
        Allure.step("Clear cart", () -> cart.clear());
    }

    @Test
    @AllureId("1001")
    @Story("Pay for a cart")
    @Severity(SeverityLevel.CRITICAL)
    @Owner("alice")
    @Tag("smoke")
    @Link(name = "spec", url = "https://docs.example.com/checkout")
    @Issue("FP-1")
    @TmsLink("TC-1")
    @Description("Adds two items, checks the total and attaches the receipt.")
    @DisplayName("Pay for a cart with two items")
    void payForCart() {
        Allure.step("Fill the cart", () -> {
            Allure.step("Add apple", () -> cart.put("apple", 2));
            Allure.step("Add pear", () -> {
                cart.put("pear", 1);
                Allure.attachment("cart.json", "application/json", "{\"apple\":2,\"pear\":1}", AttachmentOptions.withFileExtension(".json"));
            });
        });
        Allure.step("Check the total", () -> {
            int total = cart.entrySet().stream().mapToInt(e -> PRICES.get(e.getKey()) * e.getValue()).sum();
            Allure.step("Compare with expected total", () -> assertEquals(10, total));
            Allure.attachment("receipt", "text/plain", "apple x2 = 6\npear x1 = 4\ntotal = 10\n", AttachmentOptions.withFileExtension(".txt"));
            Allure.attachment("screenshot", "image/png", new ByteArrayInputStream(PNG), AttachmentOptions.withFileExtension(".png"));
        });
        System.out.println("checkout: total computed");
    }

    @Test
    @AllureId("1002")
    @Severity(SeverityLevel.NORMAL)
    @Owner("bob")
    @DisplayName("Discount is applied to the total")
    void discountApplied() {
        Allure.story("Apply a discount");
        Allure.label("layer", "api");
        Allure.issue("FP-2", "https://issues.example.com/browse/FP-2");
        Allure.tms("TC-2", "https://tms.example.com/case/TC-2");
        Allure.link("design", "https://docs.example.com/discounts");
        Allure.parameter("discount", "10%");
        Allure.description("Expected the 10% discount to reduce 50 to 45.");
        Allure.addHttpExchange("POST /discounts", HttpExchange.builder()
                .request("POST", "https://api.example.com/discounts?code=SPRING10", r -> r
                        .addHeader("Content-Type", "application/json")
                        .addHeader("Authorization", "Bearer example-token")
                        .setBody(HttpExchangeBody.utf8("{\"code\":\"SPRING10\"}")))
                .response(r -> r
                        .setStatus(200)
                        .setStatusText("OK")
                        .addHeader("Content-Type", "application/json")
                        .setBody(HttpExchangeBody.utf8("{\"percent\":10}")))
                .start(1767225600000L)
                .stop(1767225600042L)
                .build());
        cart.put("plum", 10);
        int total = Allure.step("Compute discounted total", () -> PRICES.get("plum") * cart.get("plum") * 9 / 10 + 1);
        System.out.println("discount: total " + total);
        System.err.println("discount: rounding mode is not configured");
        System.out.println("[[ATTACHMENT|attachments/screenshot.png]]");
        assertEquals(45, total, "discounted total");
    }

    @Test
    @AllureId("1003")
    @Severity(SeverityLevel.BLOCKER)
    @DisplayName("Unknown item breaks the checkout")
    void unknownItem() {
        Integer price = Allure.step("Look up price of an unknown item", () -> PRICES.get("kiwi"));
        if (price == null) {
            throw new IllegalStateException("No price for item: kiwi");
        }
    }

    @Test
    @AllureId("1004")
    @Disabled("Gift cards are not supported yet")
    @DisplayName("Pay with a gift card")
    void payWithGiftCard() {
        assertTrue(false);
    }

    @Test
    @AllureId("1005")
    @DisplayName("Pay in a foreign currency")
    void payInForeignCurrency() {
        Assumptions.assumeTrue(false, "Currency service is not configured");
    }

    @ParameterizedTest(name = "{0} x{1} costs {2}")
    @AllureId("1006")
    @Story("Price a single item")
    @CsvSource({"apple, 1, 3", "pear, 2, 8", "plum, 3, 16"})
    void priceOfItem(String item, int quantity, int expected) {
        Allure.parameter("item", item);
        int actual = Allure.step("Multiply price by quantity", () -> PRICES.get(item) * quantity);
        assertEquals(expected, actual);
    }
}
