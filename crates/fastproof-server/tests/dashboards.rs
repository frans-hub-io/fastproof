//! Dashboards on a real PostgreSQL: CRUD with `If-Match`, permissions, the lock, clones within
//! and across projects, limits, audit entries, and the data of every widget type. Skipped
//! without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::time::Duration;

use common::{ADMIN_TOKEN, Server, TestDb};
use fastproof_store::Store;
use fastproof_store::analysis::boards::BoardLimits;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const TYPES: [&str; 23] = [
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
    "cumulative-trend",
    "unique-bugs",
    "activity-stream",
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
    let rev = match (rev, p.strip_suffix("/lock")) {
        (None, Some(board)) => Some(revision_of(s, board).await),
        (r, _) => r,
    };
    if let Some(r) = rev {
        rb = rb.header("If-Match", format!("\"r{r}\""));
    }
    let r = rb.send().await.unwrap();
    let st = r.status();
    (st, r.json().await.unwrap_or(Value::Null))
}

async fn revision_of(s: &Server, board_path: &str) -> i64 {
    let (st, b) = s.get(board_path, ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    b["revision"].as_i64().unwrap()
}

async fn delete(s: &Server, token: &str, p: &str, rev: Option<i64>) -> StatusCode {
    let rev = match rev {
        Some(r) => r,
        None => revision_of(s, p).await,
    };
    s.req(Method::DELETE, p, Some(token))
        .header("If-Match", format!("\"r{rev}\""))
        .send()
        .await
        .unwrap()
        .status()
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
    assert!(
        out["details"]["changedBy"].is_null(),
        "only managers see who changed it"
    );
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
    let st = delete(&s, &other, &p, None).await;
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

    let st = delete(&s, &other, &p, None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = s.get(&p, &owner).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let st = delete(&s, &owner, &p, Some(1)).await;
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

fn widget_with(id: &str, kind: &str, settings: Value) -> Value {
    let mut w = widget(id, kind);
    w["settings"] = settings;
    w
}

#[tokio::test]
async fn grid_arithmetic_cannot_overflow_and_expensive_computations_are_capped() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("cap").await;
    let wrap = json!({"id": "a", "type": "durations", "x": 2147483647, "y": 0, "w": 1, "h": 4});
    let (st, out) = s
        .post(&path("cap", ""), ADMIN_TOKEN, board("x", vec![wrap]))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{out}");
    let big = json!({"id": "a", "type": "durations", "x": 0, "y": 0, "w": 2147483647, "h": 4});
    let (st, _) = s
        .post(&path("cap", ""), ADMIN_TOKEN, board("x", vec![big]))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let trends = |n: u32| -> Vec<Value> {
        (0..n)
            .map(|i| {
                widget_with(
                    &format!("t{i}"),
                    "pass-rate-trend",
                    json!({"days": 100 + i}),
                )
            })
            .collect()
    };
    let (st, out) = s
        .post(&path("cap", ""), ADMIN_TOKEN, board("four", trends(4)))
        .await;
    assert_eq!(st, StatusCode::CREATED, "{out}");
    let (st, out) = s
        .post(&path("cap", ""), ADMIN_TOKEN, board("five", trends(5)))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{out}");
    let (st, _) = s
        .post(
            "/api/v1/projects/cap/dashboard-preview",
            ADMIN_TOKEN,
            json!({"widgets": trends(5)}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "preview applies the same cap");

    let mut same: Vec<Value> = (0..8)
        .map(|i| {
            widget(
                &format!("s{i}"),
                [
                    "pass-rate-trend",
                    "durations",
                    "defect-trend",
                    "flaky-tests",
                ][i % 4],
            )
        })
        .collect();
    same.extend((0..4).map(|i| widget(&format!("l{i}"), "launches-table")));
    let (st, out) = s
        .post(&path("cap", ""), ADMIN_TOKEN, board("shared", same))
        .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "widgets with one filter share one computation: {out}"
    );

    let comps = |n: usize| -> Vec<Value> {
        (0..n)
            .map(|i| {
                widget_with(
                    &format!("c{i}"),
                    "component-health",
                    json!({"groupBy": format!("g{i}")}),
                )
            })
            .collect()
    };
    let (st, _) = s
        .post(&path("cap", ""), ADMIN_TOKEN, board("three", comps(3)))
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, out) = s
        .post(&path("cap", ""), ADMIN_TOKEN, board("four scans", comps(4)))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{out}");
}

#[tokio::test]
async fn data_is_shared_between_viewers_and_preview_needs_write() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("shr").await;
    let viewer = s.token("viewer-a", "viewer", &["shr"]).await;
    let viewer2 = s.token("viewer-b", "viewer", &["shr"]).await;
    let editor = s.token("editor-a", "editor", &["shr"]).await;
    let b = create(
        &s,
        &editor,
        "shr",
        "Shared",
        vec![widget("w1", "launches-table")],
    )
    .await;
    let id = b["id"].as_str().unwrap().to_string();
    let data = path("shr", &format!("/{id}/data"));
    let (st, first) = s.get(&data, &viewer).await;
    assert_eq!(st, StatusCode::OK, "{first}");
    let (st, second) = s.get(&data, &viewer2).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        first["generatedAt"], second["generatedAt"],
        "the second viewer gets the cached data"
    );

    let (st, third) = put(
        &s,
        &editor,
        &path("shr", &format!("/{id}")),
        Some(1),
        board(
            "Shared",
            vec![widget("w1", "launches-table"), widget("w2", "durations")],
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{third}");
    let (_, fresh) = s.get(&data, &viewer).await;
    assert_ne!(
        fresh["generatedAt"], first["generatedAt"],
        "a new revision is computed again"
    );
    assert!(fresh["widgets"]["w2"].is_object());

    let body = json!({"widgets": [widget("w1", "launches-table")]});
    let (st, _) = s
        .post(
            "/api/v1/projects/shr/dashboard-preview",
            &viewer,
            body.clone(),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "viewers read saved dashboards but do not preview"
    );
    let (st, _) = s
        .post("/api/v1/projects/shr/dashboard-preview", &editor, body)
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = s.get(&path("shr", &format!("/{id}")), &viewer).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn widget_queries_run_with_a_statement_timeout_and_a_time_budget() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let store = Store::new(db.pool.clone()).with_board_limits(BoardLimits {
        total: Duration::from_secs(25),
        per_widget: Duration::from_secs(7),
        cache_ttl: Duration::from_secs(60),
    });
    assert_eq!(store.board_statement_timeout().await.unwrap(), "7s");
    let main: String = sqlx::query_scalar("show statement_timeout")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_ne!(main, "7s", "the main pool keeps its own timeout");

    let s = Server::start(&db).await;
    s.project("bud").await;
    let (st, p) = s.get("/api/v1/projects/bud", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{p}");
    let pid: uuid::Uuid = sqlx::query_scalar("select id from projects where key = 'bud'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let spent = Store::new(db.pool.clone()).with_board_limits(BoardLimits {
        total: Duration::ZERO,
        per_widget: Duration::from_secs(7),
        cache_ttl: Duration::from_secs(60),
    });
    let widgets: Vec<fastproof_store::analysis::boards::BoardWidget> =
        serde_json::from_value(json!([
            widget("a", "launches-table"),
            widget("b", "pass-rate-trend")
        ]))
        .unwrap();
    let settings = spent.project_settings(pid).await.unwrap();
    let d = spent.board_data(pid, &settings, &widgets).await;
    for id in ["a", "b"] {
        let r = serde_json::to_value(&d.widgets[id]).unwrap();
        assert_eq!(r["status"], "error", "{r}");
        assert!(r["message"].as_str().unwrap().contains("too long"));
    }
}

#[tokio::test]
async fn the_owner_is_a_stable_identity_not_a_name() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("own").await;
    let a = s.token("owner-a", "editor", &["own"]).await;
    let b = s.token("owner-b", "editor", &["own"]).await;
    let viewer = s.token("watcher", "viewer", &["own"]).await;
    let made = create(&s, &a, "own", "Mine", vec![]).await;
    let id = made["id"].as_str().unwrap().to_string();
    let p = path("own", &format!("/{id}"));
    assert_eq!(made["owned"], true);
    assert_eq!(made["owner"], "owner-a");
    let lock = format!("{p}/lock");
    let (st, _) = put(&s, &a, &lock, None, json!({"locked": true})).await;
    assert_eq!(st, StatusCode::OK);

    sqlx::query("update dashboards set owner = 'owner-b' where id = $1")
        .bind(uuid::Uuid::parse_str(&id).unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
    let (st, out) = put(&s, &b, &lock, None, json!({"locked": false})).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "carrying the owner's name does not make an owner: {out}"
    );
    let (st, out) = put(&s, &a, &lock, None, json!({"locked": false})).await;
    assert_eq!(st, StatusCode::OK, "{out}");

    let (_, seen_by_viewer) = s.get(&p, &viewer).await;
    assert_eq!(seen_by_viewer["owner"], "another user");
    assert_eq!(seen_by_viewer["owned"], false);
    let (_, list) = s.get(&path("own", ""), &viewer).await;
    assert_eq!(list["items"][0]["owner"], "another user");
    let (_, seen_by_owner) = s.get(&p, &a).await;
    assert_eq!(
        seen_by_owner["owner"], "owner-b",
        "the display name column is what was stored"
    );
    assert_eq!(seen_by_owner["owned"], true);
    let (_, seen_by_manager) = s.get(&p, ADMIN_TOKEN).await;
    assert_eq!(seen_by_manager["owner"], "owner-b");
    assert_eq!(seen_by_manager["owned"], false);

    let rev = seen_by_owner["revision"].as_i64().unwrap();
    let (st, out) = put(&s, &b, &p, Some(rev - 1), board("Stale", vec![])).await;
    assert_eq!(st, StatusCode::PRECONDITION_FAILED, "{out}");
    assert!(
        out["details"]["changedBy"].is_null(),
        "who changed it is shown to managers only: {out}"
    );
    let (st, out) = put(&s, ADMIN_TOKEN, &p, Some(rev - 1), board("Stale", vec![])).await;
    assert_eq!(st, StatusCode::PRECONDITION_FAILED);
    assert!(out["details"]["changedBy"].is_string(), "{out}");
}

#[tokio::test]
async fn existing_rows_get_an_owner_identity_from_the_migration() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("mig").await;
    let pid: uuid::Uuid = sqlx::query_scalar("select id from projects where key = 'mig'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    sqlx::query("insert into dashboards (id, project_id, name, owner, owner_key) values (gen_random_uuid(), $1, 'old', 'old-owner', '')")
        .bind(pid)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../../fastproof-store/migrations/0782_dashboards_owner_key.sql"
    ))
    .execute(&db.pool)
    .await
    .unwrap();
    let key: String = sqlx::query_scalar("select owner_key from dashboards where name = 'old'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(key, "user:old-owner");
}

#[tokio::test]
async fn parallel_creates_cannot_pass_the_limit() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = std::sync::Arc::new(Server::start(&db).await);
    s.project("race").await;
    let mut tasks = Vec::new();
    for i in 0..60 {
        let s = s.clone();
        tasks.push(tokio::spawn(async move {
            s.post(
                &path("race", ""),
                ADMIN_TOKEN,
                board(&format!("b{i}"), vec![]),
            )
            .await
            .0
        }));
    }
    let mut created = 0;
    for t in tasks {
        if t.await.unwrap() == StatusCode::CREATED {
            created += 1;
        }
    }
    assert_eq!(created, 50);
    let n: i64 = sqlx::query_scalar("select count(*) from dashboards")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 50);
}

#[tokio::test]
async fn delete_and_lock_need_if_match_and_clones_are_audited_in_both_projects() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("ifm").await;
    s.project("ifm2").await;
    let b = create(
        &s,
        ADMIN_TOKEN,
        "ifm",
        "Board",
        vec![widget("w1", "durations")],
    )
    .await;
    let p = path("ifm", &format!("/{}", b["id"].as_str().unwrap()));
    let (st, _) = s.call(Method::DELETE, &p, Some(ADMIN_TOKEN), None).await;
    assert_eq!(st, StatusCode::PRECONDITION_REQUIRED);
    assert_eq!(
        delete(&s, ADMIN_TOKEN, &p, Some(9)).await,
        StatusCode::PRECONDITION_FAILED
    );
    let (st, _) = s
        .call(
            Method::PUT,
            &format!("{p}/lock"),
            Some(ADMIN_TOKEN),
            Some(json!({"locked": true})),
        )
        .await;
    assert_eq!(st, StatusCode::PRECONDITION_REQUIRED);
    let (st, _) = put(
        &s,
        ADMIN_TOKEN,
        &format!("{p}/lock"),
        Some(9),
        json!({"locked": true}),
    )
    .await;
    assert_eq!(st, StatusCode::PRECONDITION_FAILED);
    let (st, _) = s.get(&p, ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "nothing was deleted or locked");

    let (st, out) = s
        .post(
            &format!("{p}/clone"),
            ADMIN_TOKEN,
            json!({"targetProject": "ifm2"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{out}");
    let (st, _) = s.post(&format!("{p}/clone"), ADMIN_TOKEN, json!({})).await;
    assert_eq!(st, StatusCode::CREATED);
    let (_, in_source) = s
        .get(
            "/api/v1/audit?scope=ifm&action=dashboard.clone*",
            ADMIN_TOKEN,
        )
        .await;
    let actions: Vec<&str> = in_source["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["action"].as_str())
        .collect();
    assert!(actions.contains(&"dashboard.clone.out"), "{actions:?}");
    assert_eq!(
        actions
            .iter()
            .filter(|a| **a == "dashboard.clone.out")
            .count(),
        1,
        "only the cross-project copy"
    );
    let (_, in_target) = s
        .get(
            "/api/v1/audit?scope=ifm2&action=dashboard.clone",
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(in_target["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        delete(&s, ADMIN_TOKEN, &p, None).await,
        StatusCode::NO_CONTENT
    );
}

async fn preview(s: &Server, project: &str, widgets: Vec<Value>) -> (StatusCode, Value) {
    s.post(
        &format!("/api/v1/projects/{project}/dashboard-preview"),
        ADMIN_TOKEN,
        json!({"widgets": widgets}),
    )
    .await
}

async fn launch_with(s: &Server, project: &str, name: &str, build: &str, branch: &str) -> String {
    let (st, l) = s
        .post(
            &format!("/api/v1/projects/{project}/launches"),
            ADMIN_TOKEN,
            json!({"name": name, "vcs": {"branch": branch}, "attributes": [{"key": "build", "value": build}]}),
        )
        .await;
    assert!(st.is_success(), "{l}");
    let lid = l["id"].as_str().unwrap().to_string();
    results(
        s,
        project,
        &lid,
        vec![
            result("a", "passed", "Cart", None),
            result("b", "failed", "Cart", Some("boom")),
        ],
    )
    .await;
    wait_total(s, project, &lid, 2).await;
    lid
}

#[tokio::test]
async fn the_cumulative_trend_sums_launches_by_attribute_value() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("cum").await;
    db.hold_jobs(&["launch.attributes.index"]).await;
    let l1 = launch_with(&s, "cum", "one", "1.0", "main").await;
    launch_with(&s, "cum", "two", "1.0", "main").await;
    launch_with(&s, "cum", "three", "1.1", "dev").await;

    let by_build = |extra: Value| {
        let mut settings = json!({"attribute": "build"});
        for (k, v) in extra.as_object().unwrap() {
            settings[k] = v.clone();
        }
        widget_with("c1", "cumulative-trend", settings)
    };
    let (st, before) = preview(&s, "cum", vec![by_build(json!({}))]).await;
    assert_eq!(st, StatusCode::OK, "{before}");
    assert_eq!(
        before["widgets"]["c1"]["data"]["values"],
        json!([]),
        "nothing is indexed before the job ran"
    );

    let store = Store::new(db.pool.clone());
    assert_eq!(
        store
            .index_launch_attributes(Duration::from_secs(30))
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        store
            .index_launch_attributes(Duration::from_secs(30))
            .await
            .unwrap(),
        0,
        "launches are indexed once"
    );
    let (_, d) = preview(&s, "cum", vec![by_build(json!({}))]).await;
    let data = &d["widgets"]["c1"]["data"];
    assert_eq!(data["kind"], "cumulative");
    assert_eq!(data["attribute"], "build");
    let values = data["values"].as_array().unwrap();
    assert_eq!(values.len(), 2, "{data}");
    assert_eq!(values[0]["value"], "1.0", "oldest value first");
    assert_eq!(values[0]["launches"], 2);
    assert_eq!(values[0]["total"], 4);
    assert_eq!(values[0]["passed"], 2);
    assert_eq!(values[0]["failed"], 2);
    assert_eq!(values[1]["value"], "1.1");
    assert_eq!(values[1]["launches"], 1);

    let (_, d) = preview(&s, "cum", vec![by_build(json!({"branch": "dev"}))]).await;
    let values = d["widgets"]["c1"]["data"]["values"].as_array().unwrap();
    assert_eq!(values.len(), 1, "the branch filter applies: {d}");
    assert_eq!(values[0]["value"], "1.1");
    let (_, d) = preview(&s, "cum", vec![by_build(json!({"limit": 1}))]).await;
    assert_eq!(
        d["widgets"]["c1"]["data"]["values"][0]["value"], "1.1",
        "the most recent values are kept"
    );
    let (_, d) = preview(
        &s,
        "cum",
        vec![widget_with("c1", "cumulative-trend", json!({}))],
    )
    .await;
    assert_eq!(d["widgets"]["c1"]["data"]["values"], json!([]));

    let (st, p) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/cum/launches/{l1}"),
            Some(ADMIN_TOKEN),
            Some(json!({"attributes": [{"key": "build", "value": "1.1"}]})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(
        store
            .index_launch_attributes(Duration::from_secs(30))
            .await
            .unwrap(),
        1,
        "a patched launch is indexed again"
    );
    let (_, d) = preview(&s, "cum", vec![by_build(json!({}))]).await;
    let values = d["widgets"]["c1"]["data"]["values"].as_array().unwrap();
    assert_eq!(values.len(), 2);
    assert_eq!(
        values.iter().find(|v| v["value"] == "1.0").unwrap()["launches"],
        1
    );
    assert_eq!(
        values.iter().find(|v| v["value"] == "1.1").unwrap()["launches"],
        2
    );

    let (st, b) = preview(
        &s,
        "cum",
        vec![widget_with(
            "c1",
            "cumulative-trend",
            json!({"attribute": "x".repeat(65)}),
        )],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let many: Vec<Value> = (0..4)
        .map(|i| {
            widget_with(
                &format!("c{i}"),
                "cumulative-trend",
                json!({"attribute": format!("a{i}")}),
            )
        })
        .collect();
    let (st, b) = preview(&s, "cum", many).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "at most three groupings: {b}");

    let indexed: i64 = sqlx::query_scalar("select count(*) from launch_attr_index")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(indexed, 3);
    sqlx::query("delete from launches")
        .execute(&db.pool)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("select count(*) from launch_attr_index")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "the index goes with its launches");
}

#[tokio::test]
async fn unique_bugs_come_from_tracker_issue_links() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("bug").await;
    s.project("other").await;
    let pid = |k: &'static str| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, uuid::Uuid>("select id from projects where key = $1")
                .bind(k)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    let (bug, other) = (pid("bug").await, pid("other").await);
    let add = |project: uuid::Uuid,
               key: &'static str,
               category: &'static str,
               age: i32,
               results: i32,
               series: i32| {
        let pool = db.pool.clone();
        async move {
            let id = uuid::Uuid::now_v7();
            sqlx::query(
                "insert into tracker_issues (id, project_id, tracker_id, tracker_type, issue_key, url, title, state, \
                   category, created_by, created_at, updated_at) \
                 values ($1, $2, 'gl', 'gitlab', $3, $4, $5, $6, $7, 'user:x', now() - make_interval(days => $8), \
                   now() - make_interval(days => $8))",
            )
            .bind(id)
            .bind(project)
            .bind(key)
            .bind(format!("https://git.example.com/g/a/-/issues/{}", key.trim_start_matches('#')))
            .bind(format!("Bug {key} <script>alert(1)</script>"))
            .bind(if category == "closed" { "closed" } else { "opened" })
            .bind(category)
            .bind(age)
            .execute(&pool)
            .await
            .unwrap();
            for _ in 0..results {
                sqlx::query("insert into tracker_issue_links (id, issue_id, project_id, result_id, created_by) values ($1, $2, $3, $4, 'user:x')")
                    .bind(uuid::Uuid::now_v7())
                    .bind(id)
                    .bind(project)
                    .bind(uuid::Uuid::now_v7())
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            if series > 0 {
                let ids: Vec<uuid::Uuid> = (0..series).map(|_| uuid::Uuid::now_v7()).collect();
                sqlx::query("insert into tracker_issue_links (id, issue_id, project_id, fingerprint, series_ids, created_by) values ($1, $2, $3, 'fp', $4, 'user:x')")
                    .bind(uuid::Uuid::now_v7())
                    .bind(id)
                    .bind(project)
                    .bind(&ids)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        }
    };
    add(bug, "#1", "open", 90, 2, 0).await;
    add(bug, "#2", "open", 1, 0, 5).await;
    add(bug, "#3", "closed", 2, 1, 0).await;
    add(bug, "#4", "closed", 90, 9, 0).await;
    add(bug, "#5", "creating", 0, 0, 0).await;
    add(other, "#9", "open", 0, 3, 0).await;

    let (st, d) = preview(&s, "bug", vec![widget("u1", "unique-bugs")]).await;
    assert_eq!(st, StatusCode::OK, "{d}");
    let data = &d["widgets"]["u1"]["data"];
    assert_eq!(data["kind"], "bugs");
    let keys: Vec<&str> = data["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        ["#2", "#1", "#3"],
        "open bugs first by the tests that point to them; a closed one only inside the window; no other project, no unfinished create: {data}"
    );
    let first = &data["items"][0];
    assert_eq!(first["tests"], 5);
    assert_eq!(first["links"], 1);
    assert_eq!(first["closed"], false);
    assert_eq!(first["trackerType"], "gitlab");
    assert_eq!(data["items"][2]["closed"], true);
    assert!(
        first["title"].as_str().unwrap().contains("<script>"),
        "text is data, the UI escapes it"
    );

    let (_, d) = preview(
        &s,
        "bug",
        vec![widget_with(
            "u1",
            "unique-bugs",
            json!({"days": 365, "limit": 2}),
        )],
    )
    .await;
    assert_eq!(
        d["widgets"]["u1"]["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn the_activity_stream_is_shown_only_to_audit_readers_and_never_shared() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("act").await;
    s.project("quiet").await;
    let viewer = s.token("viewer-x", "viewer", &["act"]).await;
    let b = create(
        &s,
        ADMIN_TOKEN,
        "act",
        "Activity",
        vec![
            widget("w1", "launches-table"),
            widget_with("w2", "activity-stream", json!({"limit": 5})),
        ],
    )
    .await;
    let data = path("act", &format!("/{}/data", b["id"].as_str().unwrap()));

    let (st, d) = s.get(&data, ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{d}");
    assert_eq!(d["widgets"]["w2"]["status"], "ok", "{d}");
    let items = d["widgets"]["w2"]["data"]["items"].as_array().unwrap();
    assert!(items.len() <= 5);
    assert!(
        items.iter().any(|i| i["action"] == "dashboard.create"),
        "{items:?}"
    );
    for i in items {
        assert!(
            i.get("detail").is_none(),
            "no audit detail in a widget: {i}"
        );
    }

    let (st, v) = s.get(&data, &viewer).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["widgets"]["w1"]["status"], "ok");
    assert_eq!(v["widgets"]["w2"]["status"], "error", "{v}");
    assert!(
        v["widgets"]["w2"]["message"]
            .as_str()
            .unwrap()
            .contains("audit:read")
    );
    assert!(
        !v.to_string().contains("dashboard.create"),
        "nothing of the stream leaks through the shared data: {v}"
    );
    let (_, again) = s.get(&data, ADMIN_TOKEN).await;
    assert_eq!(again["widgets"]["w2"]["status"], "ok");
    assert_eq!(
        again["generatedAt"], v["generatedAt"],
        "the rest is still shared"
    );

    let (_, q) = preview(&s, "quiet", vec![widget("w2", "activity-stream")]).await;
    assert_eq!(q["widgets"]["w2"]["status"], "ok");
    assert!(
        !q["widgets"]["w2"].to_string().contains("dashboard.create"),
        "another project's records are not shown: {q}"
    );

    let many: Vec<Value> = (0..3)
        .map(|i| widget_with(&format!("a{i}"), "activity-stream", json!({"limit": i + 1})))
        .collect();
    let (st, b) = preview(&s, "act", many).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
}
