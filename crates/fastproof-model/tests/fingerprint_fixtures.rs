//! Fingerprints of real failures from `tests/fixtures` (Java, Python, JavaScript, Go).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use fastproof_model::fingerprint::{fingerprint, normalize_message, stack_frames};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// (fullName, status, message, trace) of the failed and broken Allure results of a fixture.
fn allure_failures(dir: &str) -> Vec<(String, String, Option<String>, Option<String>)> {
    let mut out = Vec::new();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixtures().join(dir))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with("-result.json"))
        .collect();
    paths.sort();
    for p in paths {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
        let status = v["status"].as_str().unwrap_or_default().to_string();
        if status != "failed" && status != "broken" {
            continue;
        }
        out.push((
            v["fullName"].as_str().unwrap_or_default().to_string(),
            status,
            v["statusDetails"]["message"].as_str().map(str::to_string),
            v["statusDetails"]["trace"].as_str().map(str::to_string),
        ));
    }
    out
}

fn by_test(
    items: &[(String, String, Option<String>, Option<String>)],
) -> BTreeMap<String, BTreeSet<String>> {
    let mut m: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (name, _, msg, trace) in items {
        let fp = fingerprint(msg.as_deref(), trace.as_deref()).unwrap();
        m.entry(name.clone()).or_default().insert(fp.hash);
    }
    m
}

#[test]
fn java_junit5() {
    let items = allure_failures("junit5/allure-results");
    assert!(items.len() >= 10);
    let m = by_test(&items);
    // Retries of one test fail the same way, including "(attempt 1)", "(attempt 2)", ...
    for (name, fps) in &m {
        if name.ends_with("flakyGateway") {
            continue;
        }
        assert_eq!(fps.len(), 1, "{name}: {fps:?}");
    }
    let all: BTreeSet<&String> = m.values().flatten().collect();
    assert!(all.len() >= 4, "{m:?}");
    let (_, _, msg, trace) = items
        .iter()
        .find(|i| i.0.ends_with("CheckoutTest.discountApplied"))
        .unwrap();
    let frames = stack_frames(trace.as_deref().unwrap());
    assert_eq!(
        frames,
        vec!["com.example.samples.CheckoutTest.discountApplied"]
    );
    assert_eq!(
        normalize_message(msg.as_deref().unwrap()),
        "discounted total ==> expected: <<n>> but was: <<n>>"
    );
    let skipped = std::fs::read_dir(fixtures().join("junit5/allure-results"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find_map(|p| {
            let v: serde_json::Value = serde_json::from_slice(&std::fs::read(p).ok()?).ok()?;
            let t = v["statusDetails"]["trace"].as_str()?;
            t.contains("Assumptions.java").then(|| t.to_string())
        })
        .unwrap();
    assert_eq!(
        stack_frames(&skipped),
        vec!["com.example.samples.CheckoutTest.payInForeignCurrency"]
    );
}

#[test]
fn python_pytest() {
    let items = allure_failures("pytest/allure-results");
    assert!(!items.is_empty());
    let m = by_test(&items);
    assert_eq!(m.values().flatten().collect::<BTreeSet<_>>().len(), m.len());
    let (_, _, msg, trace) = items
        .iter()
        .find(|i| i.0.contains("test_discount_is_applied"))
        .unwrap();
    let fp = fingerprint(msg.as_deref(), trace.as_deref()).unwrap();
    assert_eq!(
        fp.normalized_message,
        "AssertionError: discount must reduce the total by <n>% assert <n> == <n>"
    );
    assert_eq!(fp.frames, vec!["tests/test_checkout.py"]);
    let moved = trace
        .as_deref()
        .unwrap()
        .replace("0xffffad21c540", "0xffff0000beef");
    assert_eq!(
        fingerprint(msg.as_deref(), Some(&moved)).unwrap().hash,
        fp.hash
    );
}

#[test]
fn javascript_vitest_and_playwright() {
    for dir in ["vitest/allure-results", "playwright/allure-results"] {
        let items = allure_failures(dir);
        assert!(!items.is_empty(), "{dir}");
        for (name, _, msg, trace) in &items {
            let fp = fingerprint(msg.as_deref(), trace.as_deref()).unwrap();
            assert!(
                fp.frames
                    .iter()
                    .all(|f| !f.contains("node_modules") && !f.contains("<anonymous>")),
                "{dir} {name}: {:?}",
                fp.frames
            );
        }
        let m = by_test(&items);
        for (name, fps) in &m {
            assert_eq!(fps.len(), 1, "{dir} {name}: {fps:?}");
        }
    }
    let items = allure_failures("vitest/allure-results");
    let (_, _, msg, trace) = items
        .iter()
        .find(|i| i.2.as_deref() == Some("Cannot read properties of null (reading 'tax')"))
        .unwrap();
    let fp = fingerprint(msg.as_deref(), trace.as_deref()).unwrap();
    assert_eq!(fp.frames, vec!["tests/cart.test.ts"]);
    let other_checkout = trace.as_deref().unwrap().replace(
        "/builds/example/fastproof-samples",
        "/home/runner/work/shop/shop",
    );
    assert_eq!(
        fingerprint(msg.as_deref(), Some(&other_checkout))
            .unwrap()
            .hash,
        fp.hash
    );
}

fn xml_unescape(s: &str) -> String {
    s.replace("&#xA;", "\n")
        .replace("&#34;", "\"")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[test]
fn go_gotestsum() {
    let xml = std::fs::read_to_string(fixtures().join("gotestsum/junit.xml")).unwrap();
    let case = regex::Regex::new(
        r#"(?s)<testcase classname="([^"]*)" name="([^"]*)"[^>]*>\s*<failure message="([^"]*)"[^>]*>(.*?)</failure>"#,
    )
    .unwrap();
    let mut m: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for c in case.captures_iter(&xml) {
        let msg = xml_unescape(&c[3]);
        let body = xml_unescape(&c[4]);
        let fp = fingerprint(Some(&msg), Some(&body)).unwrap();
        m.entry(c[2].to_string()).or_default().insert(fp.hash);
    }
    assert!(m.len() >= 4, "{m:?}");
    assert_eq!(m["TestRefundAlwaysFails"].len(), 1);
    assert_eq!(m["TestDiscountApplied"].len(), 1);
    assert_ne!(m["TestDiscountApplied"], m["TestCardValidation/unknown"]);
    let fp = fingerprint(
        Some("Failed"),
        Some("=== RUN   TestDiscountApplied\n    checkout_test.go:24: total with discount WELCOME10: got 20, want 18\n--- FAIL: TestDiscountApplied (0.00s)\n"),
    )
    .unwrap();
    assert_eq!(
        fp.normalized_message,
        "total with discount WELCOME<n>: got <n>, want <n>"
    );
    assert_eq!(fp.frames, vec!["checkout_test.go"]);
}
