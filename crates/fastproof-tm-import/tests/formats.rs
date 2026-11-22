//! Parsers on synthetic fixtures written from the documented columns of each format.

use std::collections::BTreeMap;
use std::path::PathBuf;

use fastproof_tm_import::{
    Format, ImportError, ImportedCase, ImportedValue, ParseLimits, ParseOptions, RowError,
    StepsLayout, detect, parse_all, preview,
};

fn fixture(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(p).unwrap()
}

fn run(format: Format, name: &str) -> (Vec<ImportedCase>, Vec<RowError>) {
    parse_all(format, &fixture(name)[..], &ParseOptions::default()).unwrap()
}

fn text(v: &ImportedValue) -> &str {
    match v {
        ImportedValue::Text(s) => s,
        _ => panic!("not text: {v:?}"),
    }
}

#[test]
fn detection() {
    for (name, f) in [
        ("testrail-rows.csv", Format::TestrailCsv),
        ("testrail-cells.csv", Format::TestrailCsv),
        ("testrail-bom-shared.csv", Format::TestrailCsv),
        ("qase-v1.csv", Format::QaseCsv),
        ("qase-v2.csv", Format::QaseCsv),
        ("qase-export.json", Format::QaseJson),
        ("testops-tom.csv", Format::TestopsCsv),
        ("testops-semicolon.csv", Format::TestopsCsv),
        ("generic-cells.csv", Format::GenericCsv),
        ("fastproof.ndjson", Format::FastproofNdjson),
    ] {
        assert_eq!(detect(&fixture(name)), f, "{name}");
    }
    let p = preview(&fixture("testrail-rows.csv"), None).unwrap();
    assert_eq!(p.steps_layout, StepsLayout::Rows);
    assert_eq!(p.headers[1], "Title");
    assert_eq!(p.rows.len(), 5);
    assert_eq!(
        p.suggested_mapping.get("Browser").map(String::as_str),
        Some("field:Browser")
    );
    assert!(!p.suggested_mapping.contains_key("Title"));
    let p = preview(&fixture("testrail-cells.csv"), None).unwrap();
    assert_eq!(p.steps_layout, StepsLayout::Cells);
    let p = preview(&fixture("generic-cells.csv"), None).unwrap();
    assert_eq!(p.suggested_mapping["Summary"], "name");
    assert_eq!(p.suggested_mapping["Key"], "key");
    assert_eq!(p.suggested_mapping["Priority"], "field:Priority");
    let p = preview(&fixture("generic-rows.csv"), None).unwrap();
    assert_eq!(p.steps_layout, StepsLayout::Rows);
    let p = preview(&fixture("fastproof.ndjson"), None).unwrap();
    assert_eq!(p.rows[0][0], "Exported case");
}

#[test]
fn testrail_rows_layout() {
    let (cases, errors) = run(Format::TestrailCsv, "testrail-rows.csv");
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(cases.len(), 3);
    let c = &cases[0];
    assert_eq!(c.keys[0].kind, "testrail");
    assert_eq!(c.keys[0].value, "C3863");
    assert_eq!(c.name, "Login with a valid password");
    assert_eq!(c.folder, vec!["Accounts", "Login"]);
    assert_eq!(c.preconditions, "User **exists**");
    assert_eq!(c.steps.len(), 3);
    assert_eq!(c.steps[0].action, "Open the login page");
    assert_eq!(c.steps[0].expected, "The form is shown");
    assert_eq!(c.steps[1].data, "secret-free value");
    assert_eq!(c.steps[2].expected, "The dashboard opens");
    assert_eq!(text(&c.fields["priority"]), "High");
    assert_eq!(text(&c.fields["type"]), "Functional");
    assert_eq!(text(&c.fields["references"]), "DH-44, DH-28");
    assert_eq!(text(&c.fields["Browser"]), "Chrome");
    assert!(!c.fields.contains_key("Created By"));
    assert_eq!(c.row, 2);
    let r = &cases[2];
    assert_eq!(r.folder, vec!["Accounts", "Passwords", "Reset"]);
    assert_eq!(r.preconditions, "A user with an **email**");
    assert_eq!(r.steps[0].action, "Open *Forgot password*");
    assert_eq!(r.steps[0].expected, "- A mail is sent");
}

#[test]
fn testrail_cells_text_and_shared() {
    let (cases, _) = run(Format::TestrailCsv, "testrail-cells.csv");
    let c = &cases[0];
    assert_eq!(c.steps.len(), 3);
    assert_eq!(c.steps[1].action, "Type \"shoe\"");
    assert_eq!(c.steps[0].expected, "The catalog opens");
    assert_eq!(
        c.steps[1].expected, "Results show shoes",
        "expected results align by position"
    );
    assert_eq!(cases[1].steps.len(), 1);
    let (cases, _) = run(Format::TestrailCsv, "testrail-text.csv");
    assert_eq!(cases[0].steps[0].action, "Open the app and log in");
    assert_eq!(cases[0].steps[0].expected, "You see the home page");
    assert!(
        cases[1]
            .description
            .contains("**Mission**\n\nFind payment bugs")
    );
    assert!(cases[1].description.contains("Try `3DS` cards"));
    let (cases, _) = run(Format::TestrailCsv, "testrail-bom-shared.csv");
    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].name, "Шаг с юникодом");
    assert_eq!(cases[0].folder, vec!["Розділ", "Підрозділ"]);
    assert_eq!(cases[0].steps.len(), 2);
    assert_eq!(cases[0].tags, vec!["imported-shared-step"]);
    let rows = ParseOptions {
        steps_layout: Some(StepsLayout::Rows),
        ..Default::default()
    };
    let (cases, _) = parse_all(
        Format::TestrailCsv,
        &fixture("testrail-cells.csv")[..],
        &rows,
    )
    .unwrap();
    assert_eq!(
        cases[0].steps.len(),
        1,
        "rows layout keeps a cell as one step"
    );
}

#[test]
fn testrail_utf16() {
    let text = String::from_utf8(fixture("testrail-cells.csv")).unwrap();
    let mut le = vec![0xFF, 0xFE];
    for u in text.encode_utf16() {
        le.extend_from_slice(&u.to_le_bytes());
    }
    let (cases, _) = parse_all(Format::TestrailCsv, &le[..], &ParseOptions::default()).unwrap();
    assert_eq!(cases.len(), 2);
    assert_eq!(cases[0].name, "Search by name");
}

#[test]
fn qase_csv_v1_and_v2() {
    let (cases, errors) = run(Format::QaseCsv, "qase-v1.csv");
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(cases.len(), 2);
    let c = &cases[0];
    assert_eq!(c.keys[0].kind, "qase");
    assert_eq!(c.keys[0].value, "45");
    assert_eq!(c.folder, vec!["Authentication", "Login"]);
    assert_eq!(c.status.as_deref(), Some("active"));
    assert_eq!(c.tags, vec!["smoke", "login"]);
    assert_eq!(c.steps.len(), 2);
    assert_eq!(c.steps[1].expected, "Logged in");
    assert_eq!(c.steps[1].data, "user@example.com");
    assert!(c.description.contains("**Postconditions**\n\nLog out"));
    assert_eq!(text(&c.fields["priority"]), "high");
    assert_eq!(text(&c.fields["severity"]), "critical");
    assert_eq!(text(&c.fields["12"]), "Chrome");
    let old = &cases[1];
    assert_eq!(old.folder, vec!["Authentication"]);
    assert_eq!(old.status.as_deref(), Some("outdated"));
    assert!(
        !old.fields.contains_key("priority"),
        "undefined is no value"
    );
    assert_eq!(text(&old.fields["severity"]), "major");

    let (cases, errors) = run(Format::QaseCsv, "qase-v2.csv");
    assert!(errors.is_empty(), "{errors:?}");
    let c = &cases[0];
    assert_eq!(c.folder, vec!["Payments"]);
    assert_eq!(c.status.as_deref(), Some("draft"));
    assert_eq!(c.description, "Card **payment**");
    assert_eq!(c.steps.len(), 2);
    assert_eq!(c.steps[0].action, "Open checkout");
    assert_eq!(c.steps[0].steps[0].action, "Choose card, then \"Pay\"");
    assert_eq!(c.steps[0].steps[0].expected, "Card form");
    assert_eq!(c.steps[1].action, "Confirm\non two lines");
    assert_eq!(c.steps[1].expected, "Paid");
    assert!(cases[1].folder.is_empty());
}

#[test]
fn qase_json() {
    let (cases, errors) = run(Format::QaseJson, "qase-export.json");
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(cases.len(), 2);
    let c = &cases[0];
    assert_eq!(c.keys[0].value, "SHOP-7");
    assert_eq!(c.folder, vec!["Cart"]);
    assert_eq!(
        c.description,
        "Adds an item\n\n**Postconditions**\n\nCart is emptied"
    );
    assert_eq!(c.tags, vec!["cart", "smoke"]);
    assert_eq!(c.status.as_deref(), Some("active"));
    assert_eq!(text(&c.fields["Browser"]), "Chrome");
    assert_eq!(c.steps[0].steps[0].action, "Scroll");
    assert_eq!(c.steps[0].attachments, vec!["item.png"]);
    assert_eq!(c.attachments[0].name, "item.png");
    assert_eq!(c.steps[1].action, "Click **Add**");
    assert_eq!(cases[1].folder, vec!["Cart", "Checkout"]);
    assert_eq!(cases[1].status.as_deref(), Some("draft"));
    assert!(cases[1].tags.contains(&"imported-shared-step".to_string()));

    let (cases, errors) = run(Format::QaseJson, "qase-api.json");
    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].status.as_deref(), Some("outdated"));
    assert_eq!(errors.len(), 1, "the case without a title");
    assert_eq!(errors[0].code, "invalid_case");

    let e = parse_all(
        Format::QaseJson,
        &fixture("qase-deep.json")[..],
        &ParseOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(
            e,
            ImportError::Limit {
                limit: "maxJsonDepth",
                max: 64
            }
        ),
        "{e:?}"
    );
}

#[test]
fn testops_csv() {
    let (cases, errors) = run(Format::TestopsCsv, "testops-tom.csv");
    assert!(errors.is_empty(), "{errors:?}");
    let c = &cases[0];
    assert_eq!(c.keys[0].kind, "testops");
    assert_eq!(c.number, Some(1123));
    assert_eq!(c.steps.len(), 2);
    assert_eq!(c.steps[0].steps[0].action, "Click Profile");
    assert_eq!(c.steps[0].steps[0].steps[0].action, "Wait for load");
    assert_eq!(c.steps[1].expected, "The name is shown");
    assert_eq!(text(&c.fields["feature"]), "Profile");
    assert_eq!(text(&c.fields["story"]), "View");
    assert_eq!(text(&c.fields["layer"]), "UI");
    assert_eq!(c.links[0].url, "https://docs.example.com/profile");
    assert_eq!(c.links[1].kind, "issue");
    assert_eq!(c.status.as_deref(), Some("active"));
    assert!(!c.automated);
    assert!(cases[1].automated);
    assert_eq!(cases[1].full_names, vec!["com.example.HealthTest.ok"]);
    assert_eq!(cases[1].status.as_deref(), Some("draft"));
    assert!(
        cases[2]
            .description
            .ends_with("**Expected result**\n\nEverything works")
    );
    let (cases, _) = run(Format::TestopsCsv, "testops-semicolon.csv");
    assert_eq!(cases[0].number, Some(2001));
    assert_eq!(cases[0].steps.len(), 2);
    assert_eq!(cases[0].tags, vec!["a", "b"]);
    let (cases, _) = run(Format::TestopsCsv, "testops-ids.csv");
    assert_eq!(cases[0].number, None, "0 is no number");
    assert_eq!(cases[1].number, None);
    assert_eq!(text(&cases[0].fields["owner"]), "alice");
}

#[test]
fn generic_csv() {
    let (cases, _) = run(Format::GenericCsv, "generic-cells.csv");
    let c = &cases[0];
    assert_eq!(c.keys[0].kind, "external");
    assert_eq!(c.folder, vec!["Orders", "New"]);
    assert_eq!(c.steps[1].expected, "Saved");
    assert_eq!(c.tags, vec!["orders", "smoke"]);
    assert_eq!(
        text(&cases[1].fields["Notes"]),
        "=HYPERLINK(\"https://evil.example.com\")",
        "formulas stay text"
    );
    let mut m = BTreeMap::new();
    m.insert("Summary".to_string(), "name".to_string());
    m.insert("Steps".to_string(), "steps".to_string());
    m.insert("Priority".to_string(), "field:priority".to_string());
    let opts = ParseOptions {
        mapping: Some(m.clone()),
        ..Default::default()
    };
    let (cases, _) =
        parse_all(Format::GenericCsv, &fixture("generic-cells.csv")[..], &opts).unwrap();
    assert!(cases[0].keys.is_empty());
    assert_eq!(text(&cases[0].fields["priority"]), "high");
    assert!(!cases[0].fields.contains_key("Notes"));
    m.insert("Nope".into(), "name".into());
    let bad = ParseOptions {
        mapping: Some(m),
        ..Default::default()
    };
    assert!(parse_all(Format::GenericCsv, &fixture("generic-cells.csv")[..], &bad).is_err());
    let rows = ParseOptions {
        steps_layout: Some(StepsLayout::Rows),
        ..Default::default()
    };
    let (cases, _) =
        parse_all(Format::GenericCsv, &fixture("generic-rows.csv")[..], &rows).unwrap();
    assert_eq!(cases.len(), 2);
    assert_eq!(cases[0].steps.len(), 2);
    assert_eq!(cases[1].steps.len(), 2);
    assert_eq!(cases[1].steps[1].expected, "Same key continues");
    let (cases, errors) = run(Format::GenericCsv, "generic-broken.csv");
    assert!(cases.is_empty());
    assert_eq!(errors[0].code, "invalid_row");
}

#[test]
fn fastproof_ndjson() {
    let (cases, errors) = run(Format::FastproofNdjson, "fastproof.ndjson");
    assert_eq!(cases.len(), 2);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].row, 3);
    let c = &cases[0];
    assert_eq!(c.number, Some(5));
    assert_eq!(c.steps[0].id, "s_abc");
    assert_eq!(c.steps[0].steps[0].id, "s_def");
    assert_eq!(c.folder, vec!["A", "B"]);
    assert_eq!(c.status.as_deref(), Some("draft"));
    assert_eq!(text(&c.fields["browser"]), "chrome");
    assert!(
        parse_all(
            Format::FastproofNdjson,
            &fixture("fastproof-wrong.ndjson")[..],
            &ParseOptions::default()
        )
        .is_err()
    );
    let (cases, _) = run(Format::FastproofNdjson, "fastproof-empty.ndjson");
    assert!(cases.is_empty());
}

#[test]
fn limits() {
    let mut header: Vec<String> = (0..501).map(|i| format!("c{i}")).collect();
    header[0] = "name".into();
    let csv = header.join(",") + "\n";
    let e = parse_all(Format::GenericCsv, csv.as_bytes(), &ParseOptions::default()).unwrap_err();
    assert!(
        matches!(
            e,
            ImportError::Limit {
                limit: "maxColumns",
                ..
            }
        ),
        "{e:?}"
    );
    let small = ParseOptions {
        limits: ParseLimits {
            max_bytes: 100,
            ..Default::default()
        },
        ..Default::default()
    };
    let e = parse_all(
        Format::TestrailCsv,
        &fixture("testrail-rows.csv")[..],
        &small,
    )
    .unwrap_err();
    assert!(
        matches!(
            e,
            ImportError::Limit {
                limit: "maxImportBytes",
                ..
            }
        ),
        "{e:?}"
    );
    let rows = ParseOptions {
        limits: ParseLimits {
            max_rows: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    let e = parse_all(
        Format::TestrailCsv,
        &fixture("testrail-rows.csv")[..],
        &rows,
    )
    .unwrap_err();
    assert!(
        matches!(
            e,
            ImportError::Limit {
                limit: "maxImportRows",
                ..
            }
        ),
        "{e:?}"
    );
    let steps = ParseOptions {
        limits: ParseLimits {
            max_steps: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    let (cases, errors) = parse_all(
        Format::TestrailCsv,
        &fixture("testrail-rows.csv")[..],
        &steps,
    )
    .unwrap();
    assert_eq!(cases.len(), 2);
    assert_eq!(errors[0].code, "limit_reached");
    let cells = ParseOptions {
        limits: ParseLimits {
            max_cell_bytes: 40,
            ..Default::default()
        },
        ..Default::default()
    };
    let (_, errors) = parse_all(
        Format::TestrailCsv,
        &fixture("testrail-cells.csv")[..],
        &cells,
    )
    .unwrap();
    assert!(!errors.is_empty());
    let deep = ParseOptions {
        limits: ParseLimits {
            max_step_depth: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    let (cases, errors) =
        parse_all(Format::TestopsCsv, &fixture("testops-tom.csv")[..], &deep).unwrap();
    assert_eq!(cases.len(), 2);
    assert_eq!(errors.len(), 1);
}
