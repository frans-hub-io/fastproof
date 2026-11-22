//! Test-management fixes over HTTP: `ALLURE_ID` on a test that is already linked moves it to
//! the case the id names (the auto-created duplicate is archived only when nothing else holds
//! it, audited as `case.linkIdentity` with `source: code`); code values never wipe a case and
//! a manual case keeps its text when code automates it; a plan launch joined by a CI token with
//! `cases:sync` is trusted by code sync (on close too); closing a manual run on a finished
//! launch notifies `manualRunClosed` only; `identitiesTotal`; the whole folder tree in one
//! answer; byte limits named in bytes.

mod common;

use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

async fn send(
    s: &Server,
    method: Method,
    path: &str,
    token: &str,
    if_match: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut rb = s.req(method, path, Some(token));
    if let Some(m) = if_match {
        rb = rb.header("if-match", m);
    }
    if let Some(b) = body {
        rb = rb.json(&b);
    }
    let r = rb.send().await.unwrap();
    let status = r.status();
    let text = r.text().await.unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

async fn create(s: &Server, key: &str, body: Value) -> i64 {
    let (st, b) = s
        .post(&format!("/api/v1/projects/{key}/cases"), ADMIN_TOKEN, body)
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    b["number"].as_i64().unwrap()
}

async fn case(s: &Server, key: &str, n: i64) -> Value {
    let (st, b) = s
        .get(&format!("/api/v1/projects/{key}/cases/{n}"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    b
}

fn result(id: &str, name: &str, full: &str, labels: Value) -> Value {
    json!({"id": id, "name": name, "fullName": full, "status": "passed", "start": 1795249075967i64,
        "stop": 1795249076967i64, "labels": labels})
}

async fn post_results(s: &Server, key: &str, token: &str, launch: &str, results: Value) {
    let (st, out) = s
        .post(
            &format!("/api/v1/projects/{key}/launches/{launch}/results"),
            token,
            results,
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
}

async fn run_launch(s: &Server, key: &str, token: &str, results: Value) -> String {
    let (st, l) = s
        .post(
            &format!("/api/v1/projects/{key}/launches"),
            token,
            json!({"name": "ci", "vcs": {"branch": "main"}}),
        )
        .await;
    assert!(st.is_success(), "{st} {l}");
    let lid = l["id"].as_str().unwrap().to_string();
    post_results(s, key, token, &lid, results).await;
    let (st, f) = s
        .post(
            &format!("/api/v1/projects/{key}/launches/{lid}/finish"),
            token,
            json!({}),
        )
        .await;
    assert!(st.is_success(), "{st} {f}");
    lid
}

/// One round of the `cases.sync` job as the scheduler runs it (with the audit records).
async fn sync(s: &Server) -> fastproof_store::tm::sync::SyncStats {
    fastproof_server::scheduler::case_sync(
        &s.running.as_ref().unwrap().state,
        Duration::from_secs(60),
    )
    .await
    .unwrap()
}

/// The case number each code identity is linked to, by full name.
async fn linked(s: &Server, key: &str, full_name: &str) -> Option<i64> {
    let (st, b) = s
        .get(
            &format!("/api/v1/projects/{key}/test-identities?limit=500"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    b["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["fullName"] == full_name)
        .unwrap_or_else(|| panic!("no identity {full_name}: {b}"))["caseNumber"]
        .as_i64()
}

async fn wait_for<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(60);
    while !f().await {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn allure_id_on_a_linked_test_moves_it_to_the_named_case() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["cases.sync", "cases.backfill"]).await;
    s.project("ai").await;
    let up = s.token("ci", "uploader", &["ai"]).await;
    let card = create(
        &s,
        "ai",
        json!({"name": "Pay by card (manual)",
        "body": {"description": "Written by QA", "steps": [{"action": "Open checkout"}]},
        "tags": ["manual"], "fields": {"severity": "minor"}}),
    )
    .await;
    let refund = create(&s, "ai", json!({"name": "Refund (manual)", "body": {}})).await;
    let wallet = create(&s, "ai", json!({"name": "Wallet (manual)", "body": {}})).await;
    let manual_steps = case(&s, "ai", card).await["body"]["steps"].clone();

    let suite = || {
        json!([{"name": "suite", "value": "Pay"}, {"name": "tag", "value": "smoke"},
        {"name": "severity", "value": "critical"}])
    };
    run_launch(
        &s,
        "ai",
        &up,
        json!([
            result("r1", "pays by card", "shop.Pay#card", suite()),
            result("r2", "refunds", "shop.Pay#refund", suite()),
            result("r3", "pays by wallet", "shop.Pay#wallet", suite()),
        ]),
    )
    .await;
    let st = sync(&s).await;
    assert_eq!(st.created, 3, "{st:?}");
    let auto_card = linked(&s, "ai", "shop.Pay#card").await.unwrap();
    let auto_refund = linked(&s, "ai", "shop.Pay#refund").await.unwrap();
    let auto_wallet = linked(&s, "ai", "shop.Pay#wallet").await.unwrap();
    assert!(auto_card > wallet && auto_refund > wallet && auto_wallet > wallet);

    let r = case(&s, "ai", auto_refund).await;
    let (st, b) = send(
        &s,
        Method::PATCH,
        &format!("/api/v1/projects/ai/cases/{auto_refund}"),
        ADMIN_TOKEN,
        Some(&format!("\"r{}\"", r["revision"])),
        Some(json!({"fields": {"priority": "high"}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "an edit in the UI: {b}");
    let manual_launch = s.launch("ai", Some("manual-1")).await;
    let (st, b) = s
        .post(
            &format!(
                "/api/v1/projects/ai/launches/{}/run-items",
                manual_launch["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
            json!({"caseNumbers": [auto_wallet]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "a manual run of the code case: {b}");

    let ids = move || {
        json!([
            result(
                "r1",
                "pays by card",
                "shop.Pay#card",
                json!([{"name": "ALLURE_ID", "value": card.to_string()}, {"name": "suite", "value": "Pay"},
                    {"name": "tag", "value": "smoke"}, {"name": "severity", "value": "critical"}])
            ),
            result(
                "r2",
                "refunds",
                "shop.Pay#refund",
                json!([{"name": "ALLURE_ID", "value": refund.to_string()}, {"name": "suite", "value": "Pay"}])
            ),
            result(
                "r3",
                "pays by wallet",
                "shop.Pay#wallet",
                json!([{"name": "ALLURE_ID", "value": wallet.to_string()}, {"name": "suite", "value": "Pay"}])
            ),
        ])
    };
    run_launch(&s, "ai", &up, ids()).await;
    let st = sync(&s).await;
    assert_eq!((st.relinked, st.created), (3, 0), "{st:?}");
    for (full, n) in [
        ("shop.Pay#card", card),
        ("shop.Pay#refund", refund),
        ("shop.Pay#wallet", wallet),
    ] {
        assert_eq!(linked(&s, "ai", full).await, Some(n), "{full}");
    }

    let c = case(&s, "ai", card).await;
    assert_eq!(c["automated"], true);
    assert_eq!(c["name"], "Pay by card (manual)", "the manual name wins");
    assert_eq!(c["body"]["description"], "Written by QA");
    assert_eq!(c["body"]["steps"], manual_steps);
    assert_eq!(c["tags"], json!(["manual"]));
    assert_eq!(c["fields"]["severity"], "minor", "the manual value wins");
    assert_eq!(
        c["fields"]["suite"], "Pay",
        "empty fields are filled from code"
    );
    assert_eq!(c["identitiesTotal"], 2);
    assert!(
        c["identities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["fullName"] == "shop.Pay#card")
    );
    let (_, v) = s
        .get(
            &format!("/api/v1/projects/ai/cases/{card}/versions"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(v["items"][0]["source"], "code");

    let dup = case(&s, "ai", auto_card).await;
    assert!(
        dup["archivedAt"].is_string(),
        "auto-created duplicate archived: {dup}"
    );
    assert_eq!(dup["automated"], false);
    assert_eq!(
        dup["body"]["links"].as_array().unwrap().last().unwrap()["url"],
        format!("/p/ai/cases/{card}")
    );
    for (n, why) in [
        (auto_refund, "edited in the UI"),
        (auto_wallet, "has a manual run"),
    ] {
        let k = case(&s, "ai", n).await;
        assert!(k["archivedAt"].is_null(), "{why}: kept {k}");
        assert_eq!(k["automated"], false, "{why}: no code identity left");
    }
    assert_eq!(case(&s, "ai", refund).await["automated"], true);
    assert_eq!(case(&s, "ai", wallet).await["automated"], true);

    run_launch(&s, "ai", &up, ids()).await;
    let st = sync(&s).await;
    assert_eq!((st.relinked, st.created), (0, 0), "settled: {st:?}");

    let (_, a) = s.get("/api/v1/audit?limit=500", ADMIN_TOKEN).await;
    let relinks: Vec<&Value> = a["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["action"] == "case.linkIdentity")
        .collect();
    assert_eq!(relinks.len(), 3, "{relinks:?}");
    for r in &relinks {
        assert_eq!(r["detail"]["source"], "code", "{r}");
        assert_eq!(r["principal"], "code-sync", "{r}");
        assert_eq!(r["scope"], "ai", "{r}");
    }
    let card_audit = relinks
        .iter()
        .find(|r| r["detail"]["number"] == card)
        .unwrap();
    assert_eq!(card_audit["detail"]["previousNumber"], auto_card);
    assert_eq!(card_audit["detail"]["archivedDuplicate"], auto_card);
}

#[tokio::test]
async fn code_values_never_wipe_the_case() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["cases.sync", "cases.backfill"]).await;
    s.project("nw").await;
    let up = s.token("ci", "uploader", &["nw"]).await;
    let mut full = result(
        "r1",
        "adds",
        "shop.Cart#adds",
        json!([{"name": "tag", "value": "smoke"}, {"name": "epic", "value": "Shop"}]),
    );
    full["description"] = json!("From code");
    run_launch(&s, "nw", &up, json!([full])).await;
    sync(&s).await;
    let n = linked(&s, "nw", "shop.Cart#adds").await.unwrap();
    let c = case(&s, "nw", n).await;
    assert_eq!(
        (&c["body"]["description"], &c["tags"], &c["fields"]["epic"]),
        (&json!("From code"), &json!(["smoke"]), &json!("Shop"))
    );
    run_launch(
        &s,
        "nw",
        &up,
        json!([result(
            "r1",
            "adds to the cart",
            "shop.Cart#adds",
            json!([])
        )]),
    )
    .await;
    let st = sync(&s).await;
    assert_eq!(st.updated, 1, "the name changed: {st:?}");
    let c = case(&s, "nw", n).await;
    assert_eq!(c["name"], "adds to the cart");
    assert_eq!(
        c["body"]["description"], "From code",
        "no description from code"
    );
    assert_eq!(c["tags"], json!(["smoke"]), "no tags from code");
    assert_eq!(c["fields"]["epic"], "Shop", "no epic label");
}

#[tokio::test]
async fn a_plan_launch_joined_by_ci_is_synced_on_close() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["cases.sync", "cases.backfill"]).await;
    s.project("pl").await;
    let tester = s.token("t", "tester", &["pl"]).await;
    let up = s.token("ci", "uploader", &["pl"]).await;
    let manual = create(
        &s,
        "pl",
        json!({"name": "Checkout by hand", "body": {"steps": [{"action": "Pay"}]}}),
    )
    .await;
    let (st, plan) = s
        .post(
            "/api/v1/projects/pl/test-plans",
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "Release", "caseNumbers": [manual]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{plan}");
    let (st, started) = s
        .post(
            &format!(
                "/api/v1/projects/pl/test-plans/{}/launches",
                plan["id"].as_str().unwrap()
            ),
            &tester,
            json!({"name": "Release run", "externalId": "rel-1", "assign": "none", "includeAutomated": true}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{started}");
    let lid = started["launch"]["id"].as_str().unwrap().to_string();
    let flags = || async {
        sqlx::query_as::<_, (bool, Option<String>, Option<Value>)>(
            "select created_by_can_sync, branch, vcs from launches where id = $1::uuid",
        )
        .bind(&lid)
        .fetch_one(&db.pool)
        .await
        .unwrap()
    };
    assert!(!flags().await.0, "a tester has no cases:sync");

    let (st, joined) = s
        .post(
            "/api/v1/projects/pl/launches",
            &up,
            json!({"name": "ci", "externalId": "rel-1",
                "vcs": {"branch": "main", "repository": "https://git.example.com/shop", "commit": "0123456789abcdef"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "joins the plan launch: {joined}");
    assert_eq!(joined["id"], lid.as_str());
    assert_eq!(joined["vcs"]["branch"], "main");
    let (can, branch, vcs) = flags().await;
    assert!(can, "a sync-capable uploader contributed");
    assert_eq!(branch.as_deref(), Some("main"));
    assert_eq!(vcs.unwrap()["commit"], "0123456789abcdef");
    let (st, _) = s
        .post(
            "/api/v1/projects/pl/launches",
            &up,
            json!({"name": "ci", "externalId": "rel-1", "vcs": {"branch": "feature/x"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        flags().await.1.as_deref(),
        Some("main"),
        "a branch, once set, stays"
    );

    post_results(
        &s,
        "pl",
        &up,
        &lid,
        json!([result("r1", "pays", "shop.Checkout#pays", json!([]))]),
    )
    .await;
    wait_for("automated result", || {
        let l = lid.clone();
        let s = &s;
        async move { !s.results("pl", &l, "&source=automated").await.is_empty() }
    })
    .await;
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/pl/launches/{lid}/manual/close"),
            &tester,
            json!({"untestedTo": "skipped"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let queued: i64 =
        sqlx::query_scalar("select count(*) from case_sync_queue where launch_id = $1::uuid")
            .bind(&lid)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(queued, 1, "closing the manual run queues code sync");
    let st = sync(&s).await;
    assert_eq!(st.created, 1, "the plan launch is trusted: {st:?}");
    assert!(linked(&s, "pl", "shop.Checkout#pays").await.is_some());

    let other = s.launch("pl", Some("plain-1")).await;
    sqlx::query("update launches set created_by_can_sync = false where id = $1::uuid")
        .bind(other["id"].as_str().unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
    let (st, j) = s
        .post(
            "/api/v1/projects/pl/launches",
            &up,
            json!({"name": "ci", "externalId": "plain-1", "vcs": {"branch": "main"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let can: bool =
        sqlx::query_scalar("select created_by_can_sync from launches where id = $1::uuid")
            .bind(j["id"].as_str().unwrap())
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        !can,
        "launches not started from a plan keep their creator's trust"
    );
}

#[tokio::test]
async fn closing_a_manual_run_on_a_finished_launch_notifies_only_that() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_cfg(&db, &[], |c| {
        c.integrations.poll = Duration::from_millis(100);
        c.integrations.settle = Duration::ZERO;
    })
    .await;
    s.project("nt").await;
    let p: Uuid = sqlx::query_scalar("select id from projects where key = 'nt'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    fastproof_store::Store::new(db.pool.clone())
        .merge_project_settings(
            p,
            json!({"gates": [{"name": "g", "rules": [{"metric": "failed", "op": "lte", "value": 0}]}],
                "notifications": [{"id": "hook", "kind": "webhook", "target": "https://hooks.example.com/fp",
                    "on": ["launchFinished", "gateFailed", "newFailures", "manualRunClosed"]}]}),
        )
        .await
        .unwrap();
    let n = create(&s, "nt", json!({"name": "By hand", "body": {}})).await;
    let l = s.launch("nt", Some("ci-1")).await;
    let lid = l["id"].as_str().unwrap().to_string();
    post_results(
        &s,
        "nt",
        ADMIN_TOKEN,
        &lid,
        json!([result("r1", "api", "suite.api", json!([]))]),
    )
    .await;
    wait_for("automated result", || {
        let l = lid.clone();
        let s = &s;
        async move { !s.results("nt", &l, "").await.is_empty() }
    })
    .await;
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/nt/launches/{lid}/run-items"),
            ADMIN_TOKEN,
            json!({"caseNumbers": [n]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/nt/launches/{lid}/finish"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let events = || async {
        sqlx::query_scalar::<_, Vec<String>>(
            "select events from integration_deliveries where launch_id = $1::uuid order by created_at, id",
        )
        .bind(&lid)
        .fetch_all(&db.pool)
        .await
        .unwrap()
    };
    wait_for("the finish is notified", || async {
        events().await.len() == 1
    })
    .await;
    assert_eq!(events().await[0], vec!["launchFinished"]);

    let (_, items) = s
        .get(
            &format!("/api/v1/projects/nt/launches/{lid}/run-items"),
            ADMIN_TOKEN,
        )
        .await;
    let item = items["items"][0]["id"].as_str().unwrap().to_string();
    let (_, it) = s
        .get(
            &format!("/api/v1/projects/nt/run-items/{item}"),
            ADMIN_TOKEN,
        )
        .await;
    let (st, b) = send(
        &s,
        Method::PATCH,
        &format!("/api/v1/projects/nt/run-items/{item}"),
        ADMIN_TOKEN,
        Some(&format!("\"r{}\"", it["revision"])),
        Some(json!({"status": "failed"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/nt/launches/{lid}/manual/close"),
            ADMIN_TOKEN,
            json!({"untestedTo": "skipped"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    wait_for("the close is notified", || async {
        events().await.len() == 2
    })
    .await;
    assert_eq!(
        events().await[1],
        vec!["manualRunClosed"],
        "no second launchFinished (nor gateFailed) for the same launch"
    );
    let gate: String =
        sqlx::query_scalar("select status from launch_gates where launch_id = $1::uuid")
            .bind(&lid)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        gate, "failed",
        "the gate is re-evaluated with the failed manual case"
    );
}

#[tokio::test]
async fn folder_tree_in_one_answer_identities_total_and_byte_limits() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_TM_MAX_ATTACHMENT_BYTES", "16")]).await;
    s.project("ft").await;
    s.project("other").await;
    let base = "/api/v1/projects/ft/case-folders";
    let folder = |name: &str, parent: Option<&str>| {
        let mut b = json!({"name": name});
        if let Some(p) = parent {
            b["parentId"] = json!(p);
        }
        b
    };
    let mut ids = Vec::new();
    let (_, top) = s.post(base, ADMIN_TOKEN, folder("Top", None)).await;
    let top = top["id"].as_str().unwrap().to_string();
    let (_, second) = s.post(base, ADMIN_TOKEN, folder("Another", None)).await;
    ids.push(second["id"].as_str().unwrap().to_string());
    let (_, mid) = s.post(base, ADMIN_TOKEN, folder("Mid", Some(&top))).await;
    let mid = mid["id"].as_str().unwrap().to_string();
    let (_, leaf) = s.post(base, ADMIN_TOKEN, folder("Leaf", Some(&mid))).await;
    let leaf = leaf["id"].as_str().unwrap().to_string();
    create(
        &s,
        "ft",
        json!({"name": "deep", "folderId": leaf, "body": {}}),
    )
    .await;
    create(
        &s,
        "ft",
        json!({"name": "mid", "folderId": mid, "body": {}}),
    )
    .await;

    let (st, all) = s.get(&format!("{base}?all=true"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{all}");
    let names: Vec<&str> = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["Top", "Another", "Mid", "Leaf"],
        "parents first"
    );
    assert_eq!(all["truncated"], false);
    let by = |n: &str| {
        all["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == n)
            .unwrap()
            .clone()
    };
    assert_eq!(by("Top")["counts"]["total"], 2, "subtree counts");
    assert_eq!(by("Mid")["counts"]["total"], 2);
    assert_eq!(by("Leaf")["counts"]["total"], 1);
    assert_eq!(by("Another")["counts"]["total"], 0);
    assert_eq!(by("Top")["hasChildren"], true);
    assert_eq!(by("Leaf")["hasChildren"], false);
    assert_eq!(by("Leaf")["parentId"], mid.as_str());
    let (_, level) = s.get(&format!("{base}?parent={mid}"), ADMIN_TOKEN).await;
    assert_eq!(level["items"][0], by("Leaf"), "same view as the level list");
    assert!(level.get("truncated").is_none());
    let (st, _) = s
        .get(&format!("{base}?all=true&parent={mid}"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, other) = s
        .get("/api/v1/projects/other/case-folders?all=true", ADMIN_TOKEN)
        .await;
    assert_eq!((st, other["items"].clone()), (StatusCode::OK, json!([])));
    let viewer = s.token("v", "viewer", &["other"]).await;
    let (st, _) = s.get(&format!("{base}?all=true"), &viewer).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let _ = ids;

    let n = create(&s, "ft", json!({"name": "many tests", "body": {}})).await;
    let p: Uuid = sqlx::query_scalar("select id from projects where key = 'ft'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into test_cases (id, project_id, test_case_hash, full_name, display_name, kind, case_id, \
           first_seen_at, last_seen_at) \
         select gen_random_uuid(), $1, md5('t' || g), 'shop.T#t' || g, 't' || g, 'code', c.id, now(), now() \
         from generate_series(1, 120) g, cases c where c.project_id = $1 and c.number = $2",
    )
    .bind(p)
    .bind(n)
    .execute(&db.pool)
    .await
    .unwrap();
    let c = case(&s, "ft", n).await;
    assert_eq!(c["identities"].as_array().unwrap().len(), 100);
    assert_eq!(
        c["identitiesTotal"], 121,
        "120 code identities and the manual one"
    );

    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/ft/cases/{n}/attachments"),
            Some(ADMIN_TOKEN),
        )
        .header("content-type", "text/plain")
        .body("x".repeat(64))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let e: Value = r.json().await.unwrap();
    assert!(
        e["message"].as_str().unwrap().ends_with("(max 16 bytes)"),
        "{e}"
    );
}
