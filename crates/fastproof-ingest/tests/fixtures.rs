//! Parsing of fixtures produced by the real writers (see `tests/fixtures/README.md`).

use std::collections::HashSet;
use std::path::PathBuf;

use fastproof_ingest::{IngestBatch, IngestOptions, MemorySink, ingest_path};
use fastproof_model::identity::{md5_hex, test_case_hash};
use fastproof_model::{AttachmentKind, IdSource, ParsedResult, SourceFormat, Stage, Status};

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(rel)
}

fn ingest_with(rel: &str, format: Option<SourceFormat>) -> (IngestBatch, MemorySink) {
    let options = IngestOptions {
        format,
        ..IngestOptions::default()
    };
    let mut sink = MemorySink::default();
    let batch = ingest_path(&fixture(rel), &options, &mut sink).expect(rel);
    (batch, sink)
}

fn ingest(rel: &str) -> IngestBatch {
    ingest_with(rel, None).0
}

fn by_name<'a>(b: &'a IngestBatch, name: &str) -> Vec<&'a ParsedResult> {
    b.results
        .iter()
        .filter(|r| r.identity.full_name.ends_with(name))
        .collect()
}

fn group_statuses(b: &IngestBatch, name: &str) -> (Vec<Status>, Status, bool) {
    let first = by_name(b, name)[0];
    let g = b
        .retry_groups()
        .into_iter()
        .find(|g| g.series_key == first.identity.series_key)
        .unwrap();
    (
        g.attempts.iter().map(|&i| b.results[i].status).collect(),
        b.results[g.latest].status,
        g.flaky_in_run,
    )
}

fn assert_no_missing_attachments(b: &IngestBatch) {
    let mut b = b.clone();
    for r in &mut b.results {
        r.visit_attachments_mut(&mut |a| {
            assert!(!a.missing, "{} missing", a.source);
            assert!(a.id.is_some());
        });
    }
}

#[test]
fn pytest_allure_results() {
    let (b, sink) = ingest_with("pytest/allure-results", None);
    assert_eq!(b.format, Some(SourceFormat::AllureResults));
    assert_eq!(b.results.len(), 16);
    assert_eq!(b.containers.len(), 19);
    assert!(b.warnings.is_empty(), "{:?}", b.warnings);
    assert_no_missing_attachments(&b);
    assert_eq!(b.counts.attachments_stored as usize, sink.items.len());

    let (statuses, latest, flaky) = group_statuses(&b, "#test_payment_gateway_flaky");
    assert_eq!(statuses, vec![Status::Failed, Status::Passed]);
    assert_eq!(latest, Status::Passed);
    assert!(flaky);
    let (statuses, _, flaky) = group_statuses(&b, "#test_refund_always_fails");
    assert_eq!(statuses.len(), 3);
    assert!(!flaky);
    let refund = by_name(&b, "#test_refund_always_fails");
    let g = b
        .retry_groups()
        .into_iter()
        .find(|g| g.series_key == refund[0].identity.series_key)
        .unwrap();
    assert!(
        b.results[g.latest]
            .status_details
            .message
            .as_deref()
            .unwrap()
            .contains("attempt 3")
    );

    let passes = &by_name(&b, "#test_checkout_passes")[0];
    let id = passes.identity.explicit_id.as_ref().unwrap();
    assert_eq!((id.source, id.value.as_str()), (IdSource::AllureId, "1001"));
    assert_eq!(
        passes.identity.legacy_test_case_hash.as_deref(),
        Some(md5_hex("ALLURE_ID=1001").as_str())
    );
    assert!(!passes.fixtures.befores.is_empty() && !passes.fixtures.afters.is_empty());
    assert!(passes.step_count() > passes.steps.len());
    let kinds: HashSet<AttachmentKind> = {
        let mut k = HashSet::new();
        let mut p = (*passes).clone();
        p.visit_attachments_mut(&mut |a| {
            k.insert(a.kind);
        });
        k
    };
    assert!(kinds.contains(&AttachmentKind::Image) && kinds.contains(&AttachmentKind::Json));
    assert!(passes.labels.iter().any(|l| l.name == "severity"));
    assert!(
        passes
            .links
            .iter()
            .any(|l| l.link_type.as_deref() == Some("issue"))
    );

    let cards = by_name(&b, "#test_card_validation");
    assert_eq!(cards.len(), 3);
    let keys: HashSet<&str> = cards
        .iter()
        .map(|r| r.identity.series_key.as_str())
        .collect();
    assert_eq!(keys.len(), 3, "parameters split the series");
    for c in &cards {
        for p in &c.parameters {
            if p.mode.is_secret() {
                assert!(p.value.is_none(), "secret value {} redacted", p.name);
            }
        }
        assert!(c.parameters.iter().any(|p| p.excluded));
    }
    let xfail = &by_name(&b, "#test_currency_rounding_xfail")[0];
    assert_eq!(xfail.status, Status::Skipped);

    assert_eq!(b.launch_meta.environment.len(), 5);
    assert_eq!(b.categories.len(), 4);
    assert_eq!(
        b.categories[1].message_regex.as_deref(),
        Some(".*RuntimeError.*")
    );
}

#[test]
fn allure_hashes_match_allure_algorithm_on_real_files() {
    for dir in [
        "pytest/allure-results",
        "junit5/allure-results",
        "playwright/allure-results",
        "vitest/allure-results",
    ] {
        let b = ingest(dir);
        for r in &b.results {
            let raw: serde_json::Value = serde_json::from_slice(
                &std::fs::read(
                    fixture(dir).join(
                        r.source_file
                            .as_deref()
                            .unwrap()
                            .rsplit('/')
                            .next()
                            .unwrap(),
                    ),
                )
                .unwrap(),
            )
            .unwrap();
            let expected =
                test_case_hash(raw["testCaseId"].as_str(), raw["fullName"].as_str()).unwrap();
            assert_eq!(r.identity.test_case_hash, expected, "{dir} {}", r.name);
            assert!(r.identity.series_key.starts_with(&format!("{expected}.")));
            assert_eq!(r.identity.history_id.as_deref(), raw["historyId"].as_str());
        }
    }
}

#[test]
fn junit5_allure_results() {
    let b = ingest("junit5/allure-results");
    assert_eq!(b.results.len(), 22);
    assert_no_missing_attachments(&b);
    let (statuses, latest, flaky) = group_statuses(&b, "RetryTest.flakyGateway");
    assert_eq!(statuses, vec![Status::Failed, Status::Passed]);
    assert_eq!(latest, Status::Passed);
    assert!(flaky);
    let pay = &by_name(&b, "CheckoutTest.payForCart")[0];
    assert_eq!(
        pay.identity.explicit_id.as_ref().unwrap().source,
        IdSource::AllureId
    );
    assert_eq!(
        pay.identity
            .test_case_id
            .as_deref()
            .map(|s| s.starts_with("[engine:")),
        Some(true)
    );
    let mut http = 0;
    for r in &b.results {
        let mut r = r.clone();
        r.visit_attachments_mut(&mut |a| {
            if a.kind == AttachmentKind::HttpExchange {
                http += 1;
            }
        });
    }
    assert!(http >= 1);
    let discount = by_name(&b, "CheckoutTest.discountApplied");
    assert!(
        discount
            .iter()
            .all(|r| r.status_details.expected.is_some() && r.status_details.actual.is_some())
    );
    assert!(
        b.results
            .iter()
            .all(|r| r.stage == Stage::Finished && !r.interrupted)
    );
}

#[test]
fn surefire_flaky_and_rerun_elements() {
    let b = ingest("junit5/surefire-reports");
    assert_eq!(b.format, Some(SourceFormat::JunitXml));
    assert_eq!(b.results.len(), 22);
    assert!(
        b.launch_meta.attributes.is_empty(),
        "JVM properties are not launch attributes"
    );
    let (statuses, latest, flaky) = group_statuses(&b, "RetryTest#flakyGateway");
    assert_eq!(statuses, vec![Status::Failed, Status::Passed]);
    assert_eq!(latest, Status::Passed);
    assert!(flaky);
    let (statuses, _, flaky) = group_statuses(&b, "RetryTest#cardVaultUnreachable");
    assert_eq!(statuses, vec![Status::Broken; 3]);
    assert!(!flaky);
    let (statuses, _, _) = group_statuses(&b, "RetryTest#refundRejected");
    assert_eq!(statuses, vec![Status::Failed; 3]);
    let flaky_attempt = &by_name(&b, "RetryTest#flakyGateway")[0];
    assert_eq!(flaky_attempt.status_details.expected.as_deref(), Some("OK"));
    assert!(
        flaky_attempt
            .stdout
            .as_ref()
            .unwrap()
            .text
            .contains("attempt 1")
    );
    let params: Vec<_> = by_name(&b, "CheckoutTest#priceOfItem");
    assert_eq!(
        params
            .iter()
            .map(|r| r.identity.series_key.as_str())
            .collect::<HashSet<_>>()
            .len(),
        3
    );
    assert_eq!(params[0].parameters[0].name, "signature");
    let skipped = &by_name(&b, "CheckoutTest#payWithGiftCard")[0];
    assert_eq!(skipped.status, Status::Skipped);
}

#[test]
fn pytest_junit_xunit2_and_xunit1() {
    for f in ["pytest/junit-xunit2.xml", "pytest/junit-xunit1.xml"] {
        let b = ingest(f);
        assert_eq!(b.results.len(), 16, "{f}");
        let (statuses, latest, flaky) = group_statuses(&b, "#test_payment_gateway_flaky");
        assert_eq!(statuses, vec![Status::Failed, Status::Passed], "{f}");
        assert_eq!(latest, Status::Passed);
        assert!(flaky);
        let first = &by_name(&b, "#test_payment_gateway_flaky")[0];
        assert_eq!(first.raw_status.as_deref(), Some("rerun"));
        let (statuses, latest, _) = group_statuses(&b, "#test_refund_always_fails");
        assert_eq!(statuses, vec![Status::Failed; 3]);
        assert_eq!(latest, Status::Failed);
        let xfail = &by_name(&b, "#test_currency_rounding_xfail")[0];
        assert_eq!(
            (xfail.status, xfail.raw_status.as_deref()),
            (Status::Skipped, Some("xfail"))
        );
        let cards = by_name(&b, "#test_card_validation");
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[0].parameters[0].name, "params");
        let broken = &by_name(&b, "#test_order_history_setup_broken")[0];
        assert_eq!(broken.status, Status::Broken);
    }
    let x1 = ingest("pytest/junit-xunit1.xml");
    let r = &by_name(&x1, "#test_refund_always_fails")[0];
    assert_eq!(
        r.identity.source.file.as_deref(),
        Some("tests/test_checkout.py")
    );
    assert!(r.identity.source.line.is_some());
    let x2 = ingest("pytest/junit-xunit2.xml");
    let r = &by_name(&x2, "#test_checkout_passes")[0];
    assert!(
        r.stdout
            .as_ref()
            .is_some_and(|o| o.text.contains("Captured"))
    );
}

#[test]
fn gotestsum_rerun_duplicates() {
    let b = ingest("gotestsum/junit.xml");
    assert_eq!(b.results.len(), 24);
    assert_eq!(
        b.launch_meta
            .attribute("go.version")
            .map(|v| v.starts_with("go")),
        Some(true)
    );
    let (statuses, latest, flaky) = group_statuses(&b, "payments#TestPaymentGatewayFlaky");
    assert_eq!(statuses, vec![Status::Failed, Status::Passed]);
    assert_eq!(latest, Status::Passed);
    assert!(flaky);
    let (statuses, _, flaky) = group_statuses(&b, "payments#TestRefundAlwaysFails");
    assert_eq!(statuses, vec![Status::Failed; 3]);
    assert!(!flaky);
    let d = &by_name(&b, "checkout#TestDiscountApplied")[0];
    assert!(
        d.status_details.message.as_deref().unwrap().contains("got"),
        "real message from body"
    );
    let n = ingest("gotestsum/junit-norerun.xml");
    assert!(n.results.iter().any(|r| {
        r.status_details
            .trace
            .as_deref()
            .is_some_and(|t| t.contains("panic"))
    }));
}

#[test]
fn playwright_json_reporter() {
    let (b, sink) = ingest_with("playwright", Some(SourceFormat::PlaywrightJson));
    assert_eq!(b.format, Some(SourceFormat::PlaywrightJson));
    assert_eq!(b.results.len(), 16);
    assert_no_missing_attachments(&b);
    assert!(b.ignored.iter().any(|i| i.path == "junit.xml"));
    let cart = &by_name(&b, "cart.spec.ts \u{203a} Cart \u{203a} adds item to cart")[0];
    assert_eq!(cart.identity.env_key.as_deref(), Some("staging"));
    assert_eq!(
        cart.identity.full_name,
        "cart.spec.ts \u{203a} Cart \u{203a} adds item to cart"
    );
    assert_eq!(cart.identity.source.file.as_deref(), Some("cart.spec.ts"));
    assert!(
        cart.stdout
            .as_ref()
            .unwrap()
            .text
            .contains("staging.example.com")
    );
    assert!(cart.stderr.is_some());
    let names: Vec<&str> = cart.attachments.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["request.txt", "cart.json", "cart.png"],
        "allure runtime messages are dropped"
    );
    let png = cart
        .attachments
        .iter()
        .find(|a| a.name == "cart.png")
        .unwrap();
    assert!(
        png.resolved_path
            .as_deref()
            .unwrap()
            .starts_with("test-results/")
    );
    let inline = cart
        .attachments
        .iter()
        .find(|a| a.name == "cart.json")
        .unwrap();
    let body = sink.get(inline.id.as_ref().unwrap()).unwrap();
    assert!(serde_json::from_slice::<serde_json::Value>(&body.bytes).is_ok());
    assert!(cart.labels.iter().any(|l| l.name == "issue"));
    assert!(cart.links.iter().any(|l| l.url.contains("SHOP-101")));

    let gateway = by_name(&b, "pays via gateway");
    assert_eq!(gateway.len(), 4);
    let envs: HashSet<_> = gateway
        .iter()
        .map(|r| r.identity.env_key.clone().unwrap())
        .collect();
    assert_eq!(envs.len(), 2);
    let groups: Vec<_> = b
        .retry_groups()
        .into_iter()
        .filter(|g| g.attempts.len() == 2)
        .collect();
    assert_eq!(groups.len(), 2);
    assert!(
        groups
            .iter()
            .all(|g| g.flaky_in_run && b.results[g.latest].status == Status::Passed)
    );
    let rounding = &by_name(&b, "rounding bug")[0];
    assert_eq!(
        (rounding.status, rounding.raw_status.as_deref()),
        (Status::Passed, Some("failed"))
    );
    let discount = by_name(&b, "applies discount code");
    assert_eq!(
        discount
            .iter()
            .map(|r| r.attempt.unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(discount[0].status_details.expected.as_deref(), Some("90"));
}

#[test]
fn playwright_junit_matches_json_series() {
    let json = ingest_with("playwright", Some(SourceFormat::PlaywrightJson)).0;
    let junit = ingest_with("playwright", Some(SourceFormat::JunitXml)).0;
    assert_eq!(junit.results.len(), 12);
    let json_keys: HashSet<&str> = json
        .results
        .iter()
        .map(|r| r.identity.series_key.as_str())
        .collect();
    for r in &junit.results {
        assert!(
            json_keys.contains(r.identity.series_key.as_str()),
            "{}",
            r.identity.full_name
        );
    }
    let cart = &by_name(&junit, "adds item to cart")[0];
    assert_eq!(cart.attachments.len(), 1);
    assert!(cart.attachments[0].id.is_some());
    assert!(cart.labels.iter().any(|l| l.name == "note"));
}

#[test]
fn playwright_ctrf_reporter() {
    let b = ingest_with("playwright", Some(SourceFormat::Ctrf)).0;
    assert_eq!(b.results.len(), 16);
    assert_no_missing_attachments(&b);
    assert_eq!(b.launch_meta.vcs.branch.as_deref(), Some("main"));
    assert_eq!(b.launch_meta.ci.build_order, Some(42));
    let discount = by_name(&b, "applies discount code");
    assert_eq!(
        discount
            .iter()
            .map(|r| r.attempt.unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert!(
        discount
            .iter()
            .all(|r| r.identity.explicit_id.as_ref().unwrap().source == IdSource::CtrfTestId)
    );
    let gateway = by_name(&b, "pays via gateway");
    let envs: HashSet<_> = gateway
        .iter()
        .map(|r| r.identity.env_key.clone().unwrap())
        .collect();
    assert_eq!(
        envs,
        HashSet::from(["staging".to_string(), "production".to_string()])
    );
    let last = gateway.iter().find(|r| r.status == Status::Passed).unwrap();
    assert!(last.status_details.flaky);
}

#[test]
fn allure_playwright_detected_by_default() {
    let b = ingest("playwright");
    assert_eq!(b.format, Some(SourceFormat::AllureResults));
    assert_eq!(b.results.len(), 16);
    assert!(b.ignored.iter().any(|i| i.path == "results.json"));
    assert!(b.unreferenced.iter().any(|u| u.path == "README.md"));
}

#[test]
fn vitest_outputs() {
    let b = ingest("vitest/junit.xml");
    let (statuses, _, flaky) = group_statuses(&b, "Cart > applies discount code");
    assert_eq!(
        statuses,
        vec![Status::Failed; 3],
        "one failure element per attempt"
    );
    assert!(!flaky);
    let todo = by_name(&b, "Checkout > refund flow");
    assert_eq!(todo[0].status, Status::Skipped);

    let a = ingest("vitest/allure-results");
    assert_no_missing_attachments(&a);
    let todo = &by_name(&a, "Checkout refund flow")[0];
    assert_eq!(todo.status, Status::Unknown);
    let tax = &by_name(&a, "Cart calculates tax")[0];
    assert_eq!(tax.status, Status::Broken);
}
