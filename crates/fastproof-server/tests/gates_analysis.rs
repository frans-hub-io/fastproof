//! Gate analytics of FastProof Analysis over HTTP: the metrics `newErrors`, `toInvestigate` and
//! `total`, the gate history with its aggregates and filters, and the simulation of a draft gate
//! (it equals the real evaluation of the stored launches and writes nothing). Skipped without
//! `FASTPROOF_TEST_DATABASE_URL`.
//!
//! The integrations loop sleeps for an hour after its first round, so it never evaluates the
//! launches of these tests: the gates are evaluated on demand (`GET .../gate`) or stored by the
//! test itself.

mod common;

use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

async fn start(db: &TestDb, extra: &[(&str, &str)]) -> Server {
    Server::start_cfg(db, extra, |c| {
        c.integrations.poll = Duration::from_secs(3600);
    })
    .await
}

async fn project_id(db: &TestDb, key: &str) -> Uuid {
    sqlx::query_scalar("select id from projects where key = $1")
        .bind(key)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

async fn set_gates(db: &TestDb, key: &str, gates: Value) {
    let id = project_id(db, key).await;
    fastproof_store::Store::new(db.pool.clone())
        .merge_project_settings(id, json!({"gates": gates}))
        .await
        .unwrap();
}

/// Creates a finished launch on `branch` with results `(name, status, message)`.
async fn launch(s: &Server, project: &str, branch: &str, items: &[(&str, &str, &str)]) -> String {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches"),
            ADMIN_TOKEN,
            json!({"name": format!("run on {branch}"), "vcs": {"branch": branch}}),
        )
        .await;
    assert!(st.is_success(), "{st} {b}");
    let id = b["id"].as_str().unwrap().to_string();
    if !items.is_empty() {
        let body: Vec<Value> = items
            .iter()
            .map(|(n, status, msg)| {
                let mut r = json!({"id": format!("{id}-{n}"), "name": n, "fullName": format!("suite.{n}"),
                    "status": status, "start": 1_795_249_079_000i64, "stop": 1_795_249_080_000i64});
                if *status == "failed" || *status == "broken" {
                    r["error"] = json!({"message": msg});
                }
                r
            })
            .collect();
        let (st, b) = s
            .post(
                &format!("/api/v1/projects/{project}/launches/{id}/results"),
                ADMIN_TOKEN,
                Value::Array(body),
            )
            .await;
        assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    }
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{id}/finish"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    id
}

/// The on-demand gate of a launch, once its uploads are processed.
async fn gate(s: &Server, project: &str, launch: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (st, g) = s
            .get(
                &format!("/api/v1/projects/{project}/launches/{launch}/gate"),
                ADMIN_TOKEN,
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{g}");
        if g["status"] != "pending" {
            return g;
        }
        assert!(Instant::now() < deadline, "the gate stayed pending");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn actuals(g: &Value) -> Vec<Value> {
    g["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["actual"].clone())
        .collect()
}

async fn simulate(s: &Server, project: &str, token: &str, body: Value) -> (StatusCode, Value) {
    s.post(
        &format!("/api/v1/projects/{project}/gates/simulate"),
        token,
        body,
    )
    .await
}

async fn history(s: &Server, project: &str, token: &str, q: &str) -> (StatusCode, Value) {
    s.get(
        &format!("/api/v1/projects/{project}/gates/history?{q}"),
        token,
    )
    .await
}

async fn cli(s: &Server, project: &str, launch: &str) -> (i32, String) {
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_fastproof"))
        .arg("gate")
        .args(["--url", &s.base, "--project", project, "--id", launch])
        .env("FASTPROOF_TOKEN", ADMIN_TOKEN)
        .env_remove("FASTPROOF_LAUNCH")
        .env_remove("GITLAB_CI")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("JENKINS_URL")
        .output()
        .await
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

#[tokio::test]
async fn the_new_metrics_count_what_the_contract_says() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = start(&db, &[]).await;
    s.project("gm").await;
    // a: passed, b: failed (defect of group toInvestigate), c: failed and muted, d: broken with a
    // product bug, e: skipped, f: failed without a defect, g: broken without a defect.
    let l1 = launch(
        &s,
        "gm",
        "main",
        &[
            ("a", "passed", ""),
            ("b", "failed", "first boom"),
            ("c", "failed", "second boom"),
            ("d", "broken", "third boom"),
            ("e", "skipped", ""),
            ("f", "failed", "fourth boom"),
            ("g", "broken", "fifth boom"),
        ],
    )
    .await;
    let _ = gate(&s, "gm", &l1).await;
    for q in [
        "update results set muted = true where launch_id = $1::uuid and name = 'c'",
        "update results set defect = '{\"group\": \"productBug\"}'::jsonb where launch_id = $1::uuid and name = 'd'",
        "update results set defect = '{\"group\": \"toInvestigate\"}'::jsonb where launch_id = $1::uuid and name = 'b'",
    ] {
        let r = sqlx::query(q).bind(&l1).execute(&db.pool).await.unwrap();
        assert_eq!(r.rows_affected(), 1, "{q}");
    }
    set_gates(
        &db,
        "gm",
        json!([{"name": "analysis", "rules": [
            {"metric": "toInvestigate", "op": "lte", "value": 3},
            {"metric": "total", "op": "gte", "value": 6},
            {"metric": "failed", "op": "lte", "value": 100},
        ]}]),
    )
    .await;
    let g = gate(&s, "gm", &l1).await;
    assert_eq!(
        actuals(&g),
        vec![json!(3.0), json!(6.0), json!(2.0)],
        "toInvestigate: b, f, g (c is muted, d is a product bug); total: all but skipped; failed: b, f: {g}"
    );
    assert_eq!(g["status"], "passed");

    // newErrors: distinct fresh fingerprints of the launch.
    let l2 = launch(
        &s,
        "gm",
        "feature",
        &[
            ("x", "failed", "novel alpha"),
            ("y", "failed", "novel beta"),
            ("y2", "failed", "novel beta"),
            ("z", "passed", ""),
        ],
    )
    .await;
    set_gates(
        &db,
        "gm",
        json!([{"name": "errors", "rules": [
            {"metric": "newErrors", "op": "lte", "value": 1},
            {"metric": "newErrors", "op": "lte", "value": 5, "baseline": "main"},
            {"metric": "toInvestigate", "op": "lte", "value": 2},
        ]}]),
    )
    .await;
    let g = gate(&s, "gm", &l2).await;
    assert_eq!(actuals(&g), vec![json!(2.0), json!(2.0), json!(3.0)], "{g}");
    assert_eq!(g["status"], "failed");
    assert_eq!(g["results"][0]["ok"], false);
    assert_eq!(g["results"][1]["ok"], true);
    assert_eq!(g["results"][2]["ok"], false);

    // `fastproof gate` prints the new metrics and exits 1 for the failed gate.
    let (code, out) = cli(&s, "gm", &l2).await;
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("newErrors lte 1.0 (actual 2.0)"), "{out}");
    assert!(out.contains("newErrors lte 5.0 vs main"), "{out}");
    assert!(out.contains("toInvestigate lte 2.0 (actual 3.0)"), "{out}");
}

/// Writes a stored gate evaluation of `launch`.
async fn store_gate(
    db: &TestDb,
    project: Uuid,
    launch: &str,
    at: DateTime<Utc>,
    state: &str,
    status: &str,
    results: &[(&str, &str, bool)],
) {
    let rs: Vec<Value> = results
        .iter()
        .map(|(gate, metric, ok)| {
            json!({"gate": gate, "rule": {"metric": metric, "op": "lte", "value": 0},
                "actual": if *ok { 0.0 } else { 3.0 }, "expected": 0.0, "ok": ok})
        })
        .collect();
    sqlx::query(
        "insert into launch_gates (launch_id, project_id, state, status, report, evaluated_at) \
         values ($1::uuid, $2, $3, $4, $5, $6) \
         on conflict (launch_id) do update set state = excluded.state, status = excluded.status, \
           report = excluded.report, evaluated_at = excluded.evaluated_at",
    )
    .bind(launch)
    .bind(project)
    .bind(state)
    .bind(status)
    .bind(json!({"status": status, "results": rs, "launchId": launch, "evaluatedAt": at}))
    .bind(at)
    .execute(&db.pool)
    .await
    .unwrap();
}

fn ids(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["launchId"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn the_history_aggregates_filters_and_pages() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = start(&db, &[]).await;
    s.project("gh").await;
    s.project("gx").await;
    let pid = project_id(&db, "gh").await;
    let viewer = s.token("v", "viewer", &["gh"]).await;
    let outsider = s.token("out", "editor", &["gx"]).await;
    let anchor = Utc
        .timestamp_opt(Utc::now().timestamp() - 10 * 86_400, 0)
        .unwrap();
    let at = |min: i64| anchor + chrono::Duration::minutes(min);

    // (branch, minute, default gate: None = not evaluated / Some(failed metrics), slow gate ok)
    let plan: [(&str, i64, &[&str], bool); 9] = [
        ("main", 0, &[], true),
        ("main", 10, &["failed"], true),
        ("main", 20, &["failed", "newErrors"], false),
        ("main", 40, &[], false),
        ("main", 50, &["newErrors"], false),
        ("main", 55, &[], false),
        ("main", 70, &["newErrors"], false),
        ("dev", 5, &["passRate"], false),
        ("dev", 7, &[], false),
    ];
    let mut launches = Vec::new();
    for (branch, min, failed, with_slow) in plan {
        let l = launch(&s, "gh", branch, &[]).await;
        let mut rs: Vec<(&str, &str, bool)> = vec![("default", "total", true)];
        if !failed.is_empty() {
            rs.clear();
            for m in failed {
                rs.push(("default", m, false));
            }
        }
        if with_slow {
            rs.push(("slow", "duration", true));
        }
        let status = if failed.is_empty() {
            "passed"
        } else {
            "failed"
        };
        store_gate(&db, pid, &l, at(min), "done", status, &rs).await;
        launches.push((l, min));
    }
    // not part of the history: pending, still waiting, and a run without any gate rule
    let l = launch(&s, "gh", "main", &[]).await;
    store_gate(
        &db,
        pid,
        &l,
        at(80),
        "done",
        "pending",
        &[("default", "failed", false)],
    )
    .await;
    let l = launch(&s, "gh", "main", &[]).await;
    store_gate(
        &db,
        pid,
        &l,
        at(81),
        "waiting",
        "failed",
        &[("default", "failed", false)],
    )
    .await;
    let l = launch(&s, "gh", "main", &[]).await;
    store_gate(&db, pid, &l, at(82), "done", "passed", &[]).await;
    // another project's evaluation never shows up
    let other = launch(&s, "gx", "main", &[]).await;
    store_gate(
        &db,
        project_id(&db, "gx").await,
        &other,
        at(15),
        "done",
        "failed",
        &[("default", "failed", false)],
    )
    .await;

    let mut newest_first: Vec<(String, i64)> = launches.clone();
    newest_first.sort_by_key(|l| std::cmp::Reverse(l.1));
    let expected: Vec<String> = newest_first.iter().map(|l| l.0.clone()).collect();

    let (st, h) = history(&s, "gh", ADMIN_TOKEN, "").await;
    assert_eq!(st, StatusCode::OK, "{h}");
    assert_eq!(
        ids(&h),
        expected,
        "newest first, only done passed/failed runs"
    );
    assert!(h["next"].is_null());
    assert_eq!(h["items"][0]["branch"], "main");
    assert_eq!(h["items"][0]["status"], "failed");
    assert_eq!(
        h["items"][0]["failed"],
        json!([{"gate": "default", "metric": "newErrors", "actual": 3.0, "expected": 0.0}])
    );
    assert_eq!(h["items"][8]["status"], "passed");
    assert_eq!(h["items"][8]["failed"], json!([]));
    let gates = h["gates"].as_array().unwrap();
    assert_eq!(gates.len(), 2, "{h}");
    assert_eq!(gates[0]["gate"], "default");
    assert_eq!(gates[0]["evaluations"], 9);
    assert_eq!(gates[0]["failed"], 5);
    assert_eq!(gates[0]["flips"], 6);
    assert_eq!(gates[0]["meanSecondsToGreen"], 740.0);
    assert_eq!(gates[0]["topFailingMetric"], "newErrors");
    assert_eq!(gates[1]["gate"], "slow");
    assert_eq!(gates[1]["evaluations"], 2);
    assert_eq!(gates[1]["failed"], 0);
    assert_eq!(gates[1]["flips"], 0);
    assert!(gates[1]["meanSecondsToGreen"].is_null());
    assert!(gates[1]["topFailingMetric"].is_null());
    assert_eq!(
        h["branches"],
        json!([
            {"branch": "main", "evaluations": 7, "failed": 4},
            {"branch": "dev", "evaluations": 2, "failed": 1},
        ])
    );

    // filters
    let (_, h) = history(&s, "gh", &viewer, "gate=slow").await;
    assert_eq!(ids(&h), vec![launches[1].0.clone(), launches[0].0.clone()]);
    assert_eq!(h["gates"].as_array().unwrap().len(), 1);
    assert_eq!(h["items"][0]["failed"], json!([]));
    let (_, h) = history(&s, "gh", ADMIN_TOKEN, "gate=nothing").await;
    assert_eq!(ids(&h), Vec::<String>::new());
    let (_, h) = history(&s, "gh", ADMIN_TOKEN, "branch=dev").await;
    assert_eq!(ids(&h), vec![launches[8].0.clone(), launches[7].0.clone()]);
    assert_eq!(h["gates"][0]["evaluations"], 2);
    assert_eq!(h["gates"][0]["flips"], 1);
    assert_eq!(h["gates"][0]["meanSecondsToGreen"], 120.0);
    assert_eq!(h["gates"][0]["topFailingMetric"], "passRate");
    let from = at(8).to_rfc3339();
    let to = at(45).to_rfc3339();
    let (st, h) = history(
        &s,
        "gh",
        ADMIN_TOKEN,
        &format!(
            "from={}&to={}",
            from.replace('+', "%2B"),
            to.replace('+', "%2B")
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{h}");
    assert_eq!(
        ids(&h),
        vec![
            launches[3].0.clone(),
            launches[2].0.clone(),
            launches[1].0.clone()
        ]
    );
    // a gate filter shows that gate's verdict, not the launch's
    let (_, h) = history(&s, "gh", ADMIN_TOKEN, "gate=slow&branch=main").await;
    assert!(
        h["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["status"] == "passed")
    );

    // paging: the aggregates come with the first page only, pages do not overlap
    let mut seen = Vec::new();
    let mut before = String::new();
    let mut pages = 0;
    loop {
        let (st, h) = history(&s, "gh", ADMIN_TOKEN, &format!("limit=4{before}")).await;
        assert_eq!(st, StatusCode::OK, "{h}");
        assert_eq!(h["gates"].as_array().unwrap().is_empty(), pages > 0, "{h}");
        seen.extend(ids(&h));
        pages += 1;
        match h["next"].as_str() {
            Some(n) => before = format!("&before={n}"),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen, expected);

    // validation
    for q in [
        "from=yesterday",
        "to=tomorrow",
        "limit=0",
        "limit=501",
        "before=junk",
        "gate=",
        "from=2030-02-01T00:00:00Z&to=2030-01-01T00:00:00Z",
        "from=2020-01-01T00:00:00Z&to=2022-01-01T00:00:00Z",
    ] {
        let (st, b) = history(&s, "gh", ADMIN_TOKEN, q).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{q}: {b}");
    }

    // permissions and isolation
    let (st, _) = history(&s, "gh", &outsider, "").await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = history(&s, "nope", &outsider, "").await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "the same answer for a project that does not exist"
    );
    let (st, _) = s
        .req(
            reqwest::Method::GET,
            "/api/v1/projects/gh/gates/history",
            None,
        )
        .send()
        .await
        .map(|r| (r.status(), ()))
        .unwrap();
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, h) = history(&s, "gx", &outsider, "").await;
    assert_eq!(st, StatusCode::OK, "{h}");
    assert_eq!(ids(&h), vec![other]);
}

fn gate_def() -> Value {
    json!({"name": "draft", "rules": [
        {"metric": "failed", "op": "lte", "value": 1},
        {"metric": "newFailures", "op": "lte", "value": 0},
        {"metric": "toInvestigate", "op": "lte", "value": 1},
        {"metric": "total", "op": "gte", "value": 2},
        {"metric": "newErrors", "op": "lte", "value": 1},
        {"metric": "newErrors", "op": "lte", "value": 1, "baseline": "main"},
        {"metric": "passRate", "op": "gte", "value": -20, "baseline": "main"},
        {"metric": "failed", "op": "lte", "value": 1, "baseline": "main"},
    ]})
}

#[tokio::test]
async fn the_simulation_equals_the_real_evaluation_and_writes_nothing() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = start(&db, &[("FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE", "6")]).await;
    s.project("gs").await;
    s.project("go").await;
    let editor = s.token("ed", "editor", &["gs"]).await;
    let outsider = s.token("out", "editor", &["go"]).await;
    type Items<'a> = &'a [(&'a str, &'a str, &'a str)];
    let plan: [(&str, Items); 8] = [
        ("main", &[("a", "passed", ""), ("b", "failed", "red one")]),
        (
            "main",
            &[
                ("a", "passed", ""),
                ("b", "failed", "red one"),
                ("c", "passed", ""),
            ],
        ),
        (
            "feat",
            &[
                ("a", "passed", ""),
                ("b", "failed", "red one"),
                ("d", "failed", "red two"),
            ],
        ),
        (
            "main",
            &[
                ("a", "passed", ""),
                ("b", "passed", ""),
                ("c", "passed", ""),
            ],
        ),
        (
            "feat",
            &[
                ("a", "failed", "red three"),
                ("b", "broken", "red four"),
                ("c", "failed", "red one"),
            ],
        ),
        ("feat", &[]),
        ("release/1", &[("a", "passed", ""), ("b", "skipped", "")]),
        ("main", &[("a", "failed", "red five"), ("b", "passed", "")]),
    ];
    let mut launches = Vec::new();
    for (branch, items) in plan {
        launches.push(launch(&s, "gs", branch, items).await);
    }
    let other = launch(&s, "go", "main", &[("o", "failed", "other project")]).await;
    set_gates(&db, "gs", json!([gate_def()])).await;
    let mut real = Vec::new();
    for l in &launches {
        real.push(gate(&s, "gs", l).await);
    }
    let writes = |db: &TestDb| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_as::<_, (i64, i64, i64, i64)>(
                "select (select count(*) from launch_gates), (select count(*) from integration_deliveries), \
                 (select count(*) from triage_decisions), (select count(*) from results)",
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let before = writes(&db).await;

    let (st, sim) = simulate(
        &s,
        "gs",
        &editor,
        json!({"gate": gate_def(), "launches": 20}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sim}");
    assert_eq!(sim["skipped"], 0, "{sim}");
    assert_eq!(sim["evaluated"], 8);
    let items = sim["items"].as_array().unwrap();
    assert_eq!(items.len(), 8);
    let mut failed = 0;
    for (i, item) in items.iter().enumerate() {
        let idx = launches.len() - 1 - i;
        assert_eq!(item["launchId"], launches[idx].as_str(), "newest first");
        assert_eq!(item["launchNumber"], idx as i64 + 1);
        assert_eq!(
            item["status"], real[idx]["status"],
            "launch {idx}: {item} vs {}",
            real[idx]
        );
        assert_eq!(item["results"], real[idx]["results"], "launch {idx}");
        failed += usize::from(item["status"] == "failed");
    }
    assert_eq!(sim["failed"], failed);
    assert!(
        failed > 0 && failed < 8,
        "the fixture has both outcomes: {failed}"
    );
    assert_eq!(items[0]["branch"], "main");
    assert!(items[0]["createdAt"].is_string());

    // limits, branch filter, branch globs of the draft, disabled drafts
    let (_, b) = simulate(
        &s,
        "gs",
        &editor,
        json!({"gate": gate_def(), "launches": 3}),
    )
    .await;
    assert_eq!(
        ids(&b),
        launches.iter().rev().take(3).cloned().collect::<Vec<_>>()
    );
    let (_, b) = simulate(
        &s,
        "gs",
        &editor,
        json!({"gate": gate_def(), "branch": "feat"}),
    )
    .await;
    assert_eq!(b["evaluated"], 3);
    let mut one = gate_def();
    one["branches"] = json!(["release/*"]);
    one["enabled"] = json!(false);
    let (_, b) = simulate(&s, "gs", &editor, json!({"gate": one})).await;
    assert_eq!(
        (b["evaluated"].clone(), b["skipped"].clone()),
        (json!(1), json!(7)),
        "{b}"
    );

    assert_eq!(writes(&db).await, before, "a simulation writes nothing");

    // a launch whose results are gone cannot be evaluated: it is skipped
    sqlx::query("delete from results where launch_id = $1::uuid")
        .bind(&launches[1])
        .execute(&db.pool)
        .await
        .unwrap();
    let before = writes(&db).await;
    let (_, b) = simulate(
        &s,
        "gs",
        &editor,
        json!({"gate": gate_def(), "launches": 20}),
    )
    .await;
    assert_eq!(
        (b["evaluated"].clone(), b["skipped"].clone()),
        (json!(7), json!(1)),
        "{b}"
    );
    assert!(!ids(&b).contains(&launches[1]));
    assert!(
        !ids(&b).contains(&other),
        "other projects' launches are never read"
    );

    // validation
    for body in [
        json!({"gate": gate_def(), "launches": 0}),
        json!({"gate": gate_def(), "launches": 201}),
        json!({"gate": {"name": "x", "rules": []}}),
        json!({"gate": {"name": "x", "rules": [{"metric": "nope", "op": "lte", "value": 1}]}}),
        json!({"gate": {"name": "x", "rules": [{"metric": "failed", "op": "lte", "value": 1, "extra": 1}]}}),
        json!({"gate": null}),
        json!({"gate": gate_def(), "branch": ""}),
    ] {
        let (st, b) = simulate(&s, "gs", ADMIN_TOKEN, body.clone()).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}: {b}");
    }
    let (st, _) = simulate(
        &s,
        "gs",
        ADMIN_TOKEN,
        json!({"gate": gate_def(), "unknown": 1}),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    assert_eq!(writes(&db).await, before, "a simulation writes nothing");

    // permissions: the project scope first, the same answer for projects that do not exist
    for p in ["gs", "nope"] {
        let (st, _) = simulate(&s, p, &outsider, json!({"gate": gate_def()})).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{p}");
    }
    let (st, b) = simulate(&s, "go", &outsider, json!({"gate": gate_def()})).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(ids(&b), vec![other]);

    // the multiplying-write rate: 6 per minute per caller, six were used above by the editor
    let (st, _) = simulate(
        &s,
        "go",
        &outsider,
        json!({"gate": gate_def(), "launches": 1}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let mut limited = false;
    for _ in 0..8 {
        let (st, _) = simulate(
            &s,
            "go",
            &outsider,
            json!({"gate": gate_def(), "launches": 1}),
        )
        .await;
        if st == StatusCode::TOO_MANY_REQUESTS {
            limited = true;
            break;
        }
    }
    assert!(
        limited,
        "tm_rate admits a limited number of simulations a minute"
    );
}

#[tokio::test]
async fn two_hundred_launches_simulate_quickly() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = start(&db, &[]).await;
    s.project("gb").await;
    for i in 0..210 {
        let branch = if i % 3 == 0 { "feat" } else { "main" };
        let items: Vec<(&str, &str, &str)> = vec![
            ("a", "passed", ""),
            (
                "b",
                if i % 4 == 0 { "failed" } else { "passed" },
                "bench red",
            ),
            (
                "c",
                if i % 7 == 0 { "broken" } else { "passed" },
                "bench broken",
            ),
        ];
        launch(&s, "gb", branch, &items).await;
    }
    let started = Instant::now();
    let (st, sim) = simulate(
        &s,
        "gb",
        ADMIN_TOKEN,
        json!({"gate": gate_def(), "launches": 200}),
    )
    .await;
    let took = started.elapsed();
    assert_eq!(st, StatusCode::OK, "{sim}");
    assert_eq!(sim["evaluated"], 200, "{sim}");
    eprintln!("simulation of 200 launches: {took:?}");
    assert!(took < Duration::from_secs(5), "{took:?}");
}

async fn put(s: &Server, doc: Value) -> (StatusCode, Value) {
    s.call(
        reqwest::Method::PUT,
        "/api/v1/projects/gc/settings",
        Some(ADMIN_TOKEN),
        Some(doc),
    )
    .await
}

#[tokio::test]
async fn new_metrics_wait_for_the_analysis_gate_in_settings() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut closed = start(&db, &[("FASTPROOF_ANALYSIS_GATE", "closed")]).await;
    closed.project("gc").await;
    let (st, b) = put(
        &closed,
        json!({"gates": [{"name": "g", "rules": [{"metric": "toInvestigate", "op": "lte", "value": 0}]}]}),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "upgrade_in_progress");
    assert_eq!(b["details"]["what"], "gates.toInvestigate");
    let (st, b) = put(
        &closed,
        json!({"gates": [{"name": "g", "rules": [{"metric": "failed", "op": "lte", "value": 0}]}]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "old metrics pass a closed gate: {b}");
    closed.stop().await;

    let open = start(&db, &[("FASTPROOF_ANALYSIS_GATE", "open")]).await;
    let (st, b) = put(
        &open,
        json!({"gates": [{"name": "g", "rules": [{"metric": "total", "op": "gt", "value": 0}]}]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
}

/// The first page of the history computes the aggregates of the whole window: it takes a heavy
/// slot of the caller (429 when they are taken); later pages carry no aggregates and do not.
#[tokio::test]
async fn the_history_aggregates_are_a_heavy_read() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = start(&db, &[("FASTPROOF_HEAVY_QUERIES_PER_CALLER", "1")]).await;
    s.project("gh").await;
    let mut h = axum::http::HeaderMap::new();
    h.insert(
        "authorization",
        format!("Bearer {ADMIN_TOKEN}").parse().unwrap(),
    );
    let state = &s.running.as_ref().unwrap().state;
    let auth = state
        .platform
        .authenticate(&h, &axum::http::Method::GET)
        .await
        .unwrap();
    let held = state.guards.heavy(&auth).await.unwrap();
    let (st, b) = history(&s, "gh", ADMIN_TOKEN, "").await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{b}");
    let cursor = format!("{}_{}", Utc::now().timestamp_micros(), Uuid::nil().simple());
    let (st, b) = history(&s, "gh", ADMIN_TOKEN, &format!("before={cursor}")).await;
    assert_eq!(st, StatusCode::OK, "a later page is no heavy read: {b}");
    drop(held);
    let (st, b) = history(&s, "gh", ADMIN_TOKEN, "").await;
    assert_eq!(st, StatusCode::OK, "{b}");
}

/// A simulation has a wall-time budget: past it the request answers 503 `timeout` instead of
/// holding a heavy slot for as long as 200 large launches take.
#[tokio::test]
async fn a_simulation_stops_at_its_budget() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = start(&db, &[]).await;
    s.project("gt").await;
    launch(&s, "gt", "main", &[("a", "failed", "red")]).await;
    let state = &s.running.as_ref().unwrap().state;
    let project = state.store.project("gt").await.unwrap().unwrap();
    let mut settings = fastproof_server::integrations::settings::IntegrationSettings::parse(
        &json!({"gates": [gate_def()]}),
    )
    .unwrap();
    let gate = settings.gates.pop().unwrap();
    let e = fastproof_server::integrations::gates::simulate_within(
        state,
        &project,
        &gate,
        10,
        None,
        Duration::ZERO,
    )
    .await
    .unwrap_err();
    assert!(
        e.downcast_ref::<fastproof_server::integrations::gates::SimulationTooLong>()
            .is_some(),
        "{e:#}"
    );
    let api = fastproof_server::api::gates_history::simulation_error(e);
    let r = axum::response::IntoResponse::into_response(api);
    assert_eq!(r.status().as_u16(), 503);
    let sim = fastproof_server::integrations::gates::simulate_within(
        state,
        &project,
        &gate,
        10,
        None,
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    assert_eq!(sim.evaluated, 1);
}
