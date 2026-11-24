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

async fn launch_gen(db: &TestDb, launch: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "select coalesce(max(gen), 0) from explorer_gen where launch_id = $1::uuid",
    )
    .bind(launch)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

/// Writes that do not come from this version (a replica of the previous one during a rolling
/// update, SQL by hand) still invalidate the cache, and changed labels or names recompute
/// `label_values` and clear `suite_tree`.
#[tokio::test]
async fn writes_from_other_code_invalidate_the_cache_and_stored_keys() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("old").await;
    let launch = s.launch("old", None).await;
    let lid = launch["id"].as_str().unwrap();
    let base = format!("/api/v1/projects/old/launches/{lid}");
    post_results(
        &s,
        "old",
        lid,
        &[
            result("a", "o.T#a", "failed", json!([l("owner", "ann")])),
            result("b", "o.T#b", "passed", json!([l("owner", "bob")])),
        ],
    )
    .await;
    finish(&s, "old", lid).await;
    let stored: i64 = sqlx::query_scalar(
        "select count(*) from results where launch_id = $1::uuid and suite_tree is not null and label_values is not null",
    )
    .bind(lid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(stored, 2, "ingest stores both keys");
    let (_, f) = s.get(&format!("{base}/facets"), ADMIN_TOKEN).await;
    assert_eq!(f["counts"]["muted"], 0);
    let (_, t) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=owner"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(t["children"].as_array().unwrap().len(), 2);
    let g0 = launch_gen(&db, lid).await;

    sqlx::query("update results set muted = true where launch_id = $1::uuid and name = 'a'")
        .bind(lid)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        launch_gen(&db, lid).await > g0,
        "the statement trigger counts the write"
    );
    let (_, f) = s.get(&format!("{base}/facets"), ADMIN_TOKEN).await;
    assert_eq!(f["counts"]["muted"], 1, "{f}");

    sqlx::query(
        "update results set labels = '[{\"name\": \"owner\", \"value\": \"cid\"}, {\"name\": \"parentSuite\", \"value\": \"Web\"}]' \
         where launch_id = $1::uuid and name = 'b'",
    )
    .bind(lid)
    .execute(&db.pool)
    .await
    .unwrap();
    let (lv, tree): (Value, Option<Vec<String>>) = sqlx::query_as(
        "select label_values, suite_tree from results where launch_id = $1::uuid and name = 'b'",
    )
    .bind(lid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(lv, json!({"owner": ["cid"], "parentSuite": ["Web"]}));
    assert_eq!(tree, None, "the stored suite path is cleared");
    let (_, t) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=owner"),
            ADMIN_TOKEN,
        )
        .await;
    let names: Vec<&str> = t["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["ann", "cid"]);
    let (_, scan) = s.get(&format!("{base}/tree"), ADMIN_TOKEN).await;
    let (_, counted) = s
        .get(&format!("{base}/tree?includeTests=true"), ADMIN_TOKEN)
        .await;
    assert_eq!(
        nodes(&scan, &|_| None).keys().collect::<Vec<_>>(),
        nodes(&counted, &|_| None).keys().collect::<Vec<_>>(),
        "the Suites tree follows the new labels: {counted}"
    );
}

/// A tree with more groups than the limit says `partial` and is not cached.
#[tokio::test]
async fn large_trees_are_bounded_and_partial() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("big").await;
    let launch = s.launch("big", None).await;
    let lid = launch["id"].as_str().unwrap();
    let rs: Vec<Value> = (0..12)
        .map(|i| {
            result(
                &format!("t{i}"),
                &format!("b.C{i}#t"),
                "passed",
                json!([l("owner", &format!("o{i}"))]),
            )
        })
        .collect();
    post_results(&s, "big", lid, &rs).await;
    finish(&s, "big", lid).await;
    let store = fastproof_store::Store::new(db.pool.clone());
    let project_id: uuid::Uuid =
        sqlx::query_scalar("select project_id from launches where id = $1::uuid")
            .bind(lid)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let l = store
        .launch(project_id, lid.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let opts = |by: fastproof_store::results::GroupBy| fastproof_store::results::TreeOptions {
        include_tests: true,
        ..fastproof_store::results::TreeOptions::groups(by)
    };
    let owner = || fastproof_store::results::GroupBy::Labels(vec!["owner".into()]);
    let few_keys = store.clone().with_tree_limits(5, 1000);
    let t = few_keys.tree_with(&l, &opts(owner())).await.unwrap();
    assert!(t.partial);
    assert_eq!(t.children.len(), 5);
    assert!(
        cache_rows(&db, lid).await.is_empty(),
        "partial trees are not cached"
    );
    let full = store.tree_with(&l, &opts(owner())).await.unwrap();
    assert!(!full.partial);
    assert_eq!(full.children.len(), 12);
    sqlx::query("delete from explorer_cache")
        .execute(&db.pool)
        .await
        .unwrap();
    let few_nodes = store.clone().with_tree_limits(1000, 4);
    let t = few_nodes
        .tree_with(&l, &opts(fastproof_store::results::GroupBy::Suite))
        .await
        .unwrap();
    assert!(t.partial, "{t:?}");
    let t = few_nodes
        .tree_with(&l, &fastproof_store::results::TreeOptions::groups(owner()))
        .await
        .unwrap();
    assert!(t.partial, "the result-id tree is bounded too");
}

/// A launch with more one-test suites than one `or` list holds reads them by a hashed list,
/// cold and from the cache, and its pages agree with the stored paths.
#[tokio::test]
async fn many_small_suites_and_their_pages() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("many").await;
    let launch = s.launch("many", None).await;
    let lid = launch["id"].as_str().unwrap();
    let base = format!("/api/v1/projects/many/launches/{lid}");
    let rs: Vec<Value> = (0..90)
        .map(|i| {
            result(
                &format!("t{i}"),
                &format!("m.C{i:02}#t"),
                if i % 9 == 0 { "failed" } else { "passed" },
                json!([]),
            )
        })
        .collect();
    post_results(&s, "many", lid, &rs).await;
    finish(&s, "many", lid).await;
    let (_, scan) = s.get(&format!("{base}/tree"), ADMIN_TOKEN).await;
    for round in 0..2 {
        let (_, t) = s
            .get(&format!("{base}/tree?includeTests=true"), ADMIN_TOKEN)
            .await;
        let a = nodes(&scan, &|n| ids(&n["resultIds"], ""));
        let b = nodes(&t, &|n| ids(&n["tests"], "id"));
        assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
        for (path, (counts, _, want)) in &a {
            assert_eq!(counts, &b[path].0, "round {round} {path:?}");
            assert_eq!(
                want.clone().unwrap_or_default(),
                b[path].2.clone().unwrap_or_default(),
                "round {round} {path:?}"
            );
        }
    }
    // A stored path that differs from the one computed from the row is used by the tree and its
    // pages alike.
    sqlx::query(
        "update results set suite_tree = '{elsewhere}' where launch_id = $1::uuid and name = 't1'",
    )
    .bind(lid)
    .execute(&db.pool)
    .await
    .unwrap();
    let (_, t) = s
        .get(&format!("{base}/tree?includeTests=true"), ADMIN_TOKEN)
        .await;
    assert_eq!(
        counted_node(&t, &["elsewhere".into()])["counts"]["total"],
        1,
        "{t}"
    );
    let (_, page) = s
        .get(
            &format!("{base}/tree/tests?path={}", enc("[\"elsewhere\"]")),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(page["items"][0]["name"], "t1", "{page}");
    let (_, ids_tree) = s.get(&format!("{base}/tree"), ADMIN_TOKEN).await;
    assert_eq!(
        counted_node(&ids_tree, &["elsewhere".into()])["counts"]["total"],
        1
    );
}

/// The cache is best effort, keys of different groupings and environments never meet, and a
/// deep link outside the inlined groups does not rewrite the entry.
#[tokio::test]
async fn cache_keys_writes_and_deep_links() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("ck").await;
    let launch = s.launch("ck", None).await;
    let lid = launch["id"].as_str().unwrap();
    let base = format!("/api/v1/projects/ck/launches/{lid}");
    // Groups y (4999), w (10) and z (1): the plain tree inlines z and w; a deep link into y
    // inlines y and z instead.
    let mut rs: Vec<Value> = (0..4999)
        .map(|i| {
            result(
                &format!("t{i}"),
                &format!("k.T#t{i:04}"),
                "passed",
                json!([l("a:k", "x"), l("a", "y")]),
            )
        })
        .collect();
    rs.extend((0..10).map(|i| {
        result(
            &format!("w{i}"),
            &format!("k.W#w{i}"),
            "passed",
            json!([l("a", "w")]),
        )
    }));
    rs.push(result("z0", "k.Z#z0", "passed", json!([l("a", "z")])));
    for chunk in rs.chunks(1000) {
        post_results(&s, "ck", lid, chunk).await;
    }
    finish(&s, "ck", lid).await;
    let (_, one) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=a:k"),
            ADMIN_TOKEN,
        )
        .await;
    let (_, two) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=a&env=all"),
            ADMIN_TOKEN,
        )
        .await;
    assert_ne!(one["children"], two["children"], "different entries");
    let (_, two_again) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=a&env=all"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(two, two_again);

    let deep = s.results("ck", lid, "&q=t0042").await;
    let deep_id = deep[0]["id"].as_str().unwrap().to_string();
    let (_, plain) = s
        .get(
            &format!("{base}/tree?includeTests=true&groupBy=a"),
            ADMIN_TOKEN,
        )
        .await;
    let before: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("select max(created_at) from explorer_cache where launch_id = $1::uuid")
            .bind(lid)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let _ = plain;
    for _ in 0..2 {
        let (st, t) = s
            .get(
                &format!("{base}/tree?includeTests=true&groupBy=a&reveal={deep_id}"),
                ADMIN_TOKEN,
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{t}");
        let found = nodes(&t, &|n| ids(&n["tests"], "id"))
            .values()
            .any(|(_, _, tests)| tests.as_ref().is_some_and(|x| x.contains(&deep_id)));
        assert!(found, "the deep-linked result is inline");
    }
    let after: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("select max(created_at) from explorer_cache where launch_id = $1::uuid")
            .bind(lid)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(before, after, "deep links do not rewrite the entry");

    sqlx::query("delete from explorer_cache")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "alter table explorer_cache add constraint explorer_cache_refuse check (false) not valid",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    for p in [
        format!("{base}/tree?includeTests=true"),
        format!("{base}/labels"),
        format!("{base}/facets"),
        format!("{base}/environments"),
    ] {
        let (st, b) = s.get(&p, ADMIN_TOKEN).await;
        assert_eq!(
            st,
            StatusCode::OK,
            "a failed cache write still answers: {p} {b}"
        );
    }
}

/// The backfill skips rows another transaction holds, never waits for them, and fills them
/// after the walk.
#[tokio::test]
async fn backfill_skips_locked_rows_and_comes_back() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["explorer.backfill"]).await;
    s.project("lk").await;
    let launch = s.launch("lk", None).await;
    let lid = launch["id"].as_str().unwrap();
    let rs: Vec<Value> = (0..6)
        .map(|i| {
            result(
                &format!("t{i}"),
                &format!("l.T#t{i}"),
                "passed",
                json!([l("owner", "ann")]),
            )
        })
        .collect();
    post_results(&s, "lk", lid, &rs).await;
    sqlx::query("update results set label_values = null")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("delete from analysis_state where name = 'explorer.results.backfill'")
        .execute(&db.pool)
        .await
        .unwrap();
    let mut holder = db.pool.begin().await.unwrap();
    let held: uuid::Uuid = sqlx::query_scalar(
        "select id from results where id = (select id from results where launch_id = $1::uuid \
         order by id limit 1 offset 2) for update",
    )
    .bind(lid)
    .fetch_one(&mut *holder)
    .await
    .unwrap();
    let state = &s.running.as_ref().unwrap().state;
    let started = std::time::Instant::now();
    let p = state
        .store
        .backfill_explorer(500, Duration::ZERO)
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "never waits for the held row"
    );
    assert_eq!(p.updated, 5, "{p:?}");
    let p = state
        .store
        .backfill_explorer(500, Duration::from_secs(5))
        .await
        .unwrap();
    assert!(!p.done, "the held row is still to do: {p:?}");
    holder.rollback().await.unwrap();
    let p = state
        .store
        .backfill_explorer(500, Duration::from_secs(5))
        .await
        .unwrap();
    assert!(p.done, "{p:?}");
    let left: Option<Value> = sqlx::query_scalar("select label_values from results where id = $1")
        .bind(held)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(left, Some(json!({"owner": ["ann"]})));
}

/// A launch kept by the rollup (a manual run) loses its results with the dropped partition;
/// its cached aggregates are invalidated with them.
#[tokio::test]
async fn rollup_invalidates_the_cache_of_kept_launches() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    let store = fastproof_store::Store::new(db.pool.clone());
    db.hold_jobs(&[
        "history.rollup",
        "retention.apply",
        "analysis.indexes",
        "partitions.ensure",
        "explorer.backfill",
    ])
    .await;
    store.ensure_partitions(6, 1).await.unwrap();
    s.project("ru").await;
    let (st, _) = s
        .call(
            Method::PUT,
            "/api/v1/projects/ru/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"retention": {"rawDays": 60, "keepImportant": true}, "rollup": {"afterDays": 30}})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let mut ids = Vec::new();
    for kept in [false, true] {
        let launch = s.launch("ru", None).await;
        let lid = launch["id"].as_str().unwrap().to_string();
        sqlx::query(
            "update launches set created_at = now() - interval '125 days', \
             started_at = now() - interval '125 days' where id = $1::uuid",
        )
        .bind(&lid)
        .execute(&db.pool)
        .await
        .unwrap();
        post_results(
            &s,
            "ru",
            &lid,
            &[result("a", "r.T#a", "failed", json!([l("owner", "ann")]))],
        )
        .await;
        finish(&s, "ru", &lid).await;
        if kept {
            sqlx::query("update launches set manual_state = 'closed' where id = $1::uuid")
                .bind(&lid)
                .execute(&db.pool)
                .await
                .unwrap();
        }
        ids.push(lid);
    }
    let kept = &ids[1];
    let base = format!("/api/v1/projects/ru/launches/{kept}");
    let (_, f) = s.get(&format!("{base}/facets"), ADMIN_TOKEN).await;
    assert_eq!(f["counts"]["total"], 1);
    assert!(!cache_rows(&db, kept).await.is_empty());
    let g0 = launch_gen(&db, kept).await;
    let report = store.rollup_due(Duration::from_secs(60)).await.unwrap();
    assert!(!report.partitions_dropped.is_empty(), "{report:?}");
    assert!(
        launch_gen(&db, kept).await > g0,
        "the kept launch's counter went up"
    );
    let (st, f) = s.get(&format!("{base}/facets"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        f["counts"]["total"], 0,
        "no cached counts of dropped results: {f}"
    );
}

/// The Suites tree reads the tests of its groups through the suite index of `results`, which
/// is built on every partition and holds a hash of the path, so a path longer than a B-tree
/// entry is stored and found too.
#[tokio::test]
async fn suite_groups_are_read_through_the_suite_index() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("si").await;
    let launch = s.launch("si", None).await;
    let lid = launch["id"].as_str().unwrap();
    let mut seed: u64 = 7;
    let long: String = (0..3000)
        .map(|_| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            char::from(b'a' + ((seed >> 33) % 26) as u8)
        })
        .collect();
    // Playwright titles, and Allure suite labels that make a path of 3000 bytes (a full name
    // that long cannot be stored: the name index of results refuses it).
    let mut rs = Vec::new();
    for (file, describe) in [("a.spec.ts", "A"), ("b.spec.ts", "Оплата")] {
        for i in 0..10 {
            rs.push(result(
                &format!("{file}{i}"),
                &format!("{file} › {describe} › t{i}"),
                if i == 3 { "failed" } else { "passed" },
                json!([]),
            ));
        }
    }
    let long_path: Vec<String> = long
        .as_bytes()
        .chunks(1000)
        .map(|c| String::from_utf8(c.to_vec()).unwrap())
        .collect();
    for i in 0..10 {
        rs.push(result(
            &format!("c{i}"),
            &format!("c.Long#t{i}"),
            "passed",
            json!([
                l("parentSuite", &long_path[0]),
                l("suite", &long_path[1]),
                l("subSuite", &long_path[2])
            ]),
        ));
    }
    post_results(&s, "si", lid, &rs).await;
    finish(&s, "si", lid).await;

    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        fastproof_store::analysis::indexes::ensure_analysis_indexes(&db.pool)
            .await
            .unwrap();
        let (valid, missing): (bool, i64) = sqlx::query_as(
            "select coalesce((select ix.indisvalid from pg_index ix join pg_class c on c.oid = ix.indexrelid \
               where c.relname = 'results_launch_suite'), false), \
             (select count(*) from pg_inherits i join pg_class p on p.oid = i.inhparent \
               where p.relname = 'results' and not exists (select 1 from pg_inherits ii \
                 join pg_index ix on ix.indexrelid = ii.inhrelid \
                 where ii.inhparent = 'results_launch_suite'::regclass and ix.indrelid = i.inhrelid and ix.indisvalid))",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        if valid && missing == 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "suite index valid {valid}, partitions without it {missing}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let base = format!("/api/v1/projects/si/launches/{lid}");
    let (_, t) = s
        .get(&format!("{base}/tree?includeTests=true"), ADMIN_TOKEN)
        .await;
    for path in [
        vec!["a.spec.ts".to_string(), "A".to_string()],
        vec!["b.spec.ts".to_string(), "Оплата".to_string()],
        long_path.clone(),
    ] {
        let n = counted_node(&t, &path);
        assert_eq!(n["counts"]["total"], 10, "{path:?}");
        assert_eq!(n["tests"].as_array().unwrap().len(), 10, "{path:?}");
    }

    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query("set local enable_seqscan = off")
        .execute(&mut *tx)
        .await
        .unwrap();
    let plan: Vec<String> = sqlx::query_scalar(
        "explain select r.id from results r where r.launch_id = $1::uuid and r.launch_created_at = $2 \
         and hash_array_extended(r.suite_tree, 0::bigint) = any($3::bigint[])",
    )
    .bind(lid)
    .bind(
        sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>(
            "select created_at from launches where id = $1::uuid",
        )
        .bind(lid)
        .fetch_one(&db.pool)
        .await
        .unwrap(),
    )
    .bind(vec![1i64, 2])
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    let plan = plan.join("\n");
    assert!(plan.contains("_suite"), "{plan}");
}

/// A launch that finished gets its unfiltered Suites tree built in the background, so its first
/// view is read from the cache; each finished launch is built once, and the cached tree equals
/// one built on the request.
#[tokio::test]
async fn finished_launches_get_their_first_tree_built() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["explorer.warm"]).await;
    let store = fastproof_store::Store::new(db.pool.clone());
    s.project("warm").await;
    let launch = s.launch("warm", None).await;
    let lid = launch["id"].as_str().unwrap();
    let rs: Vec<Value> = (0..30)
        .map(|i| {
            result(
                &format!("t{i}"),
                &format!("w.C{}#t{i}", i % 3),
                if i % 7 == 0 { "failed" } else { "passed" },
                json!([]),
            )
        })
        .collect();
    post_results(&s, "warm", lid, &rs).await;
    let small = s.launch("warm", None).await;
    post_results(&s, "warm", small["id"].as_str().unwrap(), &rs[..2]).await;
    finish(&s, "warm", lid).await;
    finish(&s, "warm", small["id"].as_str().unwrap()).await;
    let warm = || store.warm_explorer_trees(Duration::ZERO, 10, 20, Duration::from_secs(30));
    assert_eq!(
        warm().await.unwrap(),
        1,
        "only the launch with enough results"
    );
    assert_eq!(cache_rows(&db, lid).await.len(), 1);
    assert!(
        cache_rows(&db, small["id"].as_str().unwrap())
            .await
            .is_empty()
    );
    assert_eq!(warm().await.unwrap(), 0, "each launch once");
    let base = format!("/api/v1/projects/warm/launches/{lid}");
    let (_, cached) = s
        .get(&format!("{base}/tree?includeTests=true"), ADMIN_TOKEN)
        .await;
    sqlx::query("delete from explorer_cache where launch_id = $1::uuid")
        .bind(lid)
        .execute(&db.pool)
        .await
        .unwrap();
    let (_, built) = s
        .get(&format!("{base}/tree?includeTests=true"), ADMIN_TOKEN)
        .await;
    assert_eq!(cached, built);
    assert_eq!(cached["counts"]["total"], 30);
}
