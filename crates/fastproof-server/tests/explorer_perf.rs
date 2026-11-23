//! Launch explorer at scale: the tree counted per placement key against the full scan, a
//! result with several values of a grouping label, the cache of finished launches and its
//! invalidation, the backfill of old results, and the environment of the Failures and Logs
//! tabs. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

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

fn result(id: &str, full_name: &str, status: &str, labels: Value) -> Value {
    json!({"id": id, "name": id, "fullName": full_name, "status": status,
        "start": 1795249075000i64, "stop": 1795249075000i64 + 100, "labels": labels})
}

fn l(name: &str, value: &str) -> Value {
    json!({"name": name, "value": value})
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

/// Counts, duration and the ids of the direct results of a node.
type NodeData = (Value, Value, Option<BTreeSet<String>>);

/// Every node by path: counts, duration and the ids of its direct results.
fn nodes(
    tree: &Value,
    ids_of: &dyn Fn(&Value) -> Option<BTreeSet<String>>,
) -> BTreeMap<Vec<String>, NodeData> {
    let mut out = BTreeMap::new();
    let mut stack = vec![tree];
    while let Some(n) = stack.pop() {
        let path: Vec<String> = n["path"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap().to_string())
            .collect();
        out.insert(
            path,
            (n["counts"].clone(), n["durationMs"].clone(), ids_of(n)),
        );
        stack.extend(n["children"].as_array().unwrap().iter());
    }
    out
}

fn ids(list: &Value, field: &str) -> Option<BTreeSet<String>> {
    list.as_array().map(|a| {
        a.iter()
            .map(|t| {
                if field.is_empty() {
                    t.as_str().unwrap().to_string()
                } else {
                    t[field].as_str().unwrap().to_string()
                }
            })
            .collect()
    })
}

/// The tree with tests (counted per key, groups inlined or paged) holds the same groups,
/// counts, durations and results as the tree with result ids (every result read).
#[tokio::test]
async fn counted_tree_matches_the_full_scan() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("cnt").await;
    let launch = s.launch("cnt", None).await;
    let lid = launch["id"].as_str().unwrap();
    let base = format!("/api/v1/projects/cnt/launches/{lid}");
    let sep = " \u{203a} ";
    let mut rs = vec![
        result(
            "pw1",
            &format!("e2e/cart.spec.ts{sep}Cart{sep}adds"),
            "passed",
            json!([l("owner", "ann"), l("epic", "Shop"), l("feature", "Cart")]),
        ),
        result(
            "pw2",
            &format!("e2e/cart.spec.ts{sep}Cart{sep}removes"),
            "failed",
            json!([l("owner", "bob"), l("epic", "Shop"), l("story", "Remove")]),
        ),
        result(
            "pw3",
            &format!("e2e/cart.spec.ts{sep}top"),
            "broken",
            json!([l("owner", "ann"), l("owner", "bob"), l("tag", "smoke")]),
        ),
        result(
            "ju1",
            "com.example.ShopTest#pays",
            "passed",
            json!([
                l("suite", "com.example.ShopTest"),
                l("package", "com.example"),
                l("severity", "critical")
            ]),
        ),
        result(
            "ju2",
            "com.example.ShopTest#refunds",
            "skipped",
            json!([
                l("suite", "com.example.ShopTest"),
                l("owner", ""),
                l("tag", "smoke"),
                l("tag", "slow")
            ]),
        ),
        result(
            "py1",
            "tests.test_cart.test_total",
            "passed",
            json!([l("tag", "smoke"), l("tag", "smoke")]),
        ),
        result("odd1", "#leading", "failed", json!([])),
        result(
            "odd2",
            &format!("a{sep}{sep}b"),
            "passed",
            json!([l("epic", ""), l("epic", "Later"), l("feature", "Cart")]),
        ),
        result(
            "odd3",
            &format!(
                "\u{442}\u{435}\u{441}\u{442}{sep}\u{41a}\u{43e}\u{448}\u{438}\u{43a}{sep}\u{434}\u{43e}\u{434}\u{430}\u{454}"
            ),
            "passed",
            json!([l("severity", "minor")]),
        ),
        result(
            "al1",
            "x.Allure#one",
            "passed",
            json!([
                l("parentSuite", "Web"),
                l("suite", "Cart"),
                l("subSuite", "Add"),
                l("owner", "carol")
            ]),
        ),
        result(
            "al2",
            "x.Allure#two",
            "failed",
            json!([
                l("parentSuite", "Web"),
                l("suite", "Cart"),
                l("owner", "carol"),
                l("owner", "carol")
            ]),
        ),
    ];
    for i in 0..40 {
        rs.push(result(
            &format!("bulk{i}"),
            &format!("com.example.Bulk{}Test#case{i}", i % 3),
            if i % 7 == 0 { "failed" } else { "passed" },
            json!([
                l("owner", ["ann", "bob", "dan"][i % 3]),
                l("tag", ["a", "b"][i % 2]),
                l("severity", "normal")
            ]),
        ));
    }
    post_results(&s, "cnt", lid, &rs).await;
    for by in [
        "suite",
        "feature",
        "package",
        "owner",
        "epic,feature,story",
        "tag,owner,severity",
        "label:feature",
        "severity,nothing,owner",
    ] {
        for filter in ["", "&status=failed,broken", "&tag=smoke"] {
            let q = format!("groupBy={}{filter}", enc(by));
            let (st, scan) = s.get(&format!("{base}/tree?{q}"), ADMIN_TOKEN).await;
            assert_eq!(st, StatusCode::OK, "{q}: {scan}");
            let (st, counted) = s
                .get(&format!("{base}/tree?includeTests=true&{q}"), ADMIN_TOKEN)
                .await;
            assert_eq!(st, StatusCode::OK, "{q}: {counted}");
            assert_eq!(scan["levels"], counted["levels"], "{q}");
            let a = nodes(&scan, &|n| ids(&n["resultIds"], ""));
            let b = nodes(&counted, &|n| ids(&n["tests"], "id"));
            assert_eq!(
                a.keys().collect::<Vec<_>>(),
                b.keys().collect::<Vec<_>>(),
                "{q}"
            );
            let levels = counted["levels"]
                .as_array()
                .map(|l| {
                    let names: Vec<&str> = l.iter().map(|x| x.as_str().unwrap()).collect();
                    match names.as_slice() {
                        [] => by.to_string(),
                        [one] if matches!(*one, "suite" | "feature" | "package") => {
                            format!("label:{one}")
                        }
                        _ => names.join(","),
                    }
                })
                .unwrap_or_else(|| by.to_string());
            for (path, (counts, duration, scanned)) in &a {
                let (c, d, tests) = &b[path];
                assert_eq!(counts, c, "{q} {path:?}");
                assert_eq!(duration, d, "{q} {path:?}");
                let want = scanned.clone().unwrap_or_default();
                let n = counted_node(&counted, path)["testCount"].as_i64().unwrap();
                assert_eq!(n as usize, want.len(), "{q} {path:?}");
                let got = match tests {
                    Some(t) => t.clone(),
                    None if n == 0 => BTreeSet::new(),
                    None => {
                        let (st, page) = s
                            .get(
                                &format!(
                                    "{base}/tree/tests?groupBy={}{filter}&path={}&limit=500",
                                    enc(&levels),
                                    enc(&serde_json::to_string(path).unwrap())
                                ),
                                ADMIN_TOKEN,
                            )
                            .await;
                        assert_eq!(st, StatusCode::OK, "{page}");
                        ids(&page["items"], "id").unwrap()
                    }
                };
                assert_eq!(got, want, "{q} {path:?}");
            }
        }
    }
}

fn counted_node<'a>(tree: &'a Value, path: &[String]) -> &'a Value {
    let mut n = tree;
    for seg in path {
        n = n["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == seg.as_str())
            .unwrap();
    }
    n
}

/// A result with two values of a grouping label is in the group of each (as in Allure 3),
/// once per value, and counted once in the groups above.
#[tokio::test]
async fn several_values_of_a_label_place_a_result_in_each_group() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("dup").await;
    let launch = s.launch("dup", None).await;
    let lid = launch["id"].as_str().unwrap();
    let base = format!("/api/v1/projects/dup/launches/{lid}");
    post_results(
        &s,
        "dup",
        lid,
        &[
            result(
                "both",
                "a.T#both",
                "failed",
                json!([
                    l("owner", "ann"),
                    l("owner", "bob"),
                    l("owner", "ann"),
                    l("tag", "smoke")
                ]),
            ),
            result(
                "ann",
                "a.T#ann",
                "passed",
                json!([l("owner", "ann"), l("tag", "smoke")]),
            ),
            result(
                "blank",
                "a.T#blank",
                "passed",
                json!([l("owner", ""), l("tag", "smoke")]),
            ),
        ],
    )
    .await;
    let (_, t) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=owner"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(t["counts"]["total"], 3, "{t}");
    let names = |n: &Value| -> Vec<String> {
        n["tests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect()
    };
    let ann = counted_node(&t, &["ann".into()]);
    let bob = counted_node(&t, &["bob".into()]);
    let none = counted_node(&t, &["".into()]);
    assert_eq!(names(ann), vec!["ann", "both"]);
    assert_eq!(names(bob), vec!["both"]);
    assert_eq!(names(none), vec!["blank"]);
    assert_eq!(ann["counts"]["total"], 2);

    let (_, t) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=tag,owner"),
            ADMIN_TOKEN,
        )
        .await;
    let smoke = counted_node(&t, &["smoke".into()]);
    assert_eq!(smoke["counts"]["total"], 3, "counted once above: {t}");
    assert_eq!(smoke["counts"]["failed"], 1);
    assert_eq!(
        counted_node(&t, &["smoke".into(), "bob".into()])["counts"]["total"],
        1
    );

    let (_, page) = s
        .get(
            &format!("{base}/tree/tests?groupBy=owner&path={}", enc("[\"bob\"]")),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(page["items"][0]["name"], "both", "{page}");
    let (_, f) = s.get(&format!("{base}/facets"), ADMIN_TOKEN).await;
    let owner = f["labels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == "owner")
        .unwrap();
    assert_eq!(owner["none"], 1, "an empty value is no value: {f}");
    let listed = s.results("dup", lid, "&owner=%2500").await;
    assert_eq!(listed.len(), 1);
    let listed = s.results("dup", lid, "&owner=bob").await;
    assert_eq!(listed.len(), 1);
}

async fn cache_rows(db: &TestDb, launch: &str) -> Vec<String> {
    sqlx::query_scalar("select key from explorer_cache where launch_id = $1::uuid order by key")
        .bind(launch)
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

/// Finished launches serve the tree, label names, filter counts and environments from the
/// cache, and every write to their results (late results, triage, mutes) is seen at once.
#[tokio::test]
async fn cache_of_finished_launches_follows_writes() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("cch").await;
    let launch = s.launch("cch", None).await;
    let lid = launch["id"].as_str().unwrap();
    let base = format!("/api/v1/projects/cch/launches/{lid}");
    post_results(
        &s,
        "cch",
        lid,
        &[
            result("a", "c.T#a", "failed", json!([l("owner", "ann")])),
            result("b", "c.T#b", "passed", json!([l("owner", "bob")])),
        ],
    )
    .await;
    let (_, running) = s.get(&format!("{base}/facets"), ADMIN_TOKEN).await;
    assert_eq!(running["counts"]["total"], 2);
    assert!(
        cache_rows(&db, lid).await.is_empty(),
        "a running launch is not cached"
    );
    finish(&s, "cch", lid).await;

    let reads = [
        format!("{base}/facets"),
        format!("{base}/labels"),
        format!("{base}/environments"),
        format!("{base}/tree?includeTests=true&groupBy=owner"),
    ];
    let mut first = Vec::new();
    for r in &reads {
        let (st, b) = s.get(r, ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::OK, "{r}: {b}");
        first.push(b);
    }
    assert_eq!(cache_rows(&db, lid).await.len(), 4);
    for (r, want) in reads.iter().zip(&first) {
        let (_, b) = s.get(r, ADMIN_TOKEN).await;
        assert_eq!(&b, want, "served from the cache: {r}");
    }
    let (_, filtered) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=owner&status=failed"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(filtered["counts"]["total"], 1);
    assert_eq!(
        cache_rows(&db, lid).await.len(),
        4,
        "filtered trees are not cached"
    );

    post_results(
        &s,
        "cch",
        lid,
        &[result(
            "late",
            "c.T#late",
            "broken",
            json!([l("owner", "cid"), l("severity", "critical")]),
        )],
    )
    .await;
    let (_, f) = s.get(&reads[0], ADMIN_TOKEN).await;
    assert_eq!(f["counts"]["total"], 3, "late result: {f}");
    assert_eq!(f["counts"]["broken"], 1);
    let (_, lb) = s.get(&reads[1], ADMIN_TOKEN).await;
    assert!(
        lb["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["name"] == "severity"),
        "{lb}"
    );
    let (_, e) = s.get(&reads[2], ADMIN_TOKEN).await;
    assert_eq!(e["items"][0]["counts"]["total"], 3);
    let (_, t) = s.get(&reads[3], ADMIN_TOKEN).await;
    assert_eq!(
        counted_node(&t, &["cid".into()])["counts"]["total"],
        1,
        "{t}"
    );
    let late = s.results("cch", lid, "&status=broken").await;
    let late_id = late[0]["id"].as_str().unwrap().to_string();
    for round in 0..2 {
        let (_, t) = s
            .get(
                &format!("{base}/tree?includeTests=true&reveal={late_id}"),
                ADMIN_TOKEN,
            )
            .await;
        let found = nodes(&t, &|n| ids(&n["tests"], "id"))
            .values()
            .any(|(_, _, tests)| tests.as_ref().is_some_and(|x| x.contains(&late_id)));
        assert!(found, "round {round}: the revealed result is inline: {t}");
    }

    let (st, types) = s
        .get("/api/v1/projects/cch/defect-types", ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{types}");
    let type_id = types["items"][0]["id"].as_str().unwrap().to_string();
    let failed = s.results("cch", lid, "&status=failed").await;
    let rid = failed[0]["id"].as_str().unwrap();
    let (st, b) = s
        .post(
            "/api/v1/projects/cch/triage",
            ADMIN_TOKEN,
            json!({"resultIds": [rid], "defectTypeId": type_id, "mute": {"reason": "known"}}),
        )
        .await;
    assert!(st.is_success(), "{st} {b}");
    let (_, f) = s.get(&reads[0], ADMIN_TOKEN).await;
    assert_eq!(f["counts"]["muted"], 1, "triage: {f}");
    assert!(
        f["defects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["value"] == type_id.as_str()),
        "{f}"
    );
    let (_, t) = s.get(&reads[3], ADMIN_TOKEN).await;
    assert_eq!(t["counts"]["muted"], 1, "{t}");

    let gen_before: i64 =
        sqlx::query_scalar("select gen from explorer_gen where launch_id = $1::uuid")
            .bind(lid)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(gen_before >= 3, "{gen_before}");
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/cch/launches/{lid}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    if st.is_success() {
        let left: i64 =
            sqlx::query_scalar("select count(*) from explorer_cache where launch_id = $1::uuid")
                .bind(lid)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let gone: bool =
            sqlx::query_scalar("select not exists (select 1 from launches where id = $1::uuid)")
                .bind(lid)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        if gone {
            assert_eq!(left, 0, "deleted with the launch");
        }
    }
}

/// Results stored before `label_values` and `attachment_kinds` existed are filled in batches
/// from a cursor, and the explorer finds them the same before and after.
#[tokio::test]
async fn backfill_fills_old_results_in_batches() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["explorer.backfill"]).await;
    s.project("bf").await;
    let launch = s.launch("bf", None).await;
    let lid = launch["id"].as_str().unwrap();
    let base = format!("/api/v1/projects/bf/launches/{lid}");
    let done = s
        .ingest(
            "bf",
            lid,
            read("playwright/results.json"),
            "?fileName=results.json",
        )
        .await;
    assert_eq!(done["status"], "done", "{done}");
    finish(&s, "bf", lid).await;
    let (_, before_tree) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=owner,tag"),
            ADMIN_TOKEN,
        )
        .await;
    let with_kinds: i64 = sqlx::query_scalar(
        "select count(*) from results where launch_id = $1::uuid and attachment_kinds <> 0",
    )
    .bind(lid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(with_kinds > 0);
    let (_, f) = s.get(&format!("{base}/facets"), ADMIN_TOKEN).await;
    let kinds_before = f["attachments"].clone();

    let (_, suites_before) = s
        .get(&format!("{base}/tree?includeTests=true"), ADMIN_TOKEN)
        .await;
    let stored_before: Vec<(Vec<String>, bool)> =
        sqlx::query_as("select suite_tree, suite_titled from results order by id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    sqlx::query("update results set label_values = null, attachment_kinds = 0, suite_tree = null, suite_titled = null")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("update fastproof_schema_migrations set installed_on = now() + interval '1 day' where version = 600")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("delete from analysis_state where name = 'explorer.results.backfill'")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("update explorer_gen set gen = gen + 1")
        .execute(&db.pool)
        .await
        .unwrap();
    let state = &s.running.as_ref().unwrap().state;
    let (_, t) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=owner,tag"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(
        t, before_tree,
        "read from labels while label_values is empty"
    );
    let (_, t) = s
        .get(&format!("{base}/tree?includeTests=true"), ADMIN_TOKEN)
        .await;
    assert_eq!(
        t, suites_before,
        "suite paths computed while suite_tree is empty"
    );
    let (_, f) = s.get(&format!("{base}/facets"), ADMIN_TOKEN).await;
    assert!(
        f["attachments"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["count"] == 0),
        "the cache saw the reset: {f}"
    );

    let total: i64 = sqlx::query_scalar("select count(*) from results")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let mut rounds = 0;
    loop {
        let p = state
            .store
            .backfill_explorer(3, Duration::ZERO)
            .await
            .unwrap();
        rounds += 1;
        assert!(p.scanned <= 3);
        if p.done {
            assert_eq!(p.total_scanned as i64, total);
            break;
        }
        assert!(rounds < 1000);
    }
    assert!(rounds as i64 >= total / 3, "one batch per call: {rounds}");
    let left: i64 = sqlx::query_scalar("select count(*) from results where label_values is null")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
    let same: bool = sqlx::query_scalar(
        "select bool_and(label_values = fastproof_label_values(labels)) from results",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(same);
    let after_kinds: i64 = sqlx::query_scalar(
        "select count(*) from results where launch_id = $1::uuid and attachment_kinds <> 0",
    )
    .bind(lid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(after_kinds, with_kinds);
    let (_, f) = s.get(&format!("{base}/facets"), ADMIN_TOKEN).await;
    assert_eq!(f["attachments"], kinds_before, "{f}");
    let (_, t) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=owner,tag"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(t, before_tree);
    let stored_after: Vec<(Vec<String>, bool)> =
        sqlx::query_as("select suite_tree, suite_titled from results order by id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        stored_after, stored_before,
        "the backfill stores what ingest stored"
    );
    let (_, t) = s
        .get(&format!("{base}/tree?includeTests=true"), ADMIN_TOKEN)
        .await;
    assert_eq!(t, suites_before);
    let again = state
        .store
        .backfill_explorer(3, Duration::ZERO)
        .await
        .unwrap();
    assert!(again.done && again.scanned == 0);
    let p = fastproof_server::scheduler::explorer_backfill(state, Duration::ZERO)
        .await
        .unwrap();
    assert!(p.done);
}

/// The environment switcher scopes the Failures tab (clusters) and the stored logs.
#[tokio::test]
async fn clusters_and_logs_follow_the_environment() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("envt").await;
    let launch = s.launch("envt", None).await;
    let lid = launch["id"].as_str().unwrap();
    let base = format!("/api/v1/projects/envt/launches/{lid}");
    let done = s
        .ingest(
            "envt",
            lid,
            read("playwright/results.json"),
            "?fileName=results.json",
        )
        .await;
    assert_eq!(done["status"], "done", "{done}");
    let (_, envs) = s.get(&format!("{base}/environments"), ADMIN_TOKEN).await;
    let keys: Vec<String> = envs["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["key"].as_str().map(str::to_string))
        .collect();
    assert!(keys.len() >= 2, "{envs}");
    let count = |v: &Value| -> i64 {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["count"].as_i64().unwrap())
            .sum()
    };
    let (st, all) = s.get(&format!("{base}/clusters"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{all}");
    assert!(count(&all) > 0, "the fixture has failures: {all}");
    let mut sum = 0;
    for k in &keys {
        let (st, c) = s
            .get(&format!("{base}/clusters?env={}", enc(k)), ADMIN_TOKEN)
            .await;
        assert_eq!(st, StatusCode::OK, "{c}");
        let failing = s
            .results(
                "envt",
                lid,
                &format!("&env={}&status=failed,broken", enc(k)),
            )
            .await
            .iter()
            .filter(|r| !r["fingerprint"].is_null())
            .count() as i64;
        assert_eq!(count(&c), failing, "{k}: {c}");
        sum += count(&c);
    }
    assert_eq!(sum, count(&all));
    let (_, none) = s
        .get(&format!("{base}/clusters?env=%2500"), ADMIN_TOKEN)
        .await;
    assert_eq!(count(&none), 0);
    let (st, _) = s
        .get(
            &format!("{base}/clusters?env={}", "x".repeat(300)),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let (st, b) = s
        .post(
            "/api/v1/projects/envt/launches",
            ADMIN_TOKEN,
            json!({"name": "api", "environment": {"key": "staging"}}),
        )
        .await;
    assert!(st.is_success(), "{b}");
    let api = b["id"].as_str().unwrap().to_string();
    post_results(
        &s,
        "envt",
        &api,
        &[result("t1", "a.T#one", "passed", json!([]))],
    )
    .await;
    let logs = format!("/api/v1/projects/envt/launches/{api}/logs");
    let (st, out) = s
        .post(
            &logs,
            ADMIN_TOKEN,
            json!([
                {"resultId": "t1", "attempt": 0, "seq": 1, "ts": "2026-11-21T10:00:00Z", "level": "info", "stream": "log", "message": "of t1"},
                {"resultId": "pending", "attempt": 0, "seq": 1, "ts": "2026-11-21T10:00:01Z", "level": "info", "stream": "log", "message": "not stored yet"}
            ]),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    let messages = |v: &Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["message"].as_str().unwrap().to_string())
            .collect()
    };
    let (_, every) = s.get(&logs, ADMIN_TOKEN).await;
    assert_eq!(messages(&every).len(), 2, "{every}");
    let (_, staging) = s.get(&format!("{logs}?env=staging"), ADMIN_TOKEN).await;
    assert_eq!(messages(&staging), vec!["of t1"]);
    let (_, other) = s.get(&format!("{logs}?env=production"), ADMIN_TOKEN).await;
    assert!(messages(&other).is_empty(), "{other}");
    let (_, none) = s.get(&format!("{logs}?env=%2500"), ADMIN_TOKEN).await;
    assert!(messages(&none).is_empty(), "{none}");
}
