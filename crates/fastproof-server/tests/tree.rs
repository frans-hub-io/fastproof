//! The launch tree with test leaves, its filters and the paged tests of one group.
//! Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const PW: &str = " \u{203a} ";

async fn post_results(s: &Server, project: &str, launch: &str, results: &[Value]) {
    let body: String = results
        .iter()
        .map(|r| r.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/{project}/launches/{launch}/results"),
            Some(ADMIN_TOKEN),
        )
        .header("content-type", "application/x-ndjson")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    let out: Value = r.json().await.unwrap();
    assert_eq!(out["accepted"], results.len(), "{out}");
}

fn result(id: &str, name: &str, full_name: &str, status: &str, labels: Value) -> Value {
    json!({"id": id, "name": name, "fullName": full_name, "status": status,
        "start": 1795249075000i64, "stop": 1795249075250i64, "labels": labels})
}

fn find<'a>(node: &'a Value, path: &[&str]) -> &'a Value {
    let mut n = node;
    for p in path {
        n = n["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == *p)
            .unwrap_or_else(|| panic!("no {p} in {n}"));
    }
    n
}

fn names(tests: &Value) -> Vec<&str> {
    tests
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn tree_with_tests_filters_and_pages() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("shop").await;
    let l = s.launch("shop", None).await;
    let lid = l["id"].as_str().unwrap();
    let base = format!("/api/v1/projects/shop/launches/{lid}");
    let checkout = format!("e2e/checkout.spec.ts{PW}Checkout{PW}Card");
    post_results(
        &s,
        "shop",
        lid,
        &[
            result("pw1", &format!("Checkout{PW}Card{PW}pays"), &format!("{checkout}{PW}pays"), "failed",
                json!([{"name": "epic", "value": "Shop"}, {"name": "feature", "value": "Checkout"}, {"name": "story", "value": "Pay"}])),
            result("pw2", &format!("Checkout{PW}Card{PW}refunds"), &format!("{checkout}{PW}refunds"), "passed",
                json!([{"name": "epic", "value": "Shop"}, {"name": "feature", "value": "Checkout"}])),
            result("pw3", &format!("Checkout{PW}shows totals"), &format!("e2e/checkout.spec.ts{PW}Checkout{PW}shows totals"), "skipped", json!([])),
            result("j1", "creates an order", "com.example.OrderTest#creates an order", "broken",
                json!([{"name": "suite", "value": "com.example.OrderTest"}])),
            result("j2", "cancels an order", "com.example.OrderTest#cancels an order", "passed",
                json!([{"name": "suite", "value": "com.example.OrderTest"}])),
            result("a1", "logs in", "tests.test_auth#logs_in", "passed",
                json!([{"name": "parentSuite", "value": "tests"}, {"name": "suite", "value": "test_auth"}, {"name": "subSuite", "value": "Login"}])),
        ],
    )
    .await;

    let (st, tree) = s
        .get(
            &format!("{base}/tree?groupBy=suite&includeTests=true"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{tree}");
    assert_eq!(tree["counts"]["total"], 6);
    assert_eq!(tree["durationMs"], 1500);
    assert!(!tree.to_string().contains("resultIds"), "{tree}");
    let card = find(&tree, &["e2e/checkout.spec.ts", "Checkout", "Card"]);
    assert_eq!(card["testCount"], 2);
    assert_eq!(names(&card["tests"]), vec!["pays", "refunds"]);
    assert_eq!(card["tests"][0]["status"], "failed");
    assert_eq!(card["tests"][0]["durationMs"], 250);
    let describe = find(&tree, &["e2e/checkout.spec.ts", "Checkout"]);
    assert_eq!(describe["counts"]["total"], 3);
    assert_eq!(describe["counts"]["failed"], 1);
    assert_eq!(describe["counts"]["skipped"], 1);
    assert_eq!(names(&describe["tests"]), vec!["shows totals"]);
    let order = find(&tree, &["com.example", "OrderTest"]);
    assert_eq!(
        names(&order["tests"]),
        vec!["cancels an order", "creates an order"]
    );
    let login = find(&tree, &["tests", "test_auth", "Login"]);
    assert_eq!(names(&login["tests"]), vec!["logs in"]);

    let (_, features) = s
        .get(
            &format!("{base}/tree?groupBy=feature&includeTests=true"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(
        names(&find(&features, &["Shop", "Checkout", "Pay"])["tests"]),
        vec!["Checkout \u{203a} Card \u{203a} pays"]
    );
    assert_eq!(
        features["testCount"], 4,
        "results without epic, feature or story sit at the root"
    );

    let (_, failed) = s
        .get(
            &format!("{base}/tree?groupBy=suite&includeTests=true&status=failed,broken"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(failed["counts"]["total"], 2);
    let top: Vec<&str> = failed["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        top,
        vec!["com.example", "e2e/checkout.spec.ts"],
        "ancestors of matches stay"
    );
    assert_eq!(
        names(&find(&failed, &["e2e/checkout.spec.ts", "Checkout", "Card"])["tests"]),
        vec!["pays"]
    );
    assert!(find(&failed, &["e2e/checkout.spec.ts", "Checkout"])["tests"].is_null());

    let (_, q) = s
        .get(
            &format!("{base}/tree?includeTests=true&q=ORDERTEST"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(q["counts"]["total"], 2);

    let path = urlencoding(&json!(["e2e/checkout.spec.ts", "Checkout", "Card"]).to_string());
    let (st, p1) = s
        .get(
            &format!("{base}/tree/tests?path={path}&limit=1"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{p1}");
    assert_eq!(names(&p1["items"]), vec!["pays"]);
    let next = p1["next"].as_str().unwrap();
    let (_, p2) = s
        .get(
            &format!("{base}/tree/tests?path={path}&limit=1&before={next}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(names(&p2["items"]), vec!["refunds"]);
    assert!(p2["next"].is_null());
    let (_, root) = s
        .get(
            &format!("{base}/tree/tests?groupBy=feature&path=%5B%5D"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(root["items"].as_array().unwrap().len(), 4);
    let order_path = urlencoding(&json!(["com.example", "OrderTest"]).to_string());
    let (_, o) = s
        .get(
            &format!("{base}/tree/tests?path={order_path}&status=broken"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(names(&o["items"]), vec!["creates an order"]);

    for bad in [
        "tree/tests?path=nope",
        "tree/tests?path=%7B%7D",
        "tree/tests",
        "tree/tests?path=%5B%5D&groupBy=x%20y",
        "tree/tests?path=%5B%5D&status=lost",
        "tree/tests?path=%5B%5D&limit=501",
        "tree?status=lost",
    ] {
        let (st, b) = s.get(&format!("{base}/{bad}"), ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}: {b}");
    }
    let deep = urlencoding(&serde_json::to_string(&vec!["x"; 65]).unwrap());
    let (st, _) = s
        .get(&format!("{base}/tree/tests?path={deep}"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let long = "q".repeat(300);
    let (st, _) = s.get(&format!("{base}/tree?q={long}"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    s.project("other").await;
    let outsider = s.token("outsider", "viewer", &["other"]).await;
    let viewer = s.token("viewer", "viewer", &["shop"]).await;
    let (st, _) = s
        .get(&format!("{base}/tree/tests?path=%5B%5D"), &outsider)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .get(&format!("{base}/tree/tests?path=%5B%5D"), &viewer)
        .await;
    assert_eq!(st, StatusCode::OK);
    let r = s
        .req(Method::GET, &format!("{base}/tree/tests?path=%5B%5D"), None)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let (st, _) = s
        .get(
            "/api/v1/projects/shop/launches/00000000-0000-0000-0000-000000000000/tree/tests?path=%5B%5D",
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn big_groups_are_paged_and_a_revealed_group_is_inlined() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("big").await;
    let l = s.launch("big", None).await;
    let lid = l["id"].as_str().unwrap();
    let base = format!("/api/v1/projects/big/launches/{lid}");
    let per = fastproof_store::results::MAX_TREE_INLINE / 5 - 100;
    for g in 0..6 {
        let batch: Vec<Value> = (0..per)
            .map(|i| {
                let status = if g == 3 && i % 100 == 7 {
                    "failed"
                } else {
                    "passed"
                };
                result(
                    &format!("g{g}-{i}"),
                    &format!("t{i:05}"),
                    &format!("big.G{g}#t{i:05}"),
                    status,
                    json!([]),
                )
            })
            .collect();
        post_results(&s, "big", lid, &batch).await;
    }
    let inlined = |tree: &Value| -> Vec<usize> {
        (0..6)
            .filter(|g| !find(tree, &["big", &format!("G{g}")])["tests"].is_null())
            .collect()
    };

    let (_, tree) = s
        .get(&format!("{base}/tree?includeTests=true"), ADMIN_TOKEN)
        .await;
    assert_eq!(tree["counts"]["total"], 6 * per);
    assert_eq!(find(&tree, &["big", "G5"])["testCount"], per);
    assert_eq!(
        inlined(&tree),
        vec![0, 1, 2, 3, 4],
        "the group with failures first, then by path"
    );

    let path = urlencoding(&json!(["big", "G5"]).to_string());
    let mut seen = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let mut url = format!("{base}/tree/tests?path={path}&limit=500");
        if let Some(b) = &before {
            url.push_str(&format!("&before={b}"));
        }
        let (st, page) = s.get(&url, ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::OK, "{page}");
        seen.extend(names(&page["items"]).into_iter().map(str::to_string));
        match page["next"].as_str() {
            Some(c) => before = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(seen.len(), per);
    assert_eq!(seen.first().map(String::as_str), Some("t00000"));

    let results = s.results("big", lid, "&q=G5%23t00042").await;
    let id = results[0]["id"].as_str().unwrap();
    let (_, revealed) = s
        .get(
            &format!("{base}/tree?includeTests=true&reveal={id}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(
        inlined(&revealed),
        vec![0, 1, 2, 3, 5],
        "the revealed group goes first"
    );

    let (_, failed) = s
        .get(
            &format!("{base}/tree?includeTests=true&status=failed"),
            ADMIN_TOKEN,
        )
        .await;
    let failing = (0..per).filter(|i| i % 100 == 7).count();
    assert_eq!(failed["counts"]["total"], failing);
    assert_eq!(
        find(&failed, &["big", "G3"])["tests"]
            .as_array()
            .unwrap()
            .len(),
        failing
    );
}

fn urlencoding(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
