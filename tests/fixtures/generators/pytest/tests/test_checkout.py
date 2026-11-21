import json

import allure
import pytest

from conftest import tiny_png


@allure.epic("Web shop")
@allure.feature("Checkout")
@allure.story("Pay with card")
@allure.severity(allure.severity_level.CRITICAL)
@allure.tag("smoke", "api")
@allure.label("owner", "qa-team")
@allure.id("1001")
@allure.link("https://docs.example.com/checkout", name="Checkout spec")
@allure.issue("https://tracker.example.com/browse/SHOP-12", name="SHOP-12")
@allure.testcase("https://tms.example.com/case/TC-1001", name="TC-1001")
@allure.title("Checkout succeeds with a valid card")
def test_checkout_passes(api_client, cart):
    """Places an order with a valid card and checks the order total.

    The description comes from the docstring.
    """
    print("checkout started")
    with allure.step("Prepare order"):
        with allure.step("Add items to cart"):
            allure.attach("A-1 x2", name="cart.txt", attachment_type=allure.attachment_type.TEXT)
        with allure.step("Build payload"):
            payload = {"items": cart, "card": "4111********1111"}
            allure.attach(json.dumps(payload), name="payload", attachment_type=allure.attachment_type.JSON)
    with allure.step("Submit order"):
        allure.attach(tiny_png(), name="screenshot", attachment_type=allure.attachment_type.PNG)
        total = sum(i["qty"] * i["price"] for i in cart)
    assert total == 20


@allure.epic("Web shop")
@allure.feature("Checkout")
@allure.story("Discounts")
@allure.severity(allure.severity_level.NORMAL)
@allure.issue("SHOP-34")
def test_discount_is_applied(cart, record_property):
    record_property("discount_code", "WELCOME10")
    with allure.step("Apply discount WELCOME10"):
        total = sum(i["qty"] * i["price"] for i in cart)
    assert total == 18, "discount must reduce the total by 10%"


@allure.epic("Web shop")
@allure.feature("Checkout")
@allure.severity(allure.severity_level.BLOCKER)
def test_tax_service_broken():
    with allure.step("Call tax service"):
        rate = None
        return 100 * rate


@allure.feature("Checkout")
def test_gift_cards_skipped():
    pytest.skip("gift cards are not enabled in this environment")


@allure.feature("Checkout")
@pytest.mark.skip(reason="feature flag CHECKOUT_V3 is off")
def test_checkout_v3():
    pass


@allure.feature("Payments")
@allure.story("Card brands")
@pytest.mark.parametrize(
    "brand,number,expected",
    [("visa", "4111111111111111", True), ("mastercard", "5500000000000004", True), ("unknown", "0000", False)],
)
def test_card_validation(brand, number, expected):
    allure.dynamic.title(f"Card validation for {brand}")
    allure.dynamic.parameter("cvv", "123", mode=allure.parameter_mode.MASKED)
    allure.dynamic.parameter("request_id", f"req-{brand}", excluded=True)
    allure.dynamic.parameter("api_key", "secret-key", mode=allure.parameter_mode.HIDDEN)
    assert (len(number) == 16) is expected


@allure.feature("Payments")
@allure.story("Retries")
@pytest.mark.flaky(reruns=2)
def test_payment_gateway_flaky(attempt_counter):
    attempt = attempt_counter("test_payment_gateway_flaky")
    print(f"attempt {attempt}")
    assert attempt >= 2, f"gateway timeout on attempt {attempt}"


@allure.feature("Payments")
@allure.story("Retries")
@pytest.mark.flaky(reruns=2)
def test_refund_always_fails(attempt_counter):
    attempt = attempt_counter("test_refund_always_fails")
    assert attempt == 0, f"refund rejected on attempt {attempt}"


@allure.feature("Payments")
@pytest.mark.xfail(reason="SHOP-56 rounding bug", strict=False)
def test_currency_rounding_xfail():
    assert round(2.675, 2) == 2.68


@allure.feature("Payments")
@pytest.mark.xfail(reason="SHOP-57 maybe fixed", strict=False)
def test_currency_symbol_xpass():
    assert "EUR" == "EUR"


@allure.feature("Orders")
def test_order_history_setup_broken(database):
    assert database is not None
