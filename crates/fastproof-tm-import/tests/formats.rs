//! Parsers on synthetic fixtures written from the documented columns of each format.

use std::collections::BTreeMap;
use std::path::PathBuf;

use fastproof_tm_import::{
    Format, ImportError, ImportedCase, ImportedKey, ImportedParameterRow, ImportedParameters,
    ImportedRequirement, ImportedRequirementRef, ImportedSharedStep, ImportedStep, ImportedValue,
    ParseLimits, ParseOptions, RowError, StepsLayout, detect, export, parse_all, parse_full,
    preview,
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

fn qase(doc: &str) -> Result<(Vec<ImportedCase>, Vec<RowError>), ImportError> {
    parse_all(Format::QaseJson, doc.as_bytes(), &ParseOptions::default())
}

fn invalid(r: Result<(Vec<ImportedCase>, Vec<RowError>), ImportError>) -> String {
    match r {
        Err(ImportError::Invalid(m)) => m,
        other => panic!("not invalid: {other:?}"),
    }
}

/// The streamed Qase JSON reader: key order, root shapes and limits.
#[test]
fn qase_json_streaming_rules() {
    let (cases, _) = qase(r#"[{"id": 1, "title": "A"}, {"id": 2, "title": "B"}]"#).unwrap();
    assert_eq!(cases.len(), 2);
    assert_eq!(cases[1].keys[0].value, "2");

    let (cases, _) = qase(
        r#"{"suites_flat": [{"id": 5, "title": "Top"}, {"id": 6, "title": "Leaf", "parent_id": 5}],
            "cases": [{"id": 1, "title": "A", "suite_id": 6}]}"#,
    )
    .unwrap();
    assert_eq!(cases[0].folder, vec!["Top", "Leaf"]);
    let m = invalid(qase(
        r#"{"cases": [{"id": 1, "title": "A", "suite_id": 6}], "suiteList": [{"id": 6, "title": "Leaf"}]}"#,
    ));
    assert!(m.contains("suites_flat"), "{m}");
    let m = invalid(qase(
        r#"{"cases": [{"id": 1, "title": "A"}], "suiteList": [{"id": 6, "title": "Leaf"}]}"#,
    ));
    assert!(m.contains("suites_flat"), "a flat list after any case: {m}");

    let m = invalid(qase(
        r#"{"cases": [{"id": 1, "title": "A"}], "code": "LATE"}"#,
    ));
    assert!(m.contains("code"), "{m}");
    let (cases, _) =
        qase(r#"{"project_code": "P", "cases": [{"id": 1, "title": "A"}], "code": "P"}"#).unwrap();
    assert_eq!(cases[0].keys[0].value, "P-1");
    let m = invalid(qase(
        r#"{"suites": [{"cases": [{"id": 4, "title": "A"}]}], "code": "C"}"#,
    ));
    assert!(m.contains("code"), "{m}");

    let (cases, _) = qase(
        r#"{"status": true, "result": {"total": 1, "entities": [{"id": 1, "title": "E"}]}, "cases": [{"id": 2, "title": "C"}]}"#,
    )
    .unwrap();
    assert_eq!(
        cases.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        vec!["E"]
    );

    let (cases, _) = qase(
        r#"{"suites": [{"suites": [{"cases": [{"id": 1, "title": "Deep"}], "title": "Inner"}], "title": 7}]}"#,
    )
    .unwrap();
    assert_eq!(cases[0].folder, vec!["7", "Inner"]);

    assert!(invalid(qase(r#"{"suites": null}"#)).contains("no suites or cases"));
    assert!(invalid(qase("42")).contains("no suites or cases"));
    assert!(qase(r#"{"suites": []}"#).unwrap().0.is_empty());
    assert!(invalid(qase(r#"{"cases": []} x"#)).contains("invalid JSON"));
    assert!(invalid(qase(r#"{"cases": [{"id": 1"#)).contains("invalid JSON"));

    let long = format!(
        r#"{{"cases": [{{"id": 1, "title": "{}"}}]}}"#,
        "x".repeat(65_536 * 6 + 1)
    );
    assert!(matches!(
        qase(&long),
        Err(ImportError::Limit {
            limit: "maxImportCellBytes",
            max: 65_536
        })
    ));
    let small = ParseOptions {
        limits: ParseLimits {
            max_rows: 2,
            ..ParseLimits::default()
        },
        ..ParseOptions::default()
    };
    let three =
        r#"{"cases": [{"id": 1, "title": "A"}, {"id": 2, "title": "B"}, {"id": 3, "title": "C"}]}"#;
    assert!(matches!(
        parse_all(Format::QaseJson, three.as_bytes(), &small),
        Err(ImportError::Limit {
            limit: "maxImportRows",
            max: 2
        })
    ));
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

/// Round 2 of the Qase JSON reader: flat suite lists only before the cases (a second one
/// too) and counted with the cases, suite cycles and long suite names bounded, string length
/// counted decoded.
#[test]
fn qase_json_bounds_suites_and_strings() {
    let m = invalid(qase(
        r#"{"suites_flat": [{"id": 1, "title": "A"}], "cases": [{"id": 1, "title": "x", "suite_id": 1}],
            "suiteList": [{"id": 1, "title": "B"}]}"#,
    ));
    assert!(m.contains("suites_flat"), "{m}");
    let m = invalid(qase(
        r#"{"suites": [{"title": "S", "cases": [{"id": 1, "title": "x"}]}], "suites_flat": []}"#,
    ));
    assert!(m.contains("suites_flat"), "{m}");
    let three = ParseOptions {
        limits: ParseLimits {
            max_rows: 3,
            ..ParseLimits::default()
        },
        ..ParseOptions::default()
    };
    let doc = r#"{"suites_flat": [{"id": 1, "title": "A"}, {"id": 2, "title": "B"}],
        "cases": [{"id": 1, "title": "x"}, {"id": 2, "title": "y"}]}"#;
    assert!(matches!(
        parse_all(Format::QaseJson, doc.as_bytes(), &three),
        Err(ImportError::Limit {
            limit: "maxImportRows",
            max: 3
        })
    ));

    let long = "t".repeat(60_000);
    let doc = format!(
        r#"{{"suites_flat": [{{"id": 1, "title": "{long}", "parent_id": 2}}, {{"id": 2, "title": "{long}", "parent_id": 1}}],
            "cases": [{{"id": 1, "title": "a", "suite_id": 1}}, {{"id": 2, "title": "b", "suite_id": 2}}]}}"#
    );
    let (cases, _) = qase(&doc).unwrap();
    let max = ParseLimits::default();
    for c in &cases {
        assert!(
            c.folder.len() <= max.max_folder_depth + 1,
            "{}",
            c.folder.len()
        );
        assert!(c.folder.len() >= 2, "both suites of the cycle");
        assert!(c.folder.iter().all(|n| n.len() == max.max_name_bytes + 1));
    }

    let at = format!(
        r#"{{"cases": [{{"id": 1, "title": "{}"}}]}}"#,
        "x".repeat(65_536)
    );
    assert_eq!(qase(&at).unwrap().0.len(), 1);
    let over = format!(
        r#"{{"cases": [{{"id": 1, "title": "{}"}}]}}"#,
        "x".repeat(65_537)
    );
    assert!(matches!(
        qase(&over),
        Err(ImportError::Limit {
            limit: "maxImportCellBytes",
            ..
        })
    ));
    let escaped = format!(
        r#"{{"cases": [{{"id": 1, "title": "{}"}}]}}"#,
        "\\u00e9".repeat(21_846)
    );
    assert!(
        matches!(
            qase(&escaped),
            Err(ImportError::Limit {
                limit: "maxImportCellBytes",
                ..
            })
        ),
        "a \\u escape counts as 3 decoded bytes"
    );
}

/// Round 2: a cycle of Qase CSV suites ends the path, which has at most
/// `maxFolderDepth + 1` levels, and long suite names are cut.
#[test]
fn qase_csv_suite_cycle_is_bounded() {
    let long = "s".repeat(2000);
    let csv = format!(
        "id,title,suite,suite_id,suite_parent_id,suite_without_cases\n\
         ,{long},{long},1,2,1\n,S2,S2,2,1,1\n5,Case,,1,,0\n"
    );
    let (cases, errors) =
        parse_all(Format::QaseCsv, csv.as_bytes(), &ParseOptions::default()).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    let f = &cases[0].folder;
    assert_eq!(f.len(), 2, "{f:?}");
    assert_eq!(f[1].len(), ParseLimits::default().max_name_bytes + 1);
}

fn key(kind: &str, value: &str) -> ImportedKey {
    ImportedKey {
        kind: kind.into(),
        value: value.into(),
    }
}

#[test]
fn ndjson_v2_records_come_with_their_types() {
    let p = parse_full(
        Format::FastproofNdjson,
        &fixture("fastproof-v2.ndjson")[..],
        &ParseOptions::default(),
    )
    .unwrap();
    assert_eq!(p.shared_steps.len(), 1);
    let ss = &p.shared_steps[0];
    assert_eq!((ss.number, ss.row), (Some(3), 2));
    assert_eq!(ss.keys, vec![key("qase", "abc")]);
    assert_eq!(ss.steps[1].steps[0].id, "s_l3");
    assert_eq!(p.requirements.len(), 2);
    assert_eq!(p.requirements[0].req_type, "epic");
    assert_eq!(p.requirements[1].parent, Some(1));
    assert_eq!(p.requirements[1].issue_key.as_deref(), Some("SHOP-9"));
    assert_eq!(p.cases.len(), 1);
    let c = &p.cases[0];
    assert_eq!(c.steps[0].shared, Some(key("number", "3")));
    assert!(c.steps[0].action.is_empty());
    let pr = c.parameters.as_ref().unwrap();
    assert_eq!(pr.rows.len(), 2);
    assert!(!pr.rows[1].enabled);
    assert_eq!(c.requirements[0].key, "REQ-2");
    assert_eq!(c.requirements[0].origin.as_deref(), Some("ui"));
    assert_eq!(
        p.errors.iter().map(|e| e.row).collect::<Vec<_>>(),
        vec![6, 7],
        "an unknown or missing type is a row error: {:?}",
        p.errors
    );
}

#[test]
fn ndjson_v2_new_records_have_their_own_limits() {
    let few = |f: fn(&mut ParseLimits)| {
        let mut l = ParseLimits::default();
        f(&mut l);
        ParseOptions {
            limits: l,
            ..Default::default()
        }
    };
    let file = fixture("fastproof-v2.ndjson");
    let e = parse_full(
        Format::FastproofNdjson,
        &file[..],
        &few(|l| l.max_shared_steps = 0),
    )
    .unwrap_err();
    assert!(
        matches!(
            e,
            ImportError::Limit {
                limit: "maxSharedSteps",
                ..
            }
        ),
        "{e:?}"
    );
    let e = parse_full(
        Format::FastproofNdjson,
        &file[..],
        &few(|l| l.max_requirements = 1),
    )
    .unwrap_err();
    assert!(
        matches!(
            e,
            ImportError::Limit {
                limit: "maxRequirements",
                ..
            }
        ),
        "{e:?}"
    );
    let p = parse_full(
        Format::FastproofNdjson,
        &file[..],
        &few(|l| l.max_parameter_rows = 1),
    )
    .unwrap();
    assert!(p.cases.is_empty());
    assert!(
        p.errors
            .iter()
            .any(|e| e.row == 5 && e.code == "limit_reached"),
        "{:?}",
        p.errors
    );
    let p = parse_full(
        Format::FastproofNdjson,
        &file[..],
        &few(|l| l.max_shared_step_steps = 2),
    )
    .unwrap();
    assert!(
        p.shared_steps.is_empty(),
        "3 steps in a shared step of at most 2"
    );
    assert!(p.errors.iter().any(|e| e.row == 2), "{:?}", p.errors);
    let p = parse_full(
        Format::FastproofNdjson,
        &file[..],
        &few(|l| l.max_parameter_value_bytes = 4),
    )
    .unwrap();
    assert!(p.cases.is_empty() && p.errors.iter().any(|e| e.row == 5));
}

#[test]
fn what_an_export_writes_an_import_reads() {
    let shared = ImportedSharedStep {
        number: Some(2),
        keys: vec![key("testrail", "9")],
        name: "Log in".into(),
        description: "d".into(),
        steps: vec![ImportedStep {
            id: "s_a".into(),
            action: "Open".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let req = ImportedRequirement {
        number: Some(4),
        title: "Pay".into(),
        req_type: "story".into(),
        status: Some("done".into()),
        parent: Some(1),
        issue_key: Some("SHOP-1".into()),
        ..Default::default()
    };
    let case = ImportedCase {
        number: Some(1),
        name: "Buy".into(),
        steps: vec![ImportedStep {
            id: "s_r".into(),
            shared: Some(key("number", "2")),
            ..Default::default()
        }],
        parameters: Some(ImportedParameters {
            columns: vec!["a".into()],
            rows: vec![ImportedParameterRow {
                id: "r_1".into(),
                values: BTreeMap::from([("a".to_string(), "1".to_string())]),
                enabled: false,
            }],
            generator: None,
        }),
        requirements: vec![ImportedRequirementRef {
            key: "REQ-4".into(),
            origin: Some("code".into()),
            status: Some("proposed".into()),
        }],
        ..Default::default()
    };
    let mut file = export::ndjson_header("DEMO", "2026-11-26T00:00:00Z", Vec::new());
    file += &export::ndjson_shared_step(&shared);
    file += &export::ndjson_requirement(&req);
    file += &export::ndjson_case(&case);
    assert!(!file.contains("\"row\""), "{file}");
    let p = parse_full(
        Format::FastproofNdjson,
        file.as_bytes(),
        &ParseOptions::default(),
    )
    .unwrap();
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(p.shared_steps[0].steps, shared.steps);
    assert_eq!(p.shared_steps[0].keys, shared.keys);
    assert_eq!(p.requirements[0].req_type, "story");
    assert_eq!(p.requirements[0].status.as_deref(), Some("done"));
    assert_eq!(p.requirements[0].parent, Some(1));
    let c = &p.cases[0];
    assert_eq!(c.steps, case.steps);
    assert_eq!(c.parameters, case.parameters);
    assert_eq!(c.requirements, case.requirements);
}

#[test]
fn csv_export_expands_shared_steps_and_parameters() {
    let shared = ImportedSharedStep {
        number: Some(2),
        name: "Log in".into(),
        steps: vec![
            ImportedStep {
                action: "Open the login page".into(),
                ..Default::default()
            },
            ImportedStep {
                action: "Sign in".into(),
                expected: "Dashboard".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let case = ImportedCase {
        number: Some(1),
        name: "Buy".into(),
        steps: vec![
            ImportedStep {
                shared: Some(key("number", "2")),
                ..Default::default()
            },
            ImportedStep {
                action: "Pay with ${card}".into(),
                ..Default::default()
            },
        ],
        parameters: Some(ImportedParameters {
            columns: vec!["card".into(), "note".into()],
            rows: vec![
                ImportedParameterRow {
                    id: "r_1".into(),
                    values: BTreeMap::from([
                        ("card".to_string(), "visa".to_string()),
                        ("note".to_string(), "a\nb".to_string()),
                    ]),
                    enabled: true,
                },
                ImportedParameterRow {
                    id: "r_2".into(),
                    values: BTreeMap::from([("card".to_string(), "amex".to_string())]),
                    enabled: false,
                },
            ],
            generator: None,
        }),
        ..Default::default()
    };
    let mut lookup = export::shared_lookup(std::slice::from_ref(&shared));
    let line = export::csv_case(&case, &[], &mut lookup);
    assert!(
        line.contains("1. Open the login page\n2. Sign in\n3. Pay with ${card}"),
        "{line}"
    );
    assert!(line.contains("card=visa; note=a b"), "{line}");
    assert!(!line.contains("amex"), "a disabled row is left out: {line}");
    assert!(export::csv_header(&[]).contains("Parameters"));
    let nd = export::ndjson_case(&case);
    assert!(
        nd.contains("\"shared\"") && nd.contains("amex"),
        "NDJSON keeps both: {nd}"
    );
}

#[test]
fn testrail_shared_step_ids_group_rows_and_references_become_requirements() {
    let p = parse_full(
        Format::TestrailCsv,
        &fixture("testrail-shared.csv")[..],
        &ParseOptions::default(),
    )
    .unwrap();
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(p.shared_steps.len(), 1, "defined once, by the first case");
    let ss = &p.shared_steps[0];
    assert_eq!(ss.keys, vec![key("testrail", "5")]);
    assert_eq!(ss.steps.len(), 2);
    assert_eq!(ss.steps[0].action, "Log in as a user");
    assert_eq!(
        ss.steps[0].expected, "Logged in",
        "the first case's content"
    );
    assert!(ss.steps.iter().all(|s| s.shared.is_none()));
    assert_eq!(p.cases.len(), 2);
    let buy = &p.cases[0];
    assert_eq!(
        buy.steps.len(),
        4,
        "the rows stay, so an unknown shared step falls back to text"
    );
    assert_eq!(buy.steps[0].shared, None);
    assert_eq!(buy.steps[1].shared, Some(key("testrail", "5")));
    assert_eq!(buy.steps[2].shared, Some(key("testrail", "5")));
    assert_eq!(buy.steps[3].shared, None);
    assert!(buy.tags.contains(&"imported-shared-step".to_string()));
    assert_eq!(
        buy.requirements
            .iter()
            .map(|r| r.key.as_str())
            .collect::<Vec<_>>(),
        vec!["SHOP-1", "SHOP-2"]
    );
    assert_eq!(p.cases[1].steps[0].shared, Some(key("testrail", "5")));
    assert_eq!(p.cases[1].requirements.len(), 1);
}

#[test]
fn qase_json_shared_steps_and_parameters() {
    let doc = r#"{"code":"SHOP","suites":[{"id":1,"title":"Cart","cases":[
      {"id":7,"title":"Pay","steps":[
        {"action":"Open","expected_result":"ok"},
        {"action":"Log in","shared_step_hash":"h1","shared_step_nested_steps":[{"action":"Type user"},{"action":"Type pass"}]},
        {"action":"Pay as {{User role}}","expected_result":"Paid"}],
       "params":{"single":{"User role":["admin","guest"]},"group":[{"browser":"chrome","os":"linux"},{"browser":"firefox","os":"mac"}]}},
      {"id":8,"title":"Other","steps":[{"action":"Log in","shared_step_hash":"h1"}],"params":{"Mode":["a","b"]}},
      {"id":9,"title":"Odd","params":{"single":"what"}}]}]}"#;
    let p = parse_full(Format::QaseJson, doc.as_bytes(), &ParseOptions::default()).unwrap();
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(p.shared_steps.len(), 1);
    assert_eq!(p.shared_steps[0].keys, vec![key("qase", "h1")]);
    assert_eq!(p.shared_steps[0].steps.len(), 2);
    assert_eq!(p.shared_steps[0].steps[1].action, "Type pass");
    let pay = &p.cases[0];
    assert_eq!(pay.steps[1].shared, Some(key("qase", "h1")));
    assert_eq!(pay.steps[2].action, "Pay as ${User_role}");
    let pr = pay.parameters.as_ref().unwrap();
    assert_eq!(pr.columns, vec!["User_role", "browser", "os"]);
    assert_eq!(pr.rows.len(), 4);
    assert!(pr.rows.iter().all(|r| r.enabled && r.id.starts_with("r_")));
    assert!(pr.rows.iter().any(|r| r.values["User_role"] == "guest"
        && r.values["browser"] == "firefox"
        && r.values["os"] == "mac"));
    assert!(
        !pay.description.contains("Parameters"),
        "{}",
        pay.description
    );
    let other = &p.cases[1];
    assert_eq!(other.steps[0].shared, Some(key("qase", "h1")));
    assert_eq!(other.parameters.as_ref().unwrap().rows.len(), 2);
    let odd = &p.cases[2];
    assert!(odd.parameters.is_none());
    assert!(
        odd.description.contains("what"),
        "an unknown shape stays as text: {}",
        odd.description
    );
}

#[test]
fn qase_parameters_obey_the_limits() {
    let doc = r#"{"suites":[{"title":"s","cases":[{"id":1,"title":"Big","params":{"a":["1","2","3"],"b":["1","2","3"]}}]}]}"#;
    let opts = ParseOptions {
        limits: ParseLimits {
            max_parameter_rows: 8,
            ..Default::default()
        },
        ..Default::default()
    };
    let p = parse_full(Format::QaseJson, doc.as_bytes(), &opts).unwrap();
    assert!(p.cases.is_empty());
    assert_eq!(p.errors[0].code, "limit_reached", "{:?}", p.errors);
    let opts = ParseOptions {
        limits: ParseLimits {
            max_parameter_columns: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let p = parse_full(Format::QaseJson, doc.as_bytes(), &opts).unwrap();
    assert!(p.cases.is_empty() && p.errors.len() == 1, "{:?}", p.errors);
}

/// Review M2: a CSV export expands each reference into the shared step's text. 50 references
/// to a 64 KiB shared step made a 3 MiB line from a few bytes of a case, and 50 000 such
/// cases a 160 GB file. Expansion is charged now, per case and per export: past the budget a
/// reference is a note, so the result is never much larger than what was stored.
#[test]
fn csv_expansion_of_shared_steps_is_charged_to_a_budget() {
    let big = ImportedSharedStep {
        number: Some(7),
        name: "Big".into(),
        steps: vec![ImportedStep {
            action: "a".repeat(60 * 1024),
            ..Default::default()
        }],
        ..Default::default()
    };
    let case = ImportedCase {
        number: Some(1),
        name: "Many references".into(),
        steps: (0..50)
            .map(|_| ImportedStep {
                shared: Some(key("number", "7")),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    let mut lookup = export::SharedLookup::new(vec![big.clone()]);
    let line = export::csv_case(&case, &[], &mut lookup);
    assert!(line.len() < 1100 * 1024, "{} bytes", line.len());
    assert_eq!(
        line.matches(&"a".repeat(60 * 1024)).count(),
        17,
        "1 MiB per case"
    );
    assert!(
        line.contains("Shared step SS-7 (not expanded"),
        "{}",
        &line[..200]
    );
    let mut small = export::SharedLookup::with_budget(vec![big], 3 * 61 * 1024);
    let first = export::csv_case(&case, &[], &mut small);
    let second = export::csv_case(&case, &[], &mut small);
    assert_eq!(first.matches(&"a".repeat(60 * 1024)).count(), 3);
    assert_eq!(
        second.matches(&"a".repeat(60 * 1024)).count(),
        0,
        "the export budget is spent"
    );
    assert!(second.contains("SS-7 (not expanded"));
}
