//! The committed schemas match the code. Regenerate with
//! `FASTPROOF_UPDATE_SCHEMAS=1 cargo test -p fastproof-server --test schemas`.

use std::path::PathBuf;

fn check(rel: &str, generated: String) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    if std::env::var("FASTPROOF_UPDATE_SCHEMAS").is_ok_and(|v| v == "1") {
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{rel}: {e}; regenerate with FASTPROOF_UPDATE_SCHEMAS=1"));
    assert!(
        committed == generated,
        "{rel} is out of date: regenerate with FASTPROOF_UPDATE_SCHEMAS=1 cargo test -p fastproof-server --test schemas"
    );
}

#[test]
fn report_schema_is_committed() {
    check(
        fastproof_server::schema::REPORT_SCHEMA_PATH,
        fastproof_server::schema::report_schema_text(),
    );
}

#[test]
fn openapi_is_committed() {
    check(
        "schemas/openapi.json",
        fastproof_server::app::openapi_text(),
    );
}

#[test]
fn report_schema_round_trip() {
    let schema = fastproof_model::report::report_json_schema();
    let defs = schema["$defs"].as_object().unwrap();
    for name in [
        "ReportResult",
        "ReportStep",
        "ReportAttachment",
        "ReportError",
        "Status",
        "Parameter",
        "Label",
        "Link",
    ] {
        assert!(defs.contains_key(name), "{name} missing from the schema");
    }
    let required: Vec<&str> = defs["ReportResult"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"id") && required.contains(&"name") && required.contains(&"status"));
    assert!(!schema.to_string().contains("#/components/"));
    let example = serde_json::json!({
        "id": "run-1/checkout-pays",
        "name": "pays",
        "fullName": "shop.CheckoutTest#pays",
        "status": "failed",
        "error": {"message": "expected 200, got 500"},
        "start": 1795249075967i64,
        "stop": 1795249076967i64,
        "parameters": [{"name": "browser", "value": "firefox"}, {"name": "token", "value": "x", "mode": "masked"}],
        "labels": [{"name": "suite", "value": "checkout"}],
        "steps": [{"name": "open cart", "status": "passed", "steps": [{"name": "click", "status": "passed"}]}],
        "attachments": [{"id": "0193a1b2-0000-7000-8000-000000000000", "name": "screenshot.png"}]
    });
    let r: fastproof_model::ReportResult = serde_json::from_value(example).unwrap();
    let again: fastproof_model::ReportResult =
        serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
    assert_eq!(again, r);
    assert_eq!(r.steps[0].steps[0].name, "click");
    assert!(
        serde_json::from_value::<fastproof_model::ReportResult>(
            serde_json::json!({"id": "x", "name": "y", "status": "passed", "bogus": 1})
        )
        .is_err()
    );
}
