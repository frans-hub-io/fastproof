//! Direct uploads of attachments through pre-signed S3 URLs: issue, PUT, verification,
//! rejection, limits, expiry and sweeping, the CLI path with references in the archive, and the
//! fallback of a filesystem server. S3 tests need `FASTPROOF_TEST_S3_*` (they share the bucket's
//! `direct/` prefix, so they run one at a time).

mod common;

use std::path::Path;
use std::time::Duration;

use clap::Parser;
use common::*;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

static S3_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn body(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| ((i as u64 * 2_654_435_761 + seed as u64) % 251) as u8)
        .collect()
}

async fn s3_server(db: &TestDb, extra: &[(&str, &str)]) -> Option<Server> {
    let Some(env) = s3_env() else {
        eprintln!("FASTPROOF_TEST_S3_ENDPOINT is not set: skipped");
        return None;
    };
    let mut vars: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    vars.push(("FASTPROOF_BLOB_BACKEND_MIGRATED", "true"));
    vars.push(("FASTPROOF_DIRECT_UPLOAD_ALLOW_HTTP", "true"));
    vars.extend_from_slice(extra);
    // The tests sweep themselves (or through the scheduler when they choose) and check the
    // state of a video before its seekable copy is made.
    Some(
        Server::start_with_paused(
            db,
            &vars,
            &[
                "direct_uploads.sweep",
                "direct_uploads.orphans",
                "media.prepare",
            ],
        )
        .await,
    )
}

fn state(s: &Server) -> &fastproof_server::state::AppState {
    &s.running.as_ref().unwrap().state
}

/// Objects under this installation's `direct/` prefix that belong to an upload.
async fn direct_objects(s: &Server, upload: &str) -> Vec<String> {
    let st = state(s);
    let inst = fastproof_server::direct_uploads::installation(st)
        .await
        .unwrap();
    st.blobs
        .list(&fastproof_blob::BlobKey::direct_prefix(&inst).unwrap())
        .await
        .unwrap()
        .into_iter()
        .map(|(k, _)| k)
        .filter(|k| k.contains(upload))
        .collect()
}

async fn ask(s: &Server, project: &str, lid: &str, items: Value) -> (StatusCode, Value) {
    s.post(
        &format!("/api/v1/projects/{project}/launches/{lid}/attachments/direct"),
        ADMIN_TOKEN,
        json!({ "items": items }),
    )
    .await
}

async fn put(target: &Value, bytes: Vec<u8>) -> StatusCode {
    let mut rb = reqwest::Client::new()
        .put(target["url"].as_str().unwrap())
        .body(bytes);
    for (k, v) in target["headers"].as_object().unwrap() {
        rb = rb.header(k, v.as_str().unwrap());
    }
    rb.send().await.unwrap().status()
}

async fn complete(s: &Server, project: &str, lid: &str, upload: &Value) -> (StatusCode, Value) {
    s.call(
        Method::POST,
        &format!(
            "/api/v1/projects/{project}/launches/{lid}/attachments/direct/{}/complete",
            upload.as_str().unwrap()
        ),
        Some(ADMIN_TOKEN),
        None,
    )
    .await
}

async fn content(s: &Server, project: &str, id: &str) -> Vec<u8> {
    let r = s
        .req(
            Method::GET,
            &format!("/api/v1/projects/{project}/attachments/{id}/content?original=1"),
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    r.bytes().await.unwrap().to_vec()
}

async fn audit_actions(s: &Server) -> Vec<String> {
    let (_, a) = s.get("/api/v1/audit?limit=500", ADMIN_TOKEN).await;
    a["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["action"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn filesystem_server_answers_501_and_checks_permissions_first() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("fsdirect").await;
    let launch = s.launch("fsdirect", None).await;
    let lid = launch["id"].as_str().unwrap();
    let item = json!([{"sha256": sha(b"x"), "size": 1}]);
    let (st, b) = ask(&s, "fsdirect", lid, item.clone()).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED, "{b}");
    assert_eq!(b["code"], "direct_uploads_unavailable");
    let (st, _) = complete(
        &s,
        "fsdirect",
        lid,
        &json!(uuid::Uuid::now_v7().to_string()),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);

    s.project("elsewhere").await;
    let viewer = s.token("v", "viewer", &["fsdirect"]).await;
    let other = s.token("o", "uploader", &["elsewhere"]).await;
    for (t, want) in [
        (&viewer, StatusCode::FORBIDDEN),
        (&other, StatusCode::FORBIDDEN),
    ] {
        let (st, _) = s
            .post(
                &format!("/api/v1/projects/fsdirect/launches/{lid}/attachments/direct"),
                t,
                json!({"items": item}),
            )
            .await;
        assert_eq!(st, want);
    }
    let (st, _) = s
        .call(
            Method::POST,
            &format!("/api/v1/projects/fsdirect/launches/{lid}/attachments/direct"),
            None,
            Some(json!({"items": item})),
        )
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = ask(&s, "fsdirect", &uuid::Uuid::now_v7().to_string(), item).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "unknown launch");
}

#[tokio::test]
async fn direct_upload_is_verified_and_behaves_like_any_attachment() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let _one = S3_TESTS.lock().await;
    let Some(s) = s3_server(&db, &[]).await else {
        return;
    };
    let p = format!("direct-{:08x}", rand::random::<u32>());
    s.project(&p).await;
    let launch = s.launch(&p, None).await;
    let lid = launch["id"].as_str().unwrap();
    let video = body(300_000, 7);
    let vsha = sha(&video);

    let (st, b) = ask(
        &s,
        &p,
        lid,
        json!([{"sha256": vsha, "size": video.len(), "contentType": "video/webm", "name": "run.webm"}]),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let t = &b["items"][0];
    assert_eq!(t["status"], "upload", "{b}");
    assert_eq!(t["method"], "PUT");
    assert_eq!(t["headers"]["content-type"], "application/octet-stream");
    assert!(t["headers"]["x-amz-checksum-sha256"].is_string(), "{t}");
    assert!(t["expiresAt"].is_string());

    let (st, e) = complete(&s, &p, lid, &t["uploadId"]).await;
    assert_eq!(st, StatusCode::CONFLICT, "{e}");
    assert_eq!(e["code"], "not_uploaded");

    assert!(put(t, video.clone()).await.is_success());
    let (st, done) = complete(&s, &p, lid, &t["uploadId"]).await;
    assert_eq!(st, StatusCode::CREATED, "{done}");
    assert_eq!(done["sha256"], vsha);
    assert_eq!(done["size"], video.len());
    let (st, again) = complete(&s, &p, lid, &t["uploadId"]).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "completing twice answers the same attachment"
    );
    assert_eq!(again["id"], done["id"]);
    let aid = done["id"].as_str().unwrap();
    assert_eq!(content(&s, &p, aid).await, video);

    let st8 = state(&s);
    let id = t["uploadId"].as_str().unwrap();
    assert!(
        direct_objects(&s, id).await.is_empty(),
        "the upload's objects are gone"
    );
    let project = st8.store.project(&p).await.unwrap().unwrap();
    let (content_type, media_state, claimed): (String, i16, bool) = sqlx::query_as(
        "select a.content_type, a.media_state, \
           exists (select 1 from attachment_media m where m.attachment_id = a.id) \
         from attachments a where a.id = $1",
    )
    .bind(aid.parse::<uuid::Uuid>().unwrap())
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        content_type.starts_with("video/") && media_state == 0 && !claimed,
        "the video waits for its seekable copy like any uploaded video: \
         {content_type} {media_state} {claimed}"
    );

    let (st, out) = s
        .post(
            &format!("/api/v1/projects/{p}/launches/{lid}/results"),
            ADMIN_TOKEN,
            json!([{"id": "r1", "name": "t", "status": "failed", "start": 1, "stop": 2,
                "attachments": [{"id": aid, "name": "run.webm", "contentType": "video/webm"}]}]),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    assert_eq!(
        out["accepted"], 1,
        "the result may reference the attachment: {out}"
    );

    let (_, b2) = ask(
        &s,
        &p,
        lid,
        json!([{"sha256": vsha, "size": video.len(), "contentType": "video/webm", "name": "run.webm"},
               {"sha256": vsha, "size": video.len(), "contentType": "video/webm", "name": "other.webm"}]),
    )
    .await;
    assert_eq!(b2["items"][0]["status"], "exists", "{b2}");
    assert_eq!(b2["items"][0]["id"], done["id"]);
    assert_eq!(b2["items"][1]["status"], "exists");
    assert_eq!(st8.store.attachment_count(project.id).await.unwrap(), 1);
    sqlx::query("delete from attachment_links")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("delete from attachment_refs")
        .execute(&db.pool)
        .await
        .unwrap();
    let (_, b3) = ask(
        &s,
        &p,
        lid,
        json!([{"sha256": vsha, "size": video.len(), "contentType": "video/webm"}]),
    )
    .await;
    assert_eq!(
        b3["items"][0]["status"], "upload",
        "content no launch uses any more needs its bytes again: {b3}"
    );

    let viewer = s.token("v", "viewer", &[&p]).await;
    let (st, _) = s
        .call(
            Method::POST,
            &format!("/api/v1/projects/{p}/launches/{lid}/attachments/direct/{id}/complete"),
            Some(&viewer),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let other = s.launch(&p, Some("other")).await;
    let (st, _) = complete(&s, &p, other["id"].as_str().unwrap(), &t["uploadId"]).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "an upload belongs to its launch");
    assert!(
        audit_actions(&s)
            .await
            .iter()
            .any(|a| a == "attachment.direct.create")
    );
    st8.blobs
        .delete_prefix(&fastproof_blob::BlobKey::project_prefix(&p).unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn objects_that_differ_from_the_declaration_are_rejected_and_deleted() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let _one = S3_TESTS.lock().await;
    let Some(s) = s3_server(&db, &[("FASTPROOF_DIRECT_UPLOAD_CHECKSUM", "false")]).await else {
        return;
    };
    let p = format!("direct-{:08x}", rand::random::<u32>());
    s.project(&p).await;
    let launch = s.launch(&p, None).await;
    let lid = launch["id"].as_str().unwrap();
    let good = body(10_000, 1);
    let evil = body(10_000, 2);
    let (_, b) = ask(
        &s,
        &p,
        lid,
        json!([{"sha256": sha(&good), "size": good.len(), "name": "a.png", "contentType": "image/png"},
               {"sha256": sha(&good), "size": good.len(), "name": "b.png"},
               {"sha256": sha(&good), "size": good.len(), "name": "c.png"}]),
    )
    .await;
    let items = b["items"].as_array().unwrap();
    assert!(items.iter().all(|i| i["status"] == "upload"), "{b}");
    assert!(items[0]["headers"].get("x-amz-checksum-sha256").is_none());

    assert!(put(&items[0], evil.clone()).await.is_success());
    let (st, e) = complete(&s, &p, lid, &items[0]["uploadId"]).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{e}");
    assert_eq!(e["code"], "verification_failed");
    assert!(e["message"].as_str().unwrap().contains("sha256"), "{e}");
    let (st, _) = complete(&s, &p, lid, &items[0]["uploadId"]).await;
    assert_eq!(
        st,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a rejected upload stays rejected"
    );

    assert!(put(&items[1], good[..100].to_vec()).await.is_success());
    let (st, e) = complete(&s, &p, lid, &items[1]["uploadId"]).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{e}");
    assert!(e["message"].as_str().unwrap().contains("bytes"), "{e}");

    let st8 = state(&s);
    let project = st8.store.project(&p).await.unwrap().unwrap();
    assert_eq!(st8.store.attachment_count(project.id).await.unwrap(), 0);
    for i in &items[..2] {
        let id = i["uploadId"].as_str().unwrap();
        assert!(direct_objects(&s, id).await.is_empty(), "{id} was deleted");
    }
    assert!(
        st8.blobs
            .head(&fastproof_blob::BlobKey::attachment(&p, &sha(&evil)).unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        audit_actions(&s)
            .await
            .iter()
            .filter(|a| *a == "attachment.direct.rejected")
            .count()
            == 2
    );

    assert!(put(&items[2], good.clone()).await.is_success());
    let (st, done) = complete(&s, &p, lid, &items[2]["uploadId"]).await;
    assert_eq!(st, StatusCode::CREATED, "{done}");
    assert!(
        put(&items[2], evil.clone()).await.is_success(),
        "the URL still works until it expires"
    );
    assert_eq!(
        content(&s, &p, done["id"].as_str().unwrap()).await,
        good,
        "writing the URL again after completion changes nothing that is stored"
    );
    let id = items[2]["uploadId"].as_str().unwrap();
    assert_eq!(direct_objects(&s, id).await.len(), 1);

    let (_, big) = ask(&s, &p, lid, json!([{"sha256": sha(b"x"), "size": 10}])).await;
    let big = &big["items"][0];
    assert!(put(big, body(20_000, 3)).await.is_success());
    db.run_scheduled("direct_uploads.sweep").await;
    let big_id = big["uploadId"].as_str().unwrap();
    assert!(direct_objects(&s, big_id).await.is_empty());
    let (st, e) = complete(&s, &p, lid, &big["uploadId"]).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{e}");

    sqlx::query(
        "update direct_uploads set expires_at = now() - interval '2 hours' where status = 'done'",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let swept = fastproof_server::direct_uploads::sweep(st8).await.unwrap();
    assert_eq!(swept.finished, 1, "{swept:?}");
    assert!(
        direct_objects(&s, id).await.is_empty(),
        "what a PUT left after completion is deleted once the URL is dead"
    );

    let inst = fastproof_server::direct_uploads::installation(st8)
        .await
        .unwrap();
    let stray =
        fastproof_blob::BlobKey::direct_incoming(&inst, &uuid::Uuid::now_v7().to_string()).unwrap();
    st8.blobs
        .put_bytes(&stray, bytes::Bytes::from_static(b"left"))
        .await
        .unwrap();
    let n = fastproof_server::direct_uploads::sweep_orphans(st8, Duration::ZERO)
        .await
        .unwrap();
    assert!(n >= 1);
    assert_eq!(st8.blobs.head(&stray).await.unwrap(), None);
    st8.blobs
        .delete_prefix(&fastproof_blob::BlobKey::project_prefix(&p).unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn limits_validation_and_expiry() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let _one = S3_TESTS.lock().await;
    let Some(s) = s3_server(
        &db,
        &[
            ("FASTPROOF_DIRECT_UPLOAD_MAX_OPEN_PER_LAUNCH", "2"),
            ("FASTPROOF_DIRECT_UPLOAD_MAX_BYTES", "1000"),
            ("FASTPROOF_DIRECT_UPLOAD_MAX_BYTES_PER_LAUNCH", "1500"),
            ("FASTPROOF_DIRECT_UPLOAD_MAX_OPEN_BYTES_PER_PROJECT", "2500"),
        ],
    )
    .await
    else {
        return;
    };
    let p = format!("direct-{:08x}", rand::random::<u32>());
    s.project(&p).await;
    let launch = s.launch(&p, None).await;
    let lid = launch["id"].as_str().unwrap();
    let it = |n: u8, size: u64| json!({"sha256": sha(&[n]), "size": size});
    let (st, b) = ask(
        &s,
        &p,
        lid,
        json!([it(1, 10), it(2, 2000), it(3, 10), it(4, 10)]),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let st_codes: Vec<(String, String)> = b["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["status"].as_str().unwrap().to_string(),
                i["code"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    assert_eq!(
        st_codes,
        vec![
            ("upload".into(), "".into()),
            ("refused".into(), "too_large".into()),
            ("upload".into(), "".into()),
            ("refused".into(), "launch_open".into()),
        ]
    );
    let other = s.launch(&p, Some("bytes")).await;
    let olid = other["id"].as_str().unwrap();
    let (_, b) = ask(&s, &p, olid, json!([it(5, 1000), it(6, 600)])).await;
    assert_eq!(b["items"][1]["code"], "launch_bytes", "{b}");
    assert_eq!(b["items"][1]["limit"], 1500);
    let third = s.launch(&p, Some("project-bytes")).await;
    let (_, b3) = ask(
        &s,
        &p,
        third["id"].as_str().unwrap(),
        json!([it(9, 1000), it(10, 500)]),
    )
    .await;
    assert_eq!(b3["items"][0]["status"], "upload", "{b3}");
    assert_eq!(b3["items"][1]["code"], "project_bytes", "{b3}");
    assert_eq!(b3["items"][1]["limit"], 2500);

    for bad in [
        json!({"items": []}),
        json!({"items": (0..101).map(|i| it(i as u8, 1)).collect::<Vec<_>>()}),
        json!({"items": [{"sha256": "ABC", "size": 1}]}),
        json!({"items": [{"sha256": sha(b"a").to_uppercase(), "size": 1}]}),
        json!({"items": [{"sha256": sha(b"a"), "size": 1, "extra": 1}]}),
        json!({"items": [{"sha256": sha(b"a"), "size": -1}]}),
    ] {
        let (st, e) = s
            .post(
                &format!("/api/v1/projects/{p}/launches/{lid}/attachments/direct"),
                ADMIN_TOKEN,
                bad.clone(),
            )
            .await;
        assert!(
            st == StatusCode::BAD_REQUEST || st == StatusCode::UNPROCESSABLE_ENTITY,
            "{bad} -> {st} {e}"
        );
        assert_eq!(e["code"], "bad_request");
    }

    let first = b["items"][0]["uploadId"].clone();
    sqlx::query("update direct_uploads set expires_at = now() - interval '2 hours'")
        .execute(&db.pool)
        .await
        .unwrap();
    let (st, e) = complete(&s, &p, olid, &first).await;
    assert_eq!(st, StatusCode::GONE, "{e}");
    assert_eq!(e["code"], "upload_expired");
    let (_, b) = ask(&s, &p, lid, json!([it(11, 10)])).await;
    assert_eq!(
        b["items"][0]["code"], "launch_open",
        "expired uploads count until the sweep removed their objects: {b}"
    );
    let swept = fastproof_server::direct_uploads::sweep(state(&s))
        .await
        .unwrap();
    assert_eq!(swept.expired, 4, "{swept:?}");
    let (st, b) = ask(&s, &p, lid, json!([it(7, 10), it(8, 10)])).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        b["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["status"] == "upload"),
        "expired uploads no longer count: {b}"
    );
}

fn token_file(dir: &Path) -> std::path::PathBuf {
    let p = dir.join("token");
    std::fs::write(&p, ADMIN_TOKEN).unwrap();
    p
}

fn allure_dir(dir: &Path, video: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("v1-attachment.webm"), video).unwrap();
    std::fs::write(dir.join("s1-attachment.txt"), b"small log").unwrap();
    std::fs::write(
        dir.join("r1-result.json"),
        json!({"uuid": "r1", "historyId": "h1", "name": "plays", "fullName": "pkg.Video#plays",
            "status": "failed", "start": 1_763_712_000_000u64, "stop": 1_763_712_001_000u64,
            "attachments": [
                {"name": "recording", "source": "v1-attachment.webm", "type": "video/webm"},
                {"name": "log", "source": "s1-attachment.txt", "type": "text/plain"}]})
        .to_string(),
    )
    .unwrap();
}

async fn cli_upload(s: &Server, project: &str, dir: &Path, launch: &str, extra: &[&str]) -> i32 {
    let tf = token_file(s.dir.path());
    let mut args: Vec<String> = [
        "fastproof",
        "upload",
        dir.to_str().unwrap(),
        "--launch",
        launch,
        "--wait",
        "--direct-min-bytes",
        "1000",
        "--url",
        &s.base,
        "--token-file",
        tf.to_str().unwrap(),
        "-p",
        project,
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    args.extend(extra.iter().map(|x| x.to_string()));
    let cli = fastproof_server::cli::Cli::try_parse_from(args).unwrap();
    fastproof_server::cli::run(cli).await.unwrap()
}

async fn attachments_of(s: &Server, project: &str, lid: &str) -> Vec<Value> {
    let rs = s.results(project, lid, "").await;
    assert_eq!(rs.len(), 1, "{rs:?}");
    let rid = rs[0]["id"].as_str().unwrap();
    let (_, d) = s
        .get(
            &format!("/api/v1/projects/{project}/results/{rid}"),
            ADMIN_TOKEN,
        )
        .await;
    let mut out = Vec::new();
    fn walk(v: &Value, out: &mut Vec<Value>) {
        match v {
            Value::Object(m) => {
                if let Some(Value::Array(a)) = m.get("attachments") {
                    out.extend(a.iter().cloned());
                }
                for x in m.values() {
                    walk(x, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    walk(&d, &mut out);
    out.sort_by_key(|a| a["name"].as_str().unwrap_or("").to_string());
    out.dedup_by_key(|a| a["id"].clone());
    out
}

#[tokio::test]
async fn cli_sends_large_files_directly_and_references_them_in_the_archive() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let _one = S3_TESTS.lock().await;
    let Some(s) = s3_server(&db, &[]).await else {
        return;
    };
    let p = format!("direct-{:08x}", rand::random::<u32>());
    s.project(&p).await;
    let video = body(50_000, 9);
    let dir = s.dir.path().join("allure-results");
    allure_dir(&dir, &video);
    assert_eq!(cli_upload(&s, &p, &dir, "cli-direct", &[]).await, 0);
    let (_, l) = s
        .get(
            &format!("/api/v1/projects/{p}/launches?externalId=cli-direct"),
            ADMIN_TOKEN,
        )
        .await;
    let lid = l["items"][0]["id"].as_str().unwrap().to_string();
    let atts = attachments_of(&s, &p, &lid).await;
    assert_eq!(atts.len(), 2, "{atts:?}");
    let rec = atts.iter().find(|a| a["name"] == "recording").unwrap();
    assert_eq!(content(&s, &p, rec["id"].as_str().unwrap()).await, video);
    let n: i64 = sqlx::query_scalar("select count(*) from direct_uploads where status = 'done'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 1, "only the large file went directly");

    let (_, ups) = s
        .get(
            &format!("/api/v1/projects/{p}/launches/{lid}/uploads"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(ups["items"][0]["status"], "done", "{ups}");

    let stray = s.dir.path().join("stray");
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::write(
        stray.join("v1-attachment.webm"),
        fastproof_server::direct_uploads::stub(
            uuid::Uuid::parse_str(rec["id"].as_str().unwrap()).unwrap(),
        ),
    )
    .unwrap();
    std::fs::write(
        stray.join("r1-result.json"),
        json!({"uuid": "r2", "name": "x", "fullName": "pkg.X#x", "status": "passed",
            "start": 1, "stop": 2,
            "attachments": [{"name": "recording", "source": "v1-attachment.webm", "type": "video/webm"}]})
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        cli_upload(&s, &p, &stray, "cli-other", &["--direct-uploads", "off"]).await,
        0
    );
    let (_, l2) = s
        .get(
            &format!("/api/v1/projects/{p}/launches?externalId=cli-other"),
            ADMIN_TOKEN,
        )
        .await;
    let lid2 = l2["items"][0]["id"].as_str().unwrap().to_string();
    let (_, ups) = s
        .get(
            &format!("/api/v1/projects/{p}/launches/{lid2}/uploads"),
            ADMIN_TOKEN,
        )
        .await;
    let warnings = ups["items"][0]["warnings"].to_string();
    assert!(
        warnings.contains("not an attachment of this launch"),
        "a reference to another launch's attachment is not followed: {ups}"
    );
    let atts2 = attachments_of(&s, &p, &lid2).await;
    assert!(
        atts2
            .iter()
            .all(|a| a["id"].is_null() || a["id"] != rec["id"]),
        "{atts2:?}"
    );
    state(&s)
        .blobs
        .delete_prefix(&fastproof_blob::BlobKey::project_prefix(&p).unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn cli_falls_back_to_the_archive_on_a_filesystem_server() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("fscli").await;
    let video = body(50_000, 3);
    let dir = s.dir.path().join("allure-results");
    allure_dir(&dir, &video);
    assert_eq!(cli_upload(&s, "fscli", &dir, "fs-1", &[]).await, 0);
    let (_, l) = s
        .get(
            "/api/v1/projects/fscli/launches?externalId=fs-1",
            ADMIN_TOKEN,
        )
        .await;
    let lid = l["items"][0]["id"].as_str().unwrap().to_string();
    let atts = attachments_of(&s, "fscli", &lid).await;
    let rec = atts.iter().find(|a| a["name"] == "recording").unwrap();
    assert_eq!(
        content(&s, "fscli", rec["id"].as_str().unwrap()).await,
        video
    );
}
