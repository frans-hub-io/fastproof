//! Analysis on a real PostgreSQL: fingerprints, clusters, triage and its permissions, defect
//! types, similar failures, rules, flaky detection and transitions, comparison, dashboard,
//! rollup, retention, test plans. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use common::{ADMIN_TOKEN, Server, TestDb};
use fastproof_store::Store;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const JAVA_A: &str = "java.io.IOException: refused\n\tat com.example.vault.VaultClient.connect(VaultClient.java:42)\n\tat com.example.CheckoutTest.pay(CheckoutTest.java:17)\n\tat org.junit.platform.Runner.run(Runner.java:1)";
const JAVA_B: &str = "java.lang.NullPointerException\n\tat com.example.cart.Cart.total(Cart.java:88)\n\tat com.example.CartTest.sum(CartTest.java:9)";
const JAVA_C: &str = "java.io.IOException\n\tat com.example.db.Pool.open(Pool.java:5)\n\tat com.example.ReportTest.load(ReportTest.java:3)";

fn res(id: &str, name: &str, status: &str, msg: Option<&str>, trace: Option<&str>) -> Value {
    let mut v = json!({"id": id, "name": name, "fullName": format!("com.example.Suite#{name}"), "status": status});
    if msg.is_some() || trace.is_some() {
        v["error"] = json!({"message": msg, "trace": trace});
    }
    v
}

fn timed(mut v: Value, ms: i64) -> Value {
    v["durationMs"] = json!(ms);
    v
}

async fn launch_on(s: &Server, project: &str, branch: &str) -> String {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches"),
            ADMIN_TOKEN,
            json!({"name": "run", "vcs": {"branch": branch}}),
        )
        .await;
    assert!(st.is_success(), "{b}");
    b["id"].as_str().unwrap().to_string()
}

async fn post_results(s: &Server, project: &str, lid: &str, items: Vec<Value>) {
    let (st, out) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{lid}/results"),
            ADMIN_TOKEN,
            Value::Array(items),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    assert!(out["rejected"].as_array().unwrap().is_empty(), "{out}");
}

async fn by_name(s: &Server, project: &str, lid: &str) -> BTreeMap<String, Value> {
    s.results(project, lid, "")
        .await
        .into_iter()
        .map(|r| (r["name"].as_str().unwrap().to_string(), r))
        .collect()
}

async fn type_id(s: &Server, project: &str, group: &str) -> String {
    let (_, b) = s
        .get(
            &format!("/api/v1/projects/{project}/defect-types"),
            ADMIN_TOKEN,
        )
        .await;
    b["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["group"] == group && t["builtIn"] == true)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn project_id(store: &Store, key: &str) -> uuid::Uuid {
    store.project(key).await.unwrap().unwrap().id
}

#[tokio::test]
async fn fingerprints_clusters_triage_and_similar() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    let store = Store::new(db.pool.clone());
    s.project("an").await;
    let viewer = s.token("viewer", "viewer", &["an"]).await;
    let uploader = s.token("ci", "uploader", &["an"]).await;
    let editor = s.token("alice", "editor", &["an"]).await;

    let l1 = launch_on(&s, "an", "main").await;
    post_results(
        &s,
        "an",
        &l1,
        vec![
            res(
                "1",
                "t1",
                "failed",
                Some("Connection refused (attempt 1)"),
                Some(JAVA_A),
            ),
            res(
                "2",
                "t2",
                "failed",
                Some("Connection refused (attempt 7)"),
                Some(JAVA_A),
            ),
            res(
                "3",
                "t3",
                "broken",
                Some("NPE in cart 0x7ffd12"),
                Some(JAVA_B),
            ),
            res("4", "t4", "passed", None, None),
        ],
    )
    .await;
    let r = by_name(&s, "an", &l1).await;
    let fp1 = r["t1"]["fingerprint"].as_str().unwrap().to_string();
    assert_eq!(fp1.len(), 16);
    assert_eq!(r["t2"]["fingerprint"], fp1.as_str());
    assert_ne!(r["t3"]["fingerprint"], fp1.as_str());
    assert!(r["t4"]["fingerprint"].is_null());
    for t in ["t1", "t2", "t3", "t4"] {
        assert_eq!(r[t]["transition"], "new", "{t}");
        assert_eq!(r[t]["flaky"], false);
    }
    assert_eq!(r["t1"]["defect"]["group"], "toInvestigate");
    assert_eq!(r["t1"]["defect"]["source"], "rule");
    assert!(r["t4"]["defect"].is_null());

    let (st, c) = s
        .get(
            &format!("/api/v1/projects/an/launches/{l1}/clusters"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    let items = c["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["fingerprint"], fp1.as_str());
    assert_eq!(items[0]["count"], 2);
    assert_eq!(items[0]["statuses"], json!({"failed": 2, "broken": 0}));
    assert_eq!(items[0]["isNew"], true);
    assert!(items[0].get("knownSince").is_none());
    assert_eq!(items[1]["statuses"]["broken"], 1);

    // Triage permissions: viewer and uploader 403, editor ok.
    let pb = type_id(&s, "an", "productBug").await;
    let body = json!({"launchId": l1, "fingerprint": fp1, "defectTypeId": pb, "comment": "vault down",
        "issue": {"url": "https://issues.example.com/browse/OPS-1", "key": "OPS-1"}});
    for t in [&viewer, &uploader] {
        let (st, b) = s.post("/api/v1/projects/an/triage", t, body.clone()).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{b}");
    }
    let (st, b) = s.post("/api/v1/projects/an/triage", &editor, body).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["updated"], 2);
    let r = by_name(&s, "an", &l1).await;
    for t in ["t1", "t2"] {
        let d = &r[t]["defect"];
        assert_eq!(d["group"], "productBug");
        assert_eq!(d["typeId"], pb.as_str());
        assert_eq!(d["comment"], "vault down");
        assert_eq!(d["issueKey"], "OPS-1");
        assert_eq!(d["source"], "user");
        assert_eq!(d["by"], "alice");
    }
    assert_eq!(r["t3"]["defect"]["group"], "toInvestigate");
    let (st, _) = s
        .post(
            "/api/v1/projects/an/triage",
            &editor,
            json!({"launchId": l1, "fingerprint": fp1}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = s
        .post(
            "/api/v1/projects/an/triage",
            &editor,
            json!({"resultIds": [r["t3"]["id"]], "defectTypeId": uuid::Uuid::now_v7()}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Mute with an end, then unmute.
    let until = (chrono::Utc::now() + chrono::Duration::days(3)).to_rfc3339();
    let (st, b) = s
        .post(
            "/api/v1/projects/an/triage",
            &editor,
            json!({"resultIds": [r["t3"]["id"]], "mute": {"until": until, "reason": "known"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let r = by_name(&s, "an", &l1).await;
    assert_eq!(r["t3"]["muted"], true);
    assert!(r["t3"]["muteUntil"].is_string());
    assert_eq!(s.launch_get("an", &l1).await["counts"]["muted"], 1);
    let (st, _) = s
        .post(
            "/api/v1/projects/an/triage",
            &editor,
            json!({"resultIds": [r["t3"]["id"]], "mute": {"until": "2001-01-01T00:00:00Z", "reason": "x"}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "a mute ending in the past");

    // Defect types: settings:write for changes.
    let (st, _) = s
        .post(
            "/api/v1/projects/an/defect-types",
            &editor,
            json!({"group": "productBug", "name": "Backend bug"}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, custom) = s
        .post(
            "/api/v1/projects/an/defect-types",
            ADMIN_TOKEN,
            json!({"group": "productBug", "name": "Backend bug", "shortName": "BB", "color": "#112233"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{custom}");
    let (st, _) = s
        .post(
            "/api/v1/projects/an/defect-types",
            ADMIN_TOKEN,
            json!({"group": "productBug", "name": "Backend bug"}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, _) = s
        .post(
            "/api/v1/projects/an/defect-types",
            ADMIN_TOKEN,
            json!({"group": "nope", "name": "x"}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/an/defect-types/{pb}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "built-in types stay");
    let (st, b) = s
        .post(
            "/api/v1/projects/an/triage",
            &editor,
            json!({"resultIds": [r["t3"]["id"]], "defectTypeId": custom["id"], "unmute": true, "source": "ai"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let r = by_name(&s, "an", &l1).await;
    assert_eq!(r["t3"]["defect"]["name"], "Backend bug");
    assert_eq!(r["t3"]["defect"]["group"], "productBug");
    assert_eq!(r["t3"]["defect"]["source"], "ai");
    assert_eq!(r["t3"]["muted"], false);
    assert_eq!(s.launch_get("an", &l1).await["counts"]["muted"], 0);

    // A second launch: known errors, transitions, similar failures.
    let l2 = launch_on(&s, "an", "main").await;
    post_results(
        &s,
        "an",
        &l2,
        vec![
            res(
                "1",
                "t1",
                "failed",
                Some("Connection refused (attempt 2)"),
                Some(JAVA_A),
            ),
            res("3", "t3", "passed", None, None),
            res(
                "4",
                "t4",
                "failed",
                Some("expected: <1> but was: <2>"),
                Some(JAVA_B),
            ),
            res(
                "5",
                "t5",
                "failed",
                Some("Connection refused (attempt 3) while loading"),
                Some(JAVA_C),
            ),
        ],
    )
    .await;
    let r2 = by_name(&s, "an", &l2).await;
    assert!(r2["t1"].get("transition").is_none(), "{}", r2["t1"]);
    assert_eq!(r2["t3"]["transition"], "fixed");
    assert_eq!(r2["t4"]["transition"], "regressed");
    assert_eq!(r2["t5"]["transition"], "new");
    assert_eq!(
        r2["t1"]["defect"]["group"], "toInvestigate",
        "triage is per result"
    );
    let (_, c) = s
        .get(
            &format!("/api/v1/projects/an/launches/{l2}/clusters"),
            ADMIN_TOKEN,
        )
        .await;
    let known = c["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["fingerprint"] == fp1.as_str())
        .unwrap();
    assert_eq!(known["isNew"], false);
    assert!(known["knownSince"].is_string());

    let t1 = r2["t1"]["id"].as_str().unwrap();
    let (st, sim) = s
        .get(
            &format!("/api/v1/projects/an/results/{t1}/similar?limit=10"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{sim}");
    let items = sim["items"].as_array().unwrap();
    let same: BTreeSet<&str> = items
        .iter()
        .filter(|i| i["score"] == 1.0)
        .map(|i| i["resultId"].as_str().unwrap())
        .collect();
    assert_eq!(
        same,
        BTreeSet::from([
            r["t1"]["id"].as_str().unwrap(),
            r["t2"]["id"].as_str().unwrap()
        ])
    );
    assert_eq!(items[0]["defect"]["group"], "productBug");
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/an/results/{t1}/similar?limit=999"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    // Trigram similarity: with pg_trgm when installed, and the fallback computed in Rust.
    let pid = project_id(&store, "an").await;
    let id1: uuid::Uuid = t1.parse().unwrap();
    let mut modes = vec![false];
    if store.trigram_installed().await.unwrap() {
        modes.push(true);
    }
    for trgm in modes {
        let items = store
            .similar_results(pid, id1, 10, Some(trgm))
            .await
            .unwrap()
            .unwrap();
        let t5 = items
            .iter()
            .find(|i| i.result_id.to_string() == r2["t5"]["id"].as_str().unwrap())
            .unwrap_or_else(|| panic!("trgm={trgm}: {items:?}"));
        assert!(t5.score > 0.3 && t5.score < 1.0, "{}", t5.score);
        assert!(
            !items
                .iter()
                .any(|i| i.result_id.to_string() == r2["t4"]["id"].as_str().unwrap()),
            "unrelated messages are not similar"
        );
    }

    // Fingerprint backfill restores rows stored without fingerprints.
    sqlx::query("update attempts set fingerprint = null")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("update results set fingerprint = null")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("delete from analysis_state")
        .execute(&db.pool)
        .await
        .unwrap();
    let n = store
        .backfill_fingerprints(2, Duration::from_secs(30))
        .await
        .unwrap();
    assert!(n >= 6, "{n}");
    let r = by_name(&s, "an", &l1).await;
    assert_eq!(r["t1"]["fingerprint"], fp1.as_str());
    assert_eq!(r["t2"]["fingerprint"], fp1.as_str());

    // Issue keys become links through the project's issue trackers.
    let (st, _) = s
        .call(
            Method::PUT,
            "/api/v1/projects/an/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"issueTrackers": [{"type": "jira", "urlTemplate": "https://issues.example.com/browse/{key}",
                "keyRegex": "[A-Z]+-[0-9]+"}], "vcs": {"provider": "gitlab", "url": "https://gitlab.example.com", "repository": "group/app", "tokenSecretRef": "gitlab-token", "comment": true}})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, b) = s
        .post(
            "/api/v1/projects/an/triage",
            &editor,
            json!({"resultIds": [r["t3"]["id"]], "issue": {"key": "SHOP-7"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let r = by_name(&s, "an", &l1).await;
    assert_eq!(
        r["t3"]["defect"]["issueUrl"],
        "https://issues.example.com/browse/SHOP-7"
    );
    let (st, _) = s
        .post(
            "/api/v1/projects/an/triage",
            &editor,
            json!({"resultIds": [r["t3"]["id"]], "issue": {"key": "not a key"}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (_, set) = s.get("/api/v1/projects/an/settings", &viewer).await;
    assert_eq!(set["vcs"]["comment"], true, "other parts are kept");

    // The audit log has the triage.
    let (_, audit) = s
        .get("/api/v1/audit?action=results.triage", ADMIN_TOKEN)
        .await;
    assert!(audit["items"].as_array().unwrap().len() >= 3, "{audit}");
}

#[tokio::test]
async fn rules_on_ingest_dry_run_retroactive_and_mute_expiry() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["mutes.expire"]).await;
    let store = Store::new(db.pool.clone());
    s.project("ru").await;
    let editor = s.token("ed", "editor", &["ru"]).await;
    let uploader = s.token("ci", "uploader", &["ru"]).await;
    let viewer = s.token("v", "viewer", &["ru"]).await;
    let si = type_id(&s, "ru", "systemIssue").await;
    let ab = type_id(&s, "ru", "automationBug").await;

    let rule = json!({"name": "vault", "match": {"messageRegex": "(?i)connection refused", "traceRegex": "VaultClient"},
        "action": {"defectTypeId": si, "comment": "infra", "mute": {"days": 1}}});
    let (st, _) = s
        .post("/api/v1/projects/ru/rules", &uploader, rule.clone())
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    for bad in [
        json!({"messageRegex": "(a)\\1"}),
        json!({"messageRegex": "(?:a{1000}){1000}"}),
        json!({"messageRegex": "x".repeat(2000)}),
        json!({}),
    ] {
        let (st, b) = s
            .post(
                "/api/v1/projects/ru/rules",
                &editor,
                json!({"name": "bad", "match": bad, "action": {"comment": "c"}}),
            )
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    }
    let (st, b) = s
        .post(
            "/api/v1/projects/ru/rules",
            &editor,
            json!({"name": "x", "match": {"fingerprint": "0123456789abcdef"}, "action": {"defectTypeId": uuid::Uuid::now_v7()}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let (st, created) = s.post("/api/v1/projects/ru/rules", &editor, rule).await;
    assert_eq!(st, StatusCode::CREATED, "{created}");
    let rule_id = created["id"].as_str().unwrap().to_string();

    let l1 = launch_on(&s, "ru", "main").await;
    post_results(
        &s,
        "ru",
        &l1,
        vec![
            res(
                "1",
                "vault",
                "failed",
                Some("Connection refused"),
                Some(JAVA_A),
            ),
            res(
                "2",
                "npe",
                "broken",
                Some("NullPointerException in cart"),
                Some(JAVA_B),
            ),
            res(
                "3",
                "other",
                "failed",
                Some("Connection refused"),
                Some(JAVA_C),
            ),
        ],
    )
    .await;
    let r = by_name(&s, "ru", &l1).await;
    assert_eq!(r["vault"]["defect"]["group"], "systemIssue");
    assert_eq!(r["vault"]["defect"]["ruleId"], rule_id.as_str());
    assert_eq!(r["vault"]["defect"]["comment"], "infra");
    assert_eq!(r["vault"]["muted"], true);
    assert!(r["vault"]["muteUntil"].is_string());
    assert_eq!(
        r["other"]["defect"]["group"], "toInvestigate",
        "trace must match too"
    );
    assert_eq!(r["npe"]["defect"]["group"], "toInvestigate");
    assert_eq!(s.launch_get("ru", &l1).await["counts"]["muted"], 1);
    let (_, rules) = s.get("/api/v1/projects/ru/rules", &viewer).await;
    assert_eq!(rules["items"][0]["hits"], 1);

    // Dry run.
    let (st, dry) = s
        .post(
            "/api/v1/projects/ru/rules/test",
            &editor,
            json!({"launchId": l1, "rule": {"name": "npe", "match": {"messageRegex": "NullPointer"}, "action": {"defectTypeId": ab}}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{dry}");
    assert_eq!(dry["matches"], json!([r["npe"]["id"]]));
    assert_eq!(dry["truncated"], false);
    assert_eq!(
        r["npe"]["defect"]["group"], "toInvestigate",
        "dry runs change nothing"
    );

    // A person's triage is kept; a retroactive rule changes only the others.
    let (st, _) = s
        .post(
            "/api/v1/projects/ru/triage",
            &editor,
            json!({"resultIds": [r["other"]["id"]], "defectTypeId": si}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, npe_rule) = s
        .post(
            "/api/v1/projects/ru/rules",
            &editor,
            json!({"name": "npe", "match": {"messageRegex": "NullPointer|refused"}, "action": {"defectTypeId": ab, "accept": true}}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, task) = s
        .post(
            &format!(
                "/api/v1/projects/ru/rules/{}/apply?launchId={l1}",
                npe_rule["id"].as_str().unwrap()
            ),
            &editor,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{task}");
    let tid = task["id"].as_str().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let (_, t) = s
            .get(&format!("/api/v1/projects/ru/tasks/{tid}"), &viewer)
            .await;
        if t["status"] == "done" {
            assert_eq!(t["progress"]["updated"], 2, "{t}");
            break;
        }
        assert_ne!(t["status"], "failed", "{t}");
        assert!(std::time::Instant::now() < deadline, "{t}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let r = by_name(&s, "ru", &l1).await;
    assert_eq!(r["npe"]["defect"]["group"], "automationBug");
    assert_eq!(r["npe"]["defect"]["accepted"], true);
    assert_eq!(r["vault"]["defect"]["group"], "automationBug");
    assert_eq!(r["other"]["defect"]["source"], "user");
    assert_eq!(r["other"]["defect"]["group"], "systemIssue");

    // Mutes end: the expiry job lifts them.
    sqlx::query("update series_mutes set until = now() - interval '1 minute'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(store.expire_mutes().await.unwrap(), 1);
    let r = by_name(&s, "ru", &l1).await;
    assert_eq!(r["vault"]["muted"], false);
    assert_eq!(s.launch_get("ru", &l1).await["counts"]["muted"], 0);

    // Patch and delete.
    let (st, p) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/ru/rules/{rule_id}"),
            Some(&editor),
            Some(json!({"enabled": false, "order": 5})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(p["enabled"], false);
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/ru/defect-types/{si}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/ru/rules/{rule_id}"),
            Some(&editor),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn flaky_and_transitions_over_launches() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("fl").await;
    let editor = s.token("ed", "editor", &["fl"]).await;
    let statuses = ["passed", "failed", "passed", "failed", "passed", "failed"];
    let mut last = String::new();
    for (i, st) in statuses.iter().enumerate() {
        let l = launch_on(&s, "fl", "main").await;
        let msg = (*st == "failed").then_some("boom");
        post_results(
            &s,
            "fl",
            &l,
            vec![
                res("a", "flip", st, msg, None),
                res("b", "stable", "passed", None, None),
            ],
        )
        .await;
        let r = by_name(&s, "fl", &l).await;
        let expected = match (i, *st) {
            (0, _) => Some("new"),
            (_, "failed") => Some("regressed"),
            _ => Some("fixed"),
        };
        assert_eq!(r["flip"]["transition"].as_str(), expected, "launch {i}");
        if i > 0 {
            assert!(r["stable"].get("transition").is_none());
        }
        assert_eq!(r["stable"]["flaky"], false);
        last = l;
    }
    let r = by_name(&s, "fl", &last).await;
    assert_eq!(r["flip"]["flaky"], true, "{}", r["flip"]);

    let (st, f) = s.get("/api/v1/projects/fl/flaky", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{f}");
    let items = f["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{f}");
    assert_eq!(items[0]["name"], "flip");
    assert!(items[0]["score"].as_f64().unwrap() > 0.9);
    assert_eq!(
        items[0]["lastStatuses"],
        json!(["failed", "passed", "failed", "passed", "failed", "passed"])
    );
    assert!(items[0]["lastFlakyAt"].is_string());
    let (_, f) = s
        .get("/api/v1/projects/fl/flaky?branch=main", ADMIN_TOKEN)
        .await;
    assert_eq!(f["items"][0]["name"], "flip");
    let (_, f) = s
        .get("/api/v1/projects/fl/flaky?branch=other", ADMIN_TOKEN)
        .await;
    assert!(f["items"].as_array().unwrap().is_empty());

    // Another branch has its own history.
    let fb = launch_on(&s, "fl", "feature").await;
    post_results(
        &s,
        "fl",
        &fb,
        vec![res("b", "stable", "failed", Some("x"), None)],
    )
    .await;
    assert_eq!(by_name(&s, "fl", &fb).await["stable"]["transition"], "new");

    // Settings: detection off.
    let (st, _) = s
        .call(
            Method::PUT,
            "/api/v1/projects/fl/settings",
            Some(&editor),
            Some(json!({"flaky": {"historyDepth": -1}})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, set) = s
        .call(
            Method::PUT,
            "/api/v1/projects/fl/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"flaky": {"historyDepth": -1, "stabilizationPeriod": 5}})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{set}");
    assert_eq!(set["retention"]["rawDays"], 90);
    let (st, _) = s
        .call(
            Method::PUT,
            "/api/v1/projects/fl/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"flaky": {"historyDepth": 1000}})),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (_, got) = s.get("/api/v1/projects/fl/settings", &editor).await;
    assert_eq!(got["flaky"]["historyDepth"], -1);
    let l = launch_on(&s, "fl", "main").await;
    post_results(&s, "fl", &l, vec![res("a", "flip", "passed", None, None)]).await;
    post_results(
        &s,
        "fl",
        &l,
        vec![res("a2", "flip", "failed", Some("boom"), None)],
    )
    .await;
    let r = by_name(&s, "fl", &l).await;
    assert_eq!(r["flip"]["flaky"], false);
    assert_eq!(r["flip"]["flakyInRun"], true);
}

#[tokio::test]
async fn compare_and_important_launches() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("cmp").await;
    let uploader = s.token("ci", "uploader", &["cmp"]).await;
    let viewer = s.token("v", "viewer", &["cmp"]).await;
    let a = launch_on(&s, "cmp", "main").await;
    post_results(
        &s,
        "cmp",
        &a,
        vec![
            timed(res("a", "a", "passed", None, None), 100),
            timed(res("b", "b", "failed", Some("x"), None), 100),
            timed(res("c", "c", "failed", Some("x"), None), 100),
            timed(res("d", "d", "passed", None, None), 1000),
            timed(res("e", "e", "passed", None, None), 100),
        ],
    )
    .await;
    let b = launch_on(&s, "cmp", "main").await;
    post_results(
        &s,
        "cmp",
        &b,
        vec![
            timed(res("a", "a", "failed", Some("y"), None), 100),
            timed(res("b", "b", "passed", None, None), 120),
            timed(res("c", "c", "broken", Some("z"), None), 100),
            timed(res("d", "d", "passed", None, None), 3000),
            timed(res("f", "f", "passed", None, None), 100),
        ],
    )
    .await;
    let (st, c) = s
        .get(
            &format!("/api/v1/projects/cmp/launches/{a}/compare/{b}"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    let names = |k: &str| -> Vec<String> {
        c[k].as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names("newFailures"), vec!["a"]);
    assert_eq!(names("fixed"), vec!["b"]);
    assert_eq!(names("stillFailing"), vec!["c"]);
    assert_eq!(names("newTests"), vec!["f"]);
    assert_eq!(names("missing"), vec!["e"]);
    assert_eq!(c["missing"][0]["launchId"], a.as_str());
    let d = c["durationChanges"].as_array().unwrap();
    assert_eq!(d.len(), 1, "{c}");
    assert_eq!(d[0]["name"], "d");
    assert_eq!(d[0]["deltaMs"], 2000);
    assert_eq!(d[0]["ratio"], 3.0);
    let (st, _) = s
        .get(
            &format!(
                "/api/v1/projects/cmp/launches/{a}/compare/{}",
                uuid::Uuid::now_v7()
            ),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Important flag: launches:write.
    let (st, _) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/cmp/launches/{a}"),
            Some(&viewer),
            Some(json!({"important": true})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, l) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/cmp/launches/{a}"),
            Some(&uploader),
            Some(json!({"important": true, "name": "release 1.0", "attributes": [{"key": "release", "value": "1.0"}]})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{l}");
    assert_eq!(l["important"], true);
    assert_eq!(l["name"], "release 1.0");
    assert_eq!(l["attributes"][0]["key"], "release");
    let (st, _) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/cmp/launches/{a}"),
            Some(&uploader),
            Some(json!({"bogus": 1})),
        )
        .await;
    assert!(st.is_client_error(), "{st}");
}

/// Creates a launch and moves its creation time into the past (before any result is stored),
/// so its rows land in that month's partition.
async fn old_launch(s: &Server, db: &TestDb, project: &str, days_ago: i64) -> String {
    let id = launch_on(s, project, "main").await;
    sqlx::query(
        "update launches set created_at = now() - make_interval(days => $2), \
         started_at = now() - make_interval(days => $2) where id = $1::uuid",
    )
    .bind(&id)
    .bind(days_ago as i32)
    .execute(&db.pool)
    .await
    .unwrap();
    id
}

async fn dash(s: &Server, project: &str, query: &str) -> Value {
    let (st, d) = s
        .get(
            &format!("/api/v1/projects/{project}/dashboard{query}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{d}");
    d
}

#[tokio::test]
async fn dashboard_rollup_partitions_and_retention() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    let store = Store::new(db.pool.clone());
    // The first runs of the scheduler's jobs drop empty old partitions, and this test moves
    // launches into the past.
    db.hold_jobs(&[
        "history.rollup",
        "retention.apply",
        "analysis.indexes",
        "partitions.ensure",
    ])
    .await;
    store.ensure_partitions(6, 1).await.unwrap();
    let valid: bool = sqlx::query_scalar(
        "select indisvalid from pg_index where indexrelid = 'results_fingerprint'::regclass",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        valid,
        "the partitioned fingerprint index covers every partition"
    );
    s.project("dash").await;
    let (st, _) = s
        .call(
            Method::PUT,
            "/api/v1/projects/dash/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"retention": {"rawDays": 60, "keepImportant": true}, "rollup": {"afterDays": 30}})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let today = chrono::Utc::now().date_naive();
    let old_day = today - chrono::Duration::days(125);
    let imp_day = today - chrono::Duration::days(160);
    let month = |d: chrono::NaiveDate| d.format("%Y%m").to_string();
    // Old month: two launches, rolled up and then dropped with their partition.
    let o1 = old_launch(&s, &db, "dash", 125).await;
    post_results(
        &s,
        "dash",
        &o1,
        vec![
            timed(res("p1", "p1", "passed", None, None), 100),
            timed(res("p2", "p2", "failed", Some("boom 1"), None), 200),
            timed(res("s1", "s1", "skipped", None, None), 0),
        ],
    )
    .await;
    let o2 = old_launch(&s, &db, "dash", 125).await;
    post_results(
        &s,
        "dash",
        &o2,
        vec![
            timed(res("p1", "p1", "passed", None, None), 120),
            timed(res("p2", "p2", "passed", None, None), 180),
        ],
    )
    .await;
    // Older month with an important launch: kept.
    let imp = old_launch(&s, &db, "dash", 160).await;
    post_results(
        &s,
        "dash",
        &imp,
        vec![timed(res("p1", "p1", "failed", Some("boom 2"), None), 90)],
    )
    .await;
    let (st, _) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/dash/launches/{imp}"),
            Some(ADMIN_TOKEN),
            Some(json!({"important": true})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let recent = launch_on(&s, "dash", "main").await;
    post_results(
        &s,
        "dash",
        &recent,
        vec![
            timed(res("p1", "p1", "passed", None, None), 100),
            timed(res("p2", "p2", "failed", Some("boom 3"), None), 300),
        ],
    )
    .await;
    let parts = store.partitions("results").await.unwrap();
    let old_part = format!("results_p{}", month(old_day));
    let imp_part = format!("results_p{}", month(imp_day));
    assert!(
        parts.contains(&old_part) && parts.contains(&imp_part),
        "{parts:?}"
    );
    let raw_in_old: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select count(*) from {old_part}"
    )))
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(raw_in_old, 5);

    let q = format!("?from={}&to={today}", today - chrono::Duration::days(200));
    let before = dash(&s, "dash", &q).await;
    let rate = |d: &Value, day: chrono::NaiveDate| -> (Value, Value) {
        let p = d["passRate"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["t"] == day.to_string())
            .unwrap_or_else(|| panic!("{day}: {d}"));
        (p["passRate"].clone(), p["total"].clone())
    };
    // Day of o1 + o2: 3 passed of 5, one skipped: 3 / (5 - 1).
    assert_eq!(rate(&before, old_day), (json!(0.75), json!(5)));
    assert_eq!(rate(&before, today), (json!(0.5), json!(2)));
    assert_eq!(rate(&before, imp_day), (json!(0.0), json!(1)));
    let launches_old = before["launches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["t"] == old_day.to_string())
        .unwrap()["count"]
        .clone();
    assert_eq!(launches_old, 2);
    let defects_old = before["defectGroups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["t"] == old_day.to_string())
        .unwrap()
        .clone();
    assert_eq!(defects_old["toInvestigate"], 1);
    assert_eq!(before["mostFailing"][0]["name"], "p2");
    assert_eq!(before["mostFailing"][0]["failures"], 2);
    let durations_old = before["durations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["t"] == old_day.to_string())
        .unwrap()
        .clone();
    assert_eq!(durations_old["p50Ms"], 120);

    // Roll up a launch: aggregates equal the raw counts.
    assert!(store.rollup_launch(o1.parse().unwrap()).await.unwrap());
    assert!(
        !store.rollup_launch(o1.parse().unwrap()).await.unwrap(),
        "once"
    );
    let agg: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "select sum(runs)::bigint, sum(passed)::bigint, sum(failed)::bigint, sum(skipped)::bigint, \
         sum(to_investigate)::bigint from series_daily",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let raw: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "select count(*), count(*) filter (where status = 'passed'), count(*) filter (where status = 'failed'), \
         count(*) filter (where status = 'skipped'), count(*) filter (where defect->>'group' = 'toInvestigate') \
         from results where launch_id = $1::uuid",
    )
    .bind(&o1)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(agg, raw);
    let mixed = dash(&s, "dash", &q).await;
    assert_eq!(
        rate(&mixed, old_day),
        (json!(0.75), json!(5)),
        "no double counting"
    );

    // The rollup job: everything older than 30 days, then the old partition goes; the
    // important launch keeps its month; the default partition stays.
    let size_before: i64 = sqlx::query_scalar("select pg_total_relation_size($1::regclass)")
        .bind(&old_part)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let report = store.rollup_due(Duration::from_secs(60)).await.unwrap();
    assert_eq!(report.launches, 2, "{report:?}");
    assert!(report.partitions_dropped.contains(&old_part), "{report:?}");
    assert!(!report.partitions_dropped.contains(&imp_part), "{report:?}");
    eprintln!(
        "ROLLUP: dropped {old_part} ({size_before} bytes with indexes); raw rows 5 -> 0, series_daily rows {}",
        sqlx::query_scalar::<_, i64>("select count(*) from series_daily")
            .fetch_one(&db.pool)
            .await
            .unwrap()
    );
    let parts = store.partitions("results").await.unwrap();
    assert!(!parts.contains(&old_part));
    assert!(parts.contains(&imp_part));
    assert!(parts.contains(&"results_default".to_string()));
    assert!(
        !store
            .partitions("attempts")
            .await
            .unwrap()
            .contains(&format!("attempts_p{}", month(old_day)))
    );
    let (st, _) = s
        .get(&format!("/api/v1/projects/dash/launches/{o1}"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let after = dash(&s, "dash", &q).await;
    assert_eq!(rate(&after, old_day), (json!(0.75), json!(5)));
    assert_eq!(rate(&after, imp_day), (json!(0.0), json!(1)));
    assert_eq!(rate(&after, today), (json!(0.5), json!(2)));
    assert_eq!(after["mostFailing"][0]["failures"], 2);
    let p50 = after["durations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["t"] == old_day.to_string())
        .unwrap()["p50Ms"]
        .as_i64()
        .unwrap();
    assert!((80..=160).contains(&p50), "{p50}");
    let weekly = dash(&s, "dash", &format!("{q}&bucket=week")).await;
    let total: i64 = weekly["passRate"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["total"].as_i64().unwrap())
        .sum();
    assert_eq!(total, 8);

    // History beyond the raw window comes from the aggregates.
    let p1_tc = by_name(&s, "dash", &recent).await["p1"]["testCaseId"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, h) = s
        .get(
            &format!("/api/v1/projects/dash/test-cases/{p1_tc}/history"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{h}");
    assert_eq!(
        h["items"].as_array().unwrap().len(),
        2,
        "raw: recent + important"
    );
    let daily = h["daily"].as_array().unwrap();
    let d = daily
        .iter()
        .find(|d| d["day"] == old_day.to_string())
        .unwrap_or_else(|| panic!("{h}"));
    assert_eq!(d["runs"], 2);
    assert_eq!(d["passed"], 2);

    // Retention keeps the important launch; preview first.
    let (st, p) = s
        .get("/api/v1/projects/dash/retention/preview", ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(p["launches"], 0);
    assert_eq!(p["importantKept"], 1);
    let r = store
        .retention_apply(Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(r.launches_deleted, 0);
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/dash/launches/{imp}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    s.call(
        Method::PATCH,
        &format!("/api/v1/projects/dash/launches/{imp}"),
        Some(ADMIN_TOKEN),
        Some(json!({"important": false})),
    )
    .await;
    let (_, p) = s
        .get("/api/v1/projects/dash/retention/preview", ADMIN_TOKEN)
        .await;
    assert_eq!(p["launches"], 1);
    assert_eq!(p["launchIds"], json!([imp]));
    let r = store
        .retention_apply(Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(r.launches_deleted, 1);
    let after = dash(&s, "dash", &q).await;
    assert_eq!(
        rate(&after, imp_day),
        (json!(0.0), json!(1)),
        "rolled up before deletion"
    );
}

#[tokio::test]
async fn retention_by_count_queues_unreferenced_blobs() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["retention.apply", "attachments.gc"]).await;
    let store = Store::new(db.pool.clone());
    s.project("keep").await;
    s.call(
        Method::PUT,
        "/api/v1/projects/keep/settings",
        Some(ADMIN_TOKEN),
        Some(json!({"retention": {"rawDays": 90, "launchesKeep": 2}})),
    )
    .await;
    let mut ids = Vec::new();
    for i in 0..4 {
        let l = launch_on(&s, "keep", "main").await;
        post_results(&s, "keep", &l, vec![res("a", "a", "passed", None, None)]).await;
        ids.push(l);
        if i == 0 {
            let r = s
                .req(
                    Method::POST,
                    &format!("/api/v1/projects/keep/launches/{}/attachments", ids[0]),
                    Some(ADMIN_TOKEN),
                )
                .header("content-type", "text/plain")
                .header("x-fastproof-attachment-name", "log.txt")
                .body("only in the first launch")
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::CREATED);
        }
    }
    s.call(
        Method::PATCH,
        &format!("/api/v1/projects/keep/launches/{}", ids[1]),
        Some(ADMIN_TOKEN),
        Some(json!({"important": true})),
    )
    .await;
    let (_, p) = s
        .get("/api/v1/projects/keep/retention/preview", ADMIN_TOKEN)
        .await;
    assert_eq!(p["launches"], 1, "{p}");
    assert_eq!(p["launchIds"], json!([ids[0]]));
    sqlx::query("update attachments set last_seen_at = now() - interval '2 days'")
        .execute(&db.pool)
        .await
        .unwrap();
    let r = store
        .retention_apply(Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(r.launches_deleted, 1);
    assert_eq!(r.blobs_queued, 1);
    let (_, list) = s.get("/api/v1/projects/keep/launches", ADMIN_TOKEN).await;
    let left: BTreeSet<String> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(left, ids[1..].iter().cloned().collect());
    let key: String =
        sqlx::query_scalar("select prefix from blob_deletions where project_id is null")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let path = s.dir.path().join("blobs").join(&key);
    assert!(path.exists(), "{}", path.display());
    sqlx::query("update scheduled_jobs set next_run_at = now() where name = 'projects.purge'")
        .execute(&db.pool)
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let n: i64 = sqlx::query_scalar("select count(*) from blob_deletions")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        if !path.exists() && n == 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "blob not deleted (file exists: {}, queued: {n})",
            path.exists()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn testplan_json_for_allure_adapters_and_cli() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("tp").await;
    let uploader = s.token("ci", "uploader", &["tp"]).await;
    let viewer = s.token("v", "viewer", &["tp"]).await;
    let l = s.launch("tp", None).await;
    let lid = l["id"].as_str().unwrap();
    let dir = common::fixture("junit5/allure-results");
    let u = s
        .ingest("tp", lid, common::tar_gz(&dir, &|_| true), "?format=allure")
        .await;
    assert_eq!(u["status"], "done", "{u}");

    // Expected selectors: fullName of the tests whose final result is failed or broken.
    let mut latest: BTreeMap<String, (i64, String)> = BTreeMap::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if !p.to_string_lossy().ends_with("-result.json") {
            continue;
        }
        let v: Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
        let name = v["fullName"].as_str().unwrap().to_string();
        let series = format!("{name}\n{}", v["historyId"]);
        let start = v["start"].as_i64().unwrap_or(0);
        let st = v["status"].as_str().unwrap().to_string();
        let e = latest.entry(series).or_insert((i64::MIN, String::new()));
        if start >= e.0 {
            *e = (start, st);
        }
    }
    let expected: BTreeSet<String> = latest
        .into_iter()
        .filter(|(_, (_, st))| st == "failed" || st == "broken")
        .map(|(n, _)| n.split('\n').next().unwrap().to_string())
        .collect();
    assert!(!expected.is_empty());

    let (st, _) = s
        .post(
            "/api/v1/projects/tp/test-plans",
            &viewer,
            json!({"name": "rerun", "fromLaunchId": lid}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .post(
            "/api/v1/projects/tp/test-plans",
            &uploader,
            json!({"name": "rerun"}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, plan) = s
        .post(
            "/api/v1/projects/tp/test-plans",
            &uploader,
            json!({"name": "rerun", "fromLaunchId": lid, "statuses": ["failed", "broken"]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{plan}");
    let pid = plan["id"].as_str().unwrap();
    let r = s
        .req(
            Method::GET,
            &format!("/api/v1/projects/tp/test-plans/{pid}/testplan.json"),
            Some(&viewer),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(
        r.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    let tp: Value = r.json().await.unwrap();
    // Allure TestPlan v1.0: {"version": "1.0", "tests": [{"id"?: string, "selector": string}]}
    // as read by allure-java (TestPlanV1_0), allure-js-commons and allure-pytest.
    let obj = tp.as_object().unwrap();
    assert_eq!(obj.keys().collect::<Vec<_>>(), vec!["version", "tests"]);
    assert_eq!(tp["version"], "1.0");
    let mut selectors = BTreeSet::new();
    for t in tp["tests"].as_array().unwrap() {
        let o = t.as_object().unwrap();
        assert!(o.keys().all(|k| k == "id" || k == "selector"), "{t}");
        assert!(o["selector"].is_string());
        if let Some(id) = o.get("id") {
            assert!(id.is_string());
        }
        selectors.insert(o["selector"].as_str().unwrap().to_string());
    }
    assert_eq!(selectors, expected);

    // The CLI prints the same document.
    let tok = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tok.path(), &uploader).unwrap();
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_fastproof"))
        .args([
            "testplan",
            "--from-launch",
            lid,
            "--status",
            "failed,broken",
        ])
        .env_clear()
        .env("FASTPROOF_URL", &s.base)
        .env("FASTPROOF_PROJECT", "tp")
        .env("FASTPROOF_TOKEN_FILE", tok.path())
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cli: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(cli, tp);
}
