//! Dashboards on a real PostgreSQL: CRUD with `If-Match`, permissions, the lock, clones within
//! and across projects, limits, audit entries, and the data of every widget type. Skipped
//! without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::time::Duration;

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const TYPES: [&str; 20] = [
    "launch-statistics",
    "overall-statistics",
    "investigated-percentage",
    "launches-comparison",
    "launches-duration",
    "launches-table",
    "failed-trend",
    "non-passed-trend",
    "test-cases-growth",
    "passing-rate-per-launch",
    "passing-rate-summary",
    "flaky-tests",
    "most-failing-tests",
    "popular-patterns",
    "launch-execution",
    "component-health",
    "pass-rate-trend",
    "durations",
    "defect-trend",
    "launches-per-day",
];

fn widget(id: &str, kind: &str) -> Value {
    json!({"id": id, "type": kind, "title": kind, "settings": {}, "x": 0, "y": 0, "w": 6, "h": 4})
}

fn board(name: &str, widgets: Vec<Value>) -> Value {
    json!({"name": name, "description": "", "widgets": widgets})
}

fn path(project: &str, rest: &str) -> String {
    format!("/api/v1/projects/{project}/dashboards{rest}")
}

async fn put(
    s: &Server,
    token: &str,
    p: &str,
    rev: Option<i64>,
    body: Value,
) -> (StatusCode, Value) {
    let mut rb = s.req(Method::PUT, p, Some(token)).json(&body);
    if let Some(r) = rev {
        rb = rb.header("If-Match", format!("\"r{r}\""));
    }
    let r = rb.send().await.unwrap();
    let st = r.status();
    (st, r.json().await.unwrap_or(Value::Null))
}

async fn create(s: &Server, token: &str, project: &str, name: &str, widgets: Vec<Value>) -> Value {
    let (st, b) = s
        .post(&path(project, ""), token, board(name, widgets))
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    b
}

#[tokio::test]
async fn dashboards_crud_permissions_lock_clone_and_limits() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("dbd").await;
    s.project("dbe").await;
    let viewer = s.token("viewer-1", "viewer", &["dbd"]).await;
    let editor = s.token("editor-1", "editor", &["dbd"]).await;
    let editor2 = s.token("editor-2", "editor", &["dbd"]).await;

    let (st, _) = s.call(Method::GET, &path("dbd", ""), None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, b) = s
        .post(&path("dbd", ""), &viewer, board("nope", vec![]))
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "a viewer does not create: {b}");
    let (st, b) = s.get(&path("dbd", ""), &viewer).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, _) = s.get(&path("dbe", ""), &viewer).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "no scope in dbe");

    let b = create(
        &s,
        &editor,
        "dbd",
        "Mine",
        vec![widget("w1", "launch-statistics")],
    )
    .await;
    let id = b["id"].as_str().unwrap().to_string();
    assert_eq!(b["revision"], 1);
    assert_eq!(b["owner"], "editor-1");
    assert_eq!(b["locked"], false);

    let r = s
        .req(Method::GET, &path("dbd", &format!("/{id}")), Some(&viewer))
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["etag"], "\"r1\"");

    for bad in [
        board("", vec![]),
        board(&"n".repeat(121), vec![]),
        board(
            "x",
            (0..25)
                .map(|i| widget(&format!("w{i}"), "durations"))
                .collect(),
        ),
        board(
            "x",
            vec![widget("w1", "durations"), widget("w1", "durations")],
        ),
        board("x", vec![widget("W 1", "durations")]),
        board(
            "x",
            vec![json!({"id": "a", "type": "nope", "x": 0, "y": 0, "w": 6, "h": 4})],
        ),
        board(
            "x",
            vec![json!({"id": "a", "type": "durations", "x": 8, "y": 0, "w": 6, "h": 4})],
        ),
        board(
            "x",
            vec![json!({"id": "a", "type": "durations", "x": 0, "y": 0, "w": 6, "h": 99})],
        ),
        board(
            "x",
            vec![
                json!({"id": "a", "type": "durations", "settings": {"days": 0}, "x": 0, "y": 0, "w": 6, "h": 4}),
            ],
        ),
        board(
            "x",
            vec![
                json!({"id": "a", "type": "durations", "settings": {"limit": 51}, "x": 0, "y": 0, "w": 6, "h": 4}),
            ],
        ),
        board(
            "x",
            vec![
                json!({"id": "a", "type": "durations", "settings": {"sql": "1"}, "x": 0, "y": 0, "w": 6, "h": 4}),
            ],
        ),
        board(
            "x",
            vec![
                json!({"id": "a", "type": "durations", "title": "t\u{0}", "x": 0, "y": 0, "w": 6, "h": 4}),
            ],
        ),
    ] {
        let (st, out) = s.post(&path("dbd", ""), &editor, bad.clone()).await;
        assert!(
            st.is_client_error() && st != StatusCode::FORBIDDEN,
            "{bad} -> {st} {out}"
        );
    }

    let p = path("dbd", &format!("/{id}"));
    let (st, _) = put(&s, &editor, &p, None, board("Mine", vec![])).await;
    assert_eq!(st, StatusCode::PRECONDITION_REQUIRED);
    let (st, out) = put(&s, &editor, &p, Some(7), board("Mine", vec![])).await;
    assert_eq!(st, StatusCode::PRECONDITION_FAILED, "{out}");
    assert_eq!(out["details"]["currentRevision"], 1);
    let (st, out) = put(
        &s,
        &editor,
        &p,
        Some(1),
        board(
            "Renamed",
            vec![
                widget("w1", "launch-statistics"),
                widget("w2", "flaky-tests"),
            ],
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert_eq!(out["revision"], 2);
    assert_eq!(out["widgets"].as_array().unwrap().len(), 2);
    let (st, out) = put(&s, &editor, &p, Some(1), board("Stale", vec![])).await;
    assert_eq!(st, StatusCode::PRECONDITION_FAILED, "{out}");
    assert_eq!(out["details"]["changedBy"], "editor-1");
    let (st, _) = put(&s, &viewer, &p, Some(2), board("Viewer", vec![])).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let (st, list) = s.get(&path("dbd", ""), &viewer).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list["items"][0]["widgetCount"], 2);
    assert_eq!(list["items"][0]["name"], "Renamed");

    let lock = format!("{p}/lock");
    let (st, out) = put(&s, &editor2, &lock, None, json!({"locked": true})).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "only the owner or a manager locks: {out}"
    );
    let (st, out) = put(&s, &editor, &lock, None, json!({"locked": true})).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert_eq!(out["locked"], true);
}

#[tokio::test]
async fn locked_dashboards_belong_to_their_owner_and_managers() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("dbl").await;
    s.project("dbm").await;
    let owner = s.token("owner-1", "editor", &["dbl", "dbm"]).await;
    let other = s.token("other-1", "editor", &["dbl"]).await;
    let b = create(&s, &owner, "dbl", "Locked", vec![widget("w1", "durations")]).await;
    let id = b["id"].as_str().unwrap().to_string();
    let p = path("dbl", &format!("/{id}"));

    let (st, l) = put(
        &s,
        &owner,
        &format!("{p}/lock"),
        None,
        json!({"locked": true}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{l}");
    let rev = l["revision"].as_i64().unwrap();
    assert_eq!(l["locked"], true);

    let (st, out) = put(&s, &other, &p, Some(rev), board("Changed", vec![])).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{out}");
    assert_eq!(out["code"], "dashboard_locked");
    let (st, _) = s.call(Method::DELETE, &p, Some(&other), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = put(
        &s,
        &other,
        &format!("{p}/lock"),
        None,
        json!({"locked": false}),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, out) = s.get(&p, &other).await;
    assert_eq!(st, StatusCode::OK, "reading stays open: {out}");

    let (st, out) = put(&s, &owner, &p, Some(rev), board("Owner edit", vec![])).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    let rev = out["revision"].as_i64().unwrap();
    let (st, out) = put(
        &s,
        ADMIN_TOKEN,
        &p,
        Some(rev),
        board("Manager edit", vec![]),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "a manager may change it: {out}");
    let rev = out["revision"].as_i64().unwrap();

    let (st, cloned) = s.post(&format!("{p}/clone"), &other, json!({})).await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "anyone who writes may clone: {cloned}"
    );
    assert_eq!(cloned["locked"], false);
    assert_eq!(cloned["owner"], "other-1");
    assert_eq!(cloned["name"], "Manager edit (copy)");
    assert_eq!(cloned["project"], "dbl");

    let (st, _) = put(
        &s,
        &owner,
        &format!("{p}/lock"),
        None,
        json!({"locked": false}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, out) = put(&s, &other, &p, Some(rev + 1), board("Now open", vec![])).await;
    assert_eq!(st, StatusCode::OK, "{out}");

    let (st, _) = put(
        &s,
        &owner,
        &format!("{p}/lock"),
        None,
        json!({"locked": true, "x": 1}),
    )
    .await;
    assert!(st.is_client_error());

    let (st, _) = s.call(Method::DELETE, &p, Some(&other), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = s.get(&p, &owner).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s.call(Method::DELETE, &p, Some(&owner), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (st, audit) = s.get("/api/v1/audit?action=dashboard.*", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{audit}");
    let actions: Vec<&str> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["action"].as_str())
        .collect();
    for a in [
        "dashboard.create",
        "dashboard.update",
        "dashboard.lock",
        "dashboard.unlock",
        "dashboard.clone",
        "dashboard.delete",
    ] {
        assert!(actions.contains(&a), "audit has {a}: {actions:?}");
    }
}

#[tokio::test]
async fn clone_across_projects_checks_both_sides_and_drops_launch_ids() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("src").await;
    s.project("dst").await;
    let src_only = s.token("src-editor", "editor", &["src"]).await;
    let both = s.token("both-editor", "editor", &["src", "dst"]).await;
    let dst_viewer = s.token("dst-viewer", "viewer", &["src", "dst"]).await;
    let mut w = widget("w1", "launch-execution");
    w["settings"] = json!({"launchId": "0198a000-0000-7000-8000-000000000001", "days": 14});
    let b = create(
        &s,
        &src_only,
        "src",
        "Source",
        vec![w, widget("w2", "flaky-tests")],
    )
    .await;
    let p = path("src", &format!("/{}/clone", b["id"].as_str().unwrap()));

    let (st, out) = s.post(&p, &src_only, json!({"targetProject": "dst"})).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "no write access to the target: {out}"
    );
    let (st, out) = s
        .post(&p, &dst_viewer, json!({"targetProject": "dst"}))
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "read-only in the target: {out}");
    let (st, out) = s.post(&p, &both, json!({"targetProject": "missing"})).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "a scoped token learns nothing about other projects: {out}"
    );
    let (st, out) = s
        .post(&p, ADMIN_TOKEN, json!({"targetProject": "missing"}))
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{out}");
    let (st, out) = s
        .post(&p, &both, json!({"targetProject": "dst", "name": "Copy"}))
        .await;
    assert_eq!(st, StatusCode::CREATED, "{out}");
    assert_eq!(out["project"], "dst");
    assert_eq!(out["name"], "Copy");
    assert_eq!(out["widgets"].as_array().unwrap().len(), 2);
    assert!(out["widgets"][0]["settings"]["launchId"].is_null(), "{out}");
    assert_eq!(out["widgets"][0]["settings"]["days"], 14);
    let (st, list) = s.get(&path("dst", ""), &both).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let (st, _) = s
        .get(
            &path("dst", &format!("/{}", b["id"].as_str().unwrap())),
            &both,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "ids do not leak across projects");
    let (st, _) = s.post(&p, &src_only, json!({"unknown": 1})).await;
    assert!(st.is_client_error());
}

#[tokio::test]
async fn a_project_holds_at_most_fifty_dashboards() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("many").await;
    for i in 0..50 {
        create(&s, ADMIN_TOKEN, "many", &format!("b{i}"), vec![]).await;
    }
    let (st, out) = s
        .post(
            &path("many", ""),
            ADMIN_TOKEN,
            board("one too many", vec![]),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{out}");
    assert_eq!(out["code"], "dashboard_limit");
}

async fn results(s: &Server, project: &str, lid: &str, items: Vec<Value>) {
    let (st, out) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{lid}/results"),
            ADMIN_TOKEN,
            Value::Array(items),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
}

fn result(id: &str, status: &str, suite: &str, message: Option<&str>) -> Value {
    let mut v = json!({"id": id, "name": id, "fullName": format!("com.example.{suite}#{id}"), "status": status,
        "durationMs": 120, "labels": [{"name": "suite", "value": suite}]});
    if let Some(m) = message {
        v["error"] = json!({"message": m, "trace": null});
    }
    v
}

async fn wait_total(s: &Server, project: &str, lid: &str, total: i64) {
    for _ in 0..100 {
        if s.launch_get(project, lid).await["counts"]["total"] == total {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("launch {lid} did not reach {total} results");
}

#[tokio::test]
async fn widgets_read_aggregates_and_one_failing_widget_does_not_hide_the_rest() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("wd").await;
    let mut ids = Vec::new();
    for (n, branch) in ["main", "main", "dev"].into_iter().enumerate() {
        let (st, l) = s
            .post(
                "/api/v1/projects/wd/launches",
                ADMIN_TOKEN,
                json!({"name": format!("run {n}"), "vcs": {"branch": branch}}),
            )
            .await;
        assert!(st.is_success(), "{l}");
        let lid = l["id"].as_str().unwrap().to_string();
        results(
            &s,
            "wd",
            &lid,
            vec![
                result("a", "passed", "Cart", None),
                result(
                    "b",
                    if n == 1 { "passed" } else { "failed" },
                    "Cart",
                    Some("boom: connection refused"),
                ),
                result("c", "passed", "Pay", None),
                result("d", "skipped", "Pay", None),
            ],
        )
        .await;
        wait_total(&s, "wd", &lid, 4).await;
        ids.push(lid);
    }

    let widgets: Vec<Value> = TYPES
        .iter()
        .enumerate()
        .map(|(i, t)| widget(&format!("w{i}"), t))
        .collect();
    let b = create(&s, ADMIN_TOKEN, "wd", "All", widgets.clone()).await;
    let (st, data) = s
        .get(
            &path("wd", &format!("/{}/data", b["id"].as_str().unwrap())),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{data}");
    for (i, t) in TYPES.iter().enumerate() {
        let r = &data["widgets"][format!("w{i}")];
        assert_eq!(r["status"], "ok", "{t}: {r}");
    }
    let of = |t: &str| {
        let i = TYPES.iter().position(|x| *x == t).unwrap();
        data["widgets"][format!("w{i}")]["data"].clone()
    };
    let l = of("launches-table");
    assert_eq!(l["kind"], "launches");
    let launches = l["launches"].as_array().unwrap();
    assert_eq!(launches.len(), 3);
    assert_eq!(launches[0]["total"], 4, "oldest first");
    assert_eq!(launches[2]["id"], ids[2].as_str());
    assert_eq!(
        launches
            .iter()
            .map(|x| x["failed"].as_i64().unwrap())
            .sum::<i64>(),
        2
    );
    let ex = of("launch-execution");
    assert_eq!(ex["kind"], "launch");
    assert_eq!(
        ex["launch"]["id"],
        ids[2].as_str(),
        "the latest launch by default"
    );
    assert_eq!(
        ex["defects"]["toInvestigate"].as_i64().unwrap()
            + ex["defects"]["productBug"].as_i64().unwrap(),
        1
    );
    let h = of("component-health");
    assert_eq!(h["groupBy"], "suite");
    let comps = h["components"].as_array().unwrap();
    assert_eq!(comps.len(), 2);
    let cart = comps.iter().find(|c| c["name"] == "Cart").unwrap();
    assert_eq!(
        (
            cart["total"].as_i64(),
            cart["passed"].as_i64(),
            cart["failed"].as_i64()
        ),
        (Some(2), Some(1), Some(1))
    );
    assert_eq!(of("pass-rate-trend")["kind"], "trend");
    assert!(
        !of("pass-rate-trend")["trend"]["passRate"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(of("test-cases-growth")["points"][0]["added"], 4);
    assert_eq!(of("popular-patterns")["kind"], "patterns");

    let mut limited = widget("only-main", "launches-table");
    limited["settings"] = json!({"branch": "main", "limit": 1});
    let mut by_launch = widget("by-launch", "component-health");
    by_launch["settings"] = json!({"launchId": ids[0], "groupBy": "nolabel"});
    let (st, prev) = s
        .post(
            "/api/v1/projects/wd/dashboard-preview",
            ADMIN_TOKEN,
            json!({"widgets": [limited, by_launch]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{prev}");
    let one = prev["widgets"]["only-main"]["data"]["launches"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0]["branch"], "main");
    let c = prev["widgets"]["by-launch"]["data"]["components"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(c.len(), 1, "results without the label share one group");
    assert_eq!(c[0]["name"], "(none)");
    assert_eq!(c[0]["total"], 4);

    let (st, _) = s
        .post(
            "/api/v1/projects/wd/dashboard-preview",
            ADMIN_TOKEN,
            json!({"widgets": [widget("a", "nope")]}),
        )
        .await;
    assert!(st.is_client_error());
    let many: Vec<Value> = (0..25)
        .map(|i| widget(&format!("w{i}"), "durations"))
        .collect();
    let (st, _) = s
        .post(
            "/api/v1/projects/wd/dashboard-preview",
            ADMIN_TOKEN,
            json!({"widgets": many}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    s.project("elsewhere").await;
    let nobody = s.token("lonely", "viewer", &["elsewhere"]).await;
    let (st, _) = s
        .post(
            "/api/v1/projects/wd/dashboard-preview",
            &nobody,
            json!({"widgets": []}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .get(
            &path("wd", &format!("/{}/data", b["id"].as_str().unwrap())),
            &nobody,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}
