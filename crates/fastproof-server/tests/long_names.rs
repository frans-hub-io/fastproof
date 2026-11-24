//! Names up to `max_name_bytes` (4 KiB) of text that does not compress, failure messages of
//! 1 MiB of words and long writer-side ids are stored through every ingest path: the results
//! API, uploads and manual runs (the Kafka writer converts results with the API's code). The
//! name and message indexes of `results` used to refuse such rows (a B-tree entry holds 2704
//! bytes, a `tsvector` 1 MiB), which failed the whole request. Pages of results by name stay
//! in order, and an installation upgraded with the old indexes gets them replaced by the
//! `analysis.indexes` job. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb, tar_gz};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

/// Text of at most `bytes` bytes that does not compress: letters, Cyrillic, CJK and emoji
/// drawn from a seeded generator.
fn random_text(seed: u64, bytes: usize) -> String {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = String::new();
    loop {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let c = match x % 4 {
            0 => char::from(b'a' + (x >> 8) as u8 % 26),
            1 => char::from_u32(0x0430 + ((x >> 8) % 32) as u32).unwrap(),
            2 => char::from_u32(0x4E00 + ((x >> 8) % 20000) as u32).unwrap(),
            _ => char::from_u32(0x1F300 + ((x >> 8) % 500) as u32).unwrap(),
        };
        if out.len() + c.len_utf8() > bytes {
            return out;
        }
        out.push(c);
    }
}

/// About `bytes` of distinct short words (their `tsvector` is larger than the text).
fn words(seed: u64, bytes: usize) -> String {
    let mut x = seed | 1;
    let mut out = String::with_capacity(bytes);
    while out.len() + 10 < bytes {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        for i in 0..(3 + x % 6) {
            out.push(char::from(b'a' + ((x >> (i * 5)) % 26) as u8));
        }
        out.push(' ');
    }
    out
}

fn result(id: &str, name: &str, full_name: &str, status: &str) -> Value {
    json!({"id": id, "name": name, "fullName": full_name, "status": status,
        "start": 1_795_000_000_000i64, "stop": 1_795_000_000_100i64})
}

async fn post(s: &Server, project: &str, launch: &str, items: &[Value]) -> Value {
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/{project}/launches/{launch}/results"),
            Some(ADMIN_TOKEN),
        )
        .json(&Value::Array(items.to_vec()))
        .send()
        .await
        .unwrap();
    let st = r.status();
    let b: Value = r.json().await.unwrap_or(Value::Null);
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    b
}

async fn names_in(db: &TestDb, launch: &str) -> Vec<(String, String)> {
    sqlx::query_as("select name, full_name from results where launch_id = $1::uuid order by id")
        .bind(launch)
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

async fn index_state(db: &TestDb) -> Vec<(String, bool)> {
    sqlx::query_as(
        "select c.relname::text, ix.indisvalid from pg_index ix join pg_class c on c.oid = ix.indexrelid \
         where c.relname in ('results_launch_name', 'results_message_fts', 'results_launch_name_key', \
           'results_message_tsv', 'test_cases_project_name') order by 1",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn api_results_with_long_names_and_messages() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    assert_eq!(
        index_state(&db).await,
        vec![
            ("results_launch_name_key".to_string(), true),
            ("results_message_tsv".to_string(), true)
        ],
        "a new installation starts with the new indexes only"
    );
    s.project("ln").await;
    let launch = s.launch("ln", None).await;
    let lid = launch["id"].as_str().unwrap();

    let long = random_text(1, 4096);
    let shared = random_text(2, 3000);
    let message = words(3, 1024 * 1024 - 16);
    let mut failing = result("r1", &random_text(4, 4096), &long, "failed");
    failing["error"] = json!({"message": message});
    let items = vec![
        failing,
        result("r2", "too long", &random_text(5, 6000), "passed"),
        result("r3", "a", &format!("{shared}a"), "passed"),
        result("r4", "b", &format!("{shared}b"), "passed"),
        result("r5", "short", "shop.Cart#short", "passed"),
    ];
    let out = post(&s, "ln", lid, &items).await;
    assert_eq!(out["accepted"], 5, "{out}");
    let warnings = out["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{out}");
    assert_eq!(warnings[0]["index"], 1);
    assert!(
        warnings[0]["message"]
            .as_str()
            .unwrap()
            .contains("max_name_bytes"),
        "{out}"
    );

    let stored = names_in(&db, lid).await;
    assert!(stored.iter().any(|(_, f)| *f == long), "kept whole");
    let cut = stored
        .iter()
        .find(|(n, _)| n == "too long")
        .map(|(_, f)| f.clone())
        .unwrap();
    assert!(
        cut.len() <= 4096 && cut.ends_with('\u{2026}'),
        "{}",
        cut.len()
    );

    // Pages of one result follow the name order, shared prefixes included.
    let base = format!("/api/v1/projects/ln/launches/{lid}/results");
    let mut seen = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let mut url = format!("{base}?limit=1");
        if let Some(c) = &next {
            url.push_str(&format!("&before={}", urlencode(c)));
        }
        let (st, page) = s.get(&url, ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::OK, "{page}");
        for r in page["items"].as_array().unwrap() {
            seen.push(r["fullName"].as_str().unwrap().to_string());
        }
        match page["next"].as_str() {
            Some(c) => next = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(seen.len(), 5);
    assert_eq!(seen.iter().collect::<BTreeSet<_>>().len(), 5);
    let a = seen
        .iter()
        .position(|n| *n == format!("{shared}a"))
        .unwrap();
    let b = seen
        .iter()
        .position(|n| *n == format!("{shared}b"))
        .unwrap();
    assert!(a < b, "names that share 3000 bytes keep their order");

    let (st, tree) = s
        .get(
            &format!("/api/v1/projects/ln/launches/{lid}/tree?includeTests=true"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{tree}");
    assert_eq!(tree["counts"]["total"], 5);

    let word = message
        .split(' ')
        .skip(1000)
        .find(|w| w.len() == 8)
        .unwrap();
    let (st, hits) = s
        .get(&format!("{base}?q={}", urlencode(word)), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{hits}");
    let hits = hits["items"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "a word of the message finds the result");
    assert_eq!(hits[0]["fullName"], long);
}

fn urlencode(s: &str) -> String {
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

#[tokio::test]
async fn uploads_with_long_names_ids_and_messages() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("lu").await;
    let launch = s.launch("lu", None).await;
    let lid = launch["id"].as_str().unwrap();
    let uuid = random_text(10, 4096);
    let full = random_text(11, 4096);
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("a-result.json"),
        json!({"uuid": uuid, "name": random_text(12, 6000), "fullName": full,
            "status": "failed", "stage": "finished", "start": 1000, "stop": 1100,
            "statusDetails": {"message": words(13, 1024 * 1024 - 16)},
            "labels": [{"name": "suite", "value": random_text(14, 4096)}]})
        .to_string(),
    )
    .unwrap();
    let bytes = tar_gz(d.path(), &|_| true);
    for round in 0..2 {
        let u = s.ingest("lu", lid, bytes.clone(), "").await;
        assert_eq!(u["status"], "done", "round {round}: {u}");
    }
    let stored = names_in(&db, lid).await;
    assert_eq!(stored.len(), 1, "{:?}", stored.len());
    assert_eq!(stored[0].1, full);
    assert!(stored[0].0.len() <= 4096 && stored[0].0.ends_with('\u{2026}'));
    let (keys, attempts): (Vec<String>, i64) = sqlx::query_as(
        "select array_agg(distinct client_key), count(*) from attempts where launch_id = $1::uuid",
    )
    .bind(lid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(attempts, 1, "the same upload twice is the same attempt");
    assert!(
        keys[0].starts_with("u:allure") && keys[0].contains(":#") && keys[0].len() < 100,
        "{keys:?}"
    );
}

#[tokio::test]
async fn manual_results_and_folders_with_long_names() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_TM_MAX_NAME_BYTES", "4096")]).await;
    s.project("lm").await;
    let folder_name = random_text(20, 4096);
    let (st, folder) = s
        .post(
            "/api/v1/projects/lm/case-folders",
            ADMIN_TOKEN,
            json!({"name": folder_name}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{folder}");
    let (st, again) = s
        .post(
            "/api/v1/projects/lm/case-folders",
            ADMIN_TOKEN,
            json!({"name": folder_name}),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CONFLICT,
        "sibling names stay unique: {again}"
    );
    let case_name = random_text(21, 4096);
    let (st, case) = s
        .post(
            "/api/v1/projects/lm/cases",
            ADMIN_TOKEN,
            json!({"name": case_name, "folderId": folder["id"],
                "body": {"steps": [{"action": "Do it"}]}}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{case}");
    let number = case["number"].as_i64().unwrap();
    let l = s.launch("lm", None).await;
    let lid = l["id"].as_str().unwrap();
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/lm/launches/{lid}/run-items"),
            ADMIN_TOKEN,
            json!({"caseNumbers": [number]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (_, list) = s
        .get(
            &format!("/api/v1/projects/lm/launches/{lid}/run-items?limit=10"),
            ADMIN_TOKEN,
        )
        .await;
    let item = list["items"][0]["id"].as_str().unwrap().to_string();
    let r = s
        .req(
            Method::GET,
            &format!("/api/v1/projects/lm/run-items/{item}"),
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    let etag = r.headers()["etag"].to_str().unwrap().to_string();
    let r = s
        .req(
            Method::PATCH,
            &format!("/api/v1/projects/lm/run-items/{item}"),
            Some(ADMIN_TOKEN),
        )
        .header("if-match", etag)
        .json(&json!({"status": "failed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let stored = names_in(&db, lid).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].0, case_name,
        "a manual result is named after its case"
    );
    let suite: Vec<String> =
        sqlx::query_scalar("select suite_tree from results where launch_id = $1::uuid")
            .bind(lid)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(suite, vec![folder_name.clone()], "its suite is the folder");
    let tests: i64 = sqlx::query_scalar(
        "select count(*) from test_cases t join projects p on p.id = t.project_id \
         where p.key = 'lm' and t.kind = 'manual'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(tests, 1);
}

#[tokio::test]
async fn old_indexes_are_replaced_by_the_job() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["analysis.indexes"]).await;
    s.project("lo").await;
    let launch = s.launch("lo", None).await;
    let lid = launch["id"].as_str().unwrap();
    let long = vec![result("r1", "long", &random_text(30, 4096), "passed")];
    // As after an upgrade: the old indexes, and the new ones only on the parent.
    for q in [
        "drop index results_launch_name_key",
        "drop index results_message_tsv",
        "create index results_launch_name on results (launch_id, full_name, id)",
        "create index results_message_fts on results using gin (to_tsvector('simple', coalesce(message, '')))",
        "create index results_launch_name_key on only results (launch_id, left(full_name, 600))",
        "create index results_message_tsv on only results \
         using gin (to_tsvector('simple', left(coalesce(message, ''), 65536)))",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(q))
            .execute(&db.pool)
            .await
            .unwrap();
    }
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/lo/launches/{lid}/results"),
            Some(ADMIN_TOKEN),
        )
        .json(&Value::Array(long.clone()))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "the old name index refuses a 4 KiB name"
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        fastproof_store::analysis::indexes::ensure_analysis_indexes(&db.pool)
            .await
            .unwrap();
        let state = index_state(&db).await;
        if state
            == vec![
                ("results_launch_name_key".to_string(), true),
                ("results_message_tsv".to_string(), true),
            ]
        {
            break;
        }
        assert!(Instant::now() < deadline, "{state:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let out = post(&s, "lo", lid, &long).await;
    assert_eq!(out["accepted"], 1, "{out}");
}
