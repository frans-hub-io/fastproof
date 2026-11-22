//! Launch explorer: grouping by any label, the filters shared by the tree and the result list,
//! filter counts, label names and environments. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use common::{ADMIN_TOKEN, Server, TestDb, read};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

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

fn result(id: &str, status: &str, labels: Value) -> Value {
    json!({"id": id, "name": id, "fullName": format!("shop.Test#{id}"), "status": status,
        "start": 1795249075000i64, "stop": 1795249075000i64 + 100, "labels": labels})
}

async fn launch_on(s: &Server, project: &str, branch: &str) -> String {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches"),
            ADMIN_TOKEN,
            json!({"name": "nightly", "vcs": {"branch": branch}}),
        )
        .await;
    assert!(st.is_success(), "{st} {b}");
    b["id"].as_str().unwrap().to_string()
}

async fn finish(s: &Server, project: &str, launch: &str) {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{launch}/finish"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert!(st.is_success(), "{st} {b}");
}

fn child<'a>(node: &'a Value, name: &str) -> &'a Value {
    node["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no {name:?} in {node}"))
}

fn child_names(node: &Value) -> Vec<String> {
    node["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect()
}

fn test_names(tests: &Value) -> Vec<String> {
    let mut v: Vec<String> = tests
        .as_array()
        .unwrap_or_else(|| panic!("no tests: {tests}"))
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    v.sort();
    v
}

fn enc(s: &str) -> String {
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

async fn ids(s: &Server, base: &str, query: &str) -> Vec<String> {
    let (st, b) = s
        .get(&format!("{base}/results?limit=500{query}"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{query}: {b}");
    let mut v: Vec<String> = b["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    v.sort();
    v
}

async fn tree_names(s: &Server, base: &str, query: &str) -> Vec<String> {
    let (st, t) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=owner{query}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{query}: {t}");
    let mut out = Vec::new();
    fn walk(n: &Value, out: &mut Vec<String>) {
        if let Some(ts) = n["tests"].as_array() {
            out.extend(ts.iter().map(|t| t["name"].as_str().unwrap().to_string()));
        }
        for c in n["children"].as_array().unwrap() {
            walk(c, out);
        }
    }
    walk(&t, &mut out);
    out.sort();
    out
}

#[tokio::test]
async fn group_by_any_label_with_none_groups() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("shop").await;
    let lid = launch_on(&s, "shop", "main").await;
    let base = format!("/api/v1/projects/shop/launches/{lid}");
    post_results(
        &s,
        "shop",
        &lid,
        &[
            result("full", "failed", json!([{"name": "epic", "value": "Shop"}, {"name": "feature", "value": "Cart"}, {"name": "story", "value": "Add"}, {"name": "owner", "value": "alice"}])),
            result("nofeature", "passed", json!([{"name": "epic", "value": "Shop"}, {"name": "story", "value": "Add"}, {"name": "owner", "value": "bob"}])),
            result("epiconly", "passed", json!([{"name": "epic", "value": "Shop"}, {"name": "owner", "value": "alice"}])),
            result("bare", "skipped", json!([{"name": "severity", "value": "minor"}])),
            result("featureonly", "broken", json!([{"name": "feature", "value": "Cart"}, {"name": "x.custom", "value": "1"}])),
        ],
    )
    .await;

    let (st, t) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=epic,feature,story"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["counts"]["total"], 5);
    assert_eq!(child_names(&t), vec!["", "Shop"]);
    let shop = child(&t, "Shop");
    assert_eq!(shop["counts"]["total"], 3);
    assert_eq!(test_names(&shop["tests"]), vec!["epiconly"]);
    assert_eq!(
        test_names(&child(child(shop, "Cart"), "Add")["tests"]),
        vec!["full"]
    );
    let none_feature = child(shop, "");
    assert_eq!(none_feature["path"], json!(["Shop", ""]));
    assert_eq!(
        test_names(&child(none_feature, "Add")["tests"]),
        vec!["nofeature"]
    );
    let none = child(&t, "");
    assert_eq!(test_names(&none["tests"]), vec!["bare"]);
    assert_eq!(
        test_names(&child(none, "Cart")["tests"]),
        vec!["featureonly"]
    );

    let path = enc(&json!(["Shop", "", "Add"]).to_string());
    let (st, page) = s
        .get(
            &format!("{base}/tree/tests?groupBy=epic,feature,story&path={path}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{page}");
    assert_eq!(test_names(&page["items"]), vec!["nofeature"]);
    let (_, page) = s
        .get(
            &format!(
                "{base}/tree/tests?groupBy=epic,feature,story&path={}",
                enc(r#"[""]"#)
            ),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(test_names(&page["items"]), vec!["bare"]);

    let (_, owners) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=owner"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(child_names(&owners), vec!["", "alice", "bob"]);
    assert_eq!(child(&owners, "alice")["counts"]["total"], 2);
    assert_eq!(child(&owners, "")["counts"]["total"], 2);

    let (_, dropped) = s
        .get(&format!("{base}/tree?groupBy=epic,nope,owner"), ADMIN_TOKEN)
        .await;
    assert_eq!(
        dropped["levels"],
        json!(["epic", "owner"]),
        "levels no result has are left out"
    );
    assert_eq!(child(child(&dropped, "Shop"), "bob")["counts"]["total"], 1);
    let (_, page) = s
        .get(
            &format!(
                "{base}/tree/tests?groupBy=epic,owner&path={}",
                enc(r#"["Shop","alice"]"#)
            ),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(test_names(&page["items"]), vec!["epiconly", "full"]);

    let (_, single) = s
        .get(&format!("{base}/tree?groupBy=label:feature"), ADMIN_TOKEN)
        .await;
    assert_eq!(child_names(&single), vec!["", "Cart"]);
    let (_, custom) = s
        .get(&format!("{base}/tree?groupBy=x.custom,owner"), ADMIN_TOKEN)
        .await;
    assert_eq!(child_names(&custom), vec!["", "1"]);
    let (_, legacy) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=feature"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(
        legacy["testCount"], 1,
        "the feature preset keeps unlabeled results at the root"
    );

    for bad in [
        "groupBy=a,b,c,d,e,f",
        "groupBy=owner,owner",
        "groupBy=a%20b",
        "groupBy=owner,",
        "groupBy=label:",
    ] {
        let (st, b) = s.get(&format!("{base}/tree?{bad}"), ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}: {b}");
    }
    let long = "n".repeat(65);
    let (st, _) = s
        .get(&format!("{base}/tree?groupBy={long}"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let (st, labels) = s.get(&format!("{base}/labels"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{labels}");
    let items = labels["items"].as_array().unwrap();
    let owner = items.iter().find(|l| l["name"] == "owner").unwrap();
    assert_eq!(owner["count"], 3);
    assert_eq!(owner["values"], 2);
    assert_eq!(items[0]["name"], "epic");
    assert!(items.iter().any(|l| l["name"] == "x.custom"));
}

#[tokio::test]
async fn filters_apply_to_tree_list_and_counts() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("shop").await;
    let prev = launch_on(&s, "shop", "main").await;
    post_results(
        &s,
        "shop",
        &prev,
        &[
            result("old-fail", "failed", json!([])),
            result("new-fail", "passed", json!([])),
        ],
    )
    .await;
    finish(&s, "shop", &prev).await;

    let lid = launch_on(&s, "shop", "main").await;
    let base = format!("/api/v1/projects/shop/launches/{lid}");
    let mut retried = result(
        "retried",
        "failed",
        json!([{"name": "severity", "value": "critical"}]),
    );
    retried["retry"] = json!(0);
    let mut retried2 = result(
        "retried",
        "passed",
        json!([{"name": "severity", "value": "critical"}]),
    );
    retried2["id"] = json!("retried-1");
    retried2["retry"] = json!(1);
    retried2["start"] = json!(1795249076000i64);
    retried2["stop"] = json!(1795249076000i64 + 5000);
    let mut slow = result(
        "slow",
        "passed",
        json!([{"name": "owner", "value": "alice"}, {"name": "tag", "value": "smoke"}, {"name": "tag", "value": "api"}]),
    );
    slow["stop"] = json!(1795249075000i64 + 9000);
    let mut msg = result(
        "old-fail",
        "failed",
        json!([{"name": "severity", "value": "blocker"}, {"name": "owner", "value": "bob"}]),
    );
    msg["error"] = json!({"message": "Connection refused by db-1.example.com"});
    post_results(
        &s,
        "shop",
        &lid,
        &[
            msg,
            result("new-fail", "broken", json!([{"name": "severity", "value": "critical"}, {"name": "tag", "value": "smoke"}])),
            retried,
            retried2,
            slow,
            result("skip", "skipped", json!([])),
        ],
    )
    .await;

    assert_eq!(
        ids(&s, &base, "&severity=critical").await,
        vec!["new-fail", "retried"]
    );
    assert_eq!(
        ids(&s, &base, "&severity=critical,(none)").await,
        vec!["new-fail", "retried", "skip", "slow"]
    );
    assert_eq!(ids(&s, &base, "&tag=api").await, vec!["slow"]);
    assert_eq!(ids(&s, &base, "&tag=smoke&owner=alice").await, vec!["slow"]);
    assert_eq!(ids(&s, &base, "&retried=true").await, vec!["retried"]);
    assert_eq!(
        ids(&s, &base, "&minDurationMs=5000").await,
        vec!["retried", "slow"]
    );
    assert_eq!(
        ids(&s, &base, "&minDurationMs=1000&maxDurationMs=6000").await,
        vec!["retried"]
    );
    assert_eq!(ids(&s, &base, "&message=REFUSED").await, vec!["old-fail"]);
    assert_eq!(ids(&s, &base, "&newFailures=true").await, vec!["new-fail"]);
    assert_eq!(
        tree_names(&s, &base, "&newFailures=true").await,
        vec!["new-fail"]
    );
    assert_eq!(
        tree_names(&s, &base, "&severity=critical&status=broken").await,
        vec!["new-fail"]
    );
    let (_, t) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=severity&retried=true"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(child_names(&t), vec!["critical"]);
    let path = enc(r#"["critical"]"#);
    let (_, page) = s
        .get(
            &format!("{base}/tree/tests?groupBy=severity&path={path}&maxDurationMs=1000"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(test_names(&page["items"]), vec!["new-fail"]);

    let all = s.results("shop", &lid, "").await;
    let id_of = |n: &str| {
        all.iter()
            .find(|r| r["name"] == n)
            .map(|r| r["id"].clone())
            .unwrap()
    };
    let (_, types) = s
        .get("/api/v1/projects/shop/defect-types", ADMIN_TOKEN)
        .await;
    let pb = types["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["group"] == "productBug")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, b) = s
        .post(
            "/api/v1/projects/shop/triage",
            ADMIN_TOKEN,
            json!({"resultIds": [id_of("old-fail")], "defectTypeId": pb}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        ids(&s, &base, &format!("&defect={pb}")).await,
        vec!["old-fail"]
    );
    let untriaged = ids(&s, &base, "&defect=none").await;
    assert!(untriaged.contains(&"slow".to_string()), "{untriaged:?}");
    assert!(!untriaged.contains(&"old-fail".to_string()));
    let (st, b) = s
        .post(
            "/api/v1/projects/shop/triage",
            ADMIN_TOKEN,
            json!({"resultIds": [id_of("skip")], "mute": {"reason": "known"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(ids(&s, &base, "&muted=true").await, vec!["skip"]);

    let (st, f) = s.get(&format!("{base}/facets"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{f}");
    assert_eq!(f["counts"]["total"], 5);
    assert_eq!(f["counts"]["failed"], 1);
    assert_eq!(f["retried"], 1);
    assert_eq!(f["newFailures"], 1);
    assert_eq!(f["baselineLaunchId"], json!(prev));
    assert_eq!(f["automated"], 5);
    assert_eq!(f["counts"]["muted"], 1);
    let sev = f["labels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == "severity")
        .unwrap();
    assert_eq!(sev["values"][0], json!({"value": "critical", "count": 2}));
    assert_eq!(sev["none"], 2);
    let tag = f["labels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == "tag")
        .unwrap();
    assert_eq!(tag["values"][0], json!({"value": "smoke", "count": 2}));
    assert!(
        f["defects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["value"] == pb && d["count"] == 1),
        "{f}"
    );
    assert_eq!(f["maxDurationMs"], 9000);

    for bad in [
        "retried=yes",
        "muted=1",
        "newFailures=maybe",
        "minDurationMs=-1",
        "maxDurationMs=x",
        "defect=nope",
        "attachment=pdf",
    ] {
        let (st, b) = s.get(&format!("{base}/tree?{bad}"), ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}: {b}");
        let (st, _) = s.get(&format!("{base}/results?{bad}"), ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}");
    }
    let many = (0..21)
        .map(|i| format!("v{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let (st, _) = s
        .get(&format!("{base}/tree?severity={many}"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let long = "m".repeat(300);
    let (st, _) = s
        .get(&format!("{base}/tree?message={long}"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = s
        .get(&format!("{base}/facets?env={long}"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn environments_split_a_launch_and_attachment_kinds_filter() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("shop").await;
    let l = s.launch("shop", None).await;
    let lid = l["id"].as_str().unwrap();
    let base = format!("/api/v1/projects/shop/launches/{lid}");
    let done = s
        .ingest(
            "shop",
            lid,
            read("playwright/results.json"),
            "?fileName=results.json",
        )
        .await;
    assert_eq!(done["status"], "done", "{done}");

    let (st, envs) = s.get(&format!("{base}/environments"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{envs}");
    let items = envs["items"].as_array().unwrap();
    let keys: Vec<&str> = items.iter().filter_map(|e| e["key"].as_str()).collect();
    assert!(
        keys.contains(&"staging") && keys.contains(&"production"),
        "{envs}"
    );
    let total: i64 = items
        .iter()
        .map(|e| e["counts"]["total"].as_i64().unwrap())
        .sum();
    let launch = s.launch_get("shop", lid).await;
    assert_eq!(total, launch["counts"]["total"].as_i64().unwrap());

    let staging = items.iter().find(|e| e["key"] == "staging").unwrap();
    let (_, t) = s
        .get(&format!("{base}/tree?env=staging"), ADMIN_TOKEN)
        .await;
    assert_eq!(t["counts"], staging["counts"]);
    let (_, f) = s
        .get(&format!("{base}/facets?env=staging"), ADMIN_TOKEN)
        .await;
    assert_eq!(f["counts"], staging["counts"]);
    let listed = s.results("shop", lid, "&env=staging").await;
    assert_eq!(
        listed.len() as i64,
        staging["counts"]["total"].as_i64().unwrap()
    );
    let (_, none) = s.get(&format!("{base}/tree?env=(none)"), ADMIN_TOKEN).await;
    assert_eq!(none["counts"]["total"], 0);
    let (_, labels) = s
        .get(&format!("{base}/labels?env=production"), ADMIN_TOKEN)
        .await;
    assert!(labels["items"].as_array().is_some());

    let (_, f) = s.get(&format!("{base}/facets"), ADMIN_TOKEN).await;
    let kinds: Vec<(String, i64)> = f["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["value"].as_str().unwrap().to_string(),
                a["count"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        kinds.iter().map(|k| k.0.as_str()).collect::<Vec<_>>(),
        vec!["trace", "video", "screenshot"]
    );
    assert!(kinds.iter().any(|k| k.1 > 0), "{f}");
    for (kind, count) in &kinds {
        let listed = s.results("shop", lid, &format!("&attachment={kind}")).await;
        assert_eq!(listed.len() as i64, *count, "{kind}");
        let (_, t) = s
            .get(&format!("{base}/tree?attachment={kind}"), ADMIN_TOKEN)
            .await;
        assert_eq!(t["counts"]["total"], *count, "{kind}");
    }
}

#[tokio::test]
async fn explorer_routes_check_access() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("shop").await;
    s.project("other").await;
    let l = s.launch("shop", None).await;
    let lid = l["id"].as_str().unwrap();
    let theirs = s.launch("other", None).await;
    let tid = theirs["id"].as_str().unwrap();
    let outsider = s.token("outsider", "viewer", &["other"]).await;
    let viewer = s.token("viewer", "viewer", &["shop"]).await;
    for route in [
        "labels",
        "facets",
        "environments",
        "tree?groupBy=owner&severity=x",
    ] {
        let path = format!("/api/v1/projects/shop/launches/{lid}/{route}");
        let (st, _) = s.get(&path, &viewer).await;
        assert_eq!(st, StatusCode::OK, "{route}");
        let (st, _) = s.get(&path, &outsider).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{route}");
        let r = s.req(Method::GET, &path, None).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "{route}");
        let (st, _) = s
            .get(
                &format!("/api/v1/projects/shop/launches/{tid}/{route}"),
                ADMIN_TOKEN,
            )
            .await;
        assert_eq!(
            st,
            StatusCode::NOT_FOUND,
            "another project's launch: {route}"
        );
    }
}
