//! Seekable copies of videos: a Playwright report with a screen recording (already seekable)
//! and a MediaRecorder clip (no cues, no duration) is ingested, the `media.prepare` job runs,
//! and the content and media routes serve the copy, checked with an independent demuxer.
//! Collecting the attachment deletes the copy too; a broken video fails once and keeps its
//! original.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{ADMIN_TOKEN, Server, TestDb, tar_gz};
use reqwest::{Method, StatusCode};
use serde_json::json;
use sha2::{Digest, Sha256};

fn report_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fastproof-media/tests/data/playwright")
}

/// Makes a paused job due: the server's scheduler runs it within a tick.
async fn run_job(db: &TestDb, name: &str) {
    db.trigger_job(name).await;
}

/// Waits until no preparation is pending or running; returns `(name, status, attempts)`.
async fn prepared(db: &TestDb) -> Vec<(String, String, i32, Option<String>)> {
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    loop {
        run_job(db, "media.prepare").await;
        let rows: Vec<(String, String, i32, Option<String>)> = sqlx::query_as(
            "select coalesce(a.original_name, ''), m.status, m.attempts, m.error from attachments a \
             join attachment_media m on m.attachment_id = a.id order by 1",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        let pending: i64 = sqlx::query_scalar(
            "select count(*) from attachments where media_state = 0 and content_type like 'video/%'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        if pending == 0 && !rows.is_empty() && rows.iter().all(|r| r.1 != "running") {
            return rows;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "not prepared: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn files_under(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p.strip_prefix(dir).unwrap().to_string_lossy().to_string());
            }
        }
    }
    out
}

async fn scenario(extra: &[(&str, &str)]) {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with_paused(
        &db,
        extra,
        &["media.prepare", "attachments.gc", "projects.purge"],
    )
    .await;
    let key = format!("vid-{:08x}", rand::random::<u32>());
    s.project(&key).await;
    let launch = s.launch(&key, None).await;
    let lid = launch["id"].as_str().unwrap().to_string();
    let u = s
        .ingest(&key, &lid, tar_gz(&report_dir(), &|_| true), "")
        .await;
    assert_eq!(u["status"], "done", "{u}");

    let broken = {
        let mut v =
            std::fs::read(report_dir().join("test-results/screen-screen-recording/video.webm"))
                .unwrap();
        let at = v.windows(4).position(|w| w == b"webm").unwrap();
        v[at..at + 4].copy_from_slice(b"wxyz");
        v.truncate(20_000);
        v
    };
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/{key}/launches/{lid}/attachments"),
            Some(ADMIN_TOKEN),
        )
        .header("content-type", "video/webm")
        .header("x-fastproof-attachment-name", "broken.webm")
        .body(broken.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);

    let pending: Vec<(uuid::Uuid, Option<String>)> = sqlx::query_as(
        "select id, original_name from attachments where content_type = 'video/webm'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    for (id, name) in &pending {
        let r = s
            .req(
                Method::HEAD,
                &format!("/api/v1/projects/{key}/attachments/{id}/content"),
                Some(ADMIN_TOKEN),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(
            r.headers()["cache-control"],
            "private, no-cache",
            "{name:?}: not prepared yet, so revalidated"
        );
        let r = s
            .req(
                Method::HEAD,
                &format!("/api/v1/projects/{key}/attachments/{id}/content?original=1"),
                Some(ADMIN_TOKEN),
            )
            .send()
            .await
            .unwrap();
        assert!(
            r.headers()["cache-control"]
                .to_str()
                .unwrap()
                .contains("immutable"),
            "{name:?}: the original never changes"
        );
    }

    let rows = prepared(&db).await;
    let status = |name: &str| {
        rows.iter()
            .find(|r| r.0.contains(name))
            .unwrap_or_else(|| panic!("{name} in {rows:?}"))
            .clone()
    };
    assert_eq!(status("recording").1, "ready", "{rows:?}");
    assert_eq!(status("video").1, "native", "{rows:?}");
    let b = status("broken");
    assert_eq!((b.1.as_str(), b.2), ("failed", 1), "{rows:?}");
    assert!(b.3.is_some());

    let atts: Vec<(uuid::Uuid, String, i64, Option<String>)> = sqlx::query_as(
        "select id, sha256, size, original_name from attachments where content_type = 'video/webm'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let by_name = |n: &str| {
        atts.iter()
            .find(|a| a.3.as_deref().is_some_and(|x| x.contains(n)))
            .unwrap()
            .clone()
    };
    let (rec_id, rec_sha, rec_size, _) = by_name("recording");
    let original = std::fs::read(
        std::fs::read_dir(
            report_dir()
                .join("test-results/mediarecorder-canvas-recorded-with-MediaRecorder/attachments"),
        )
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path(),
    )
    .unwrap();
    assert_eq!(sha(&original), rec_sha);
    assert_eq!(original.len() as i64, rec_size);
    let (derived_sha, derived_size, derived_key): (String, i64, String) = sqlx::query_as(
        "select sha256, size, blob_key from attachment_media where attachment_id = $1",
    )
    .bind(rec_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(derived_key.starts_with(&format!("projects/{key}/derived/")));
    assert!(derived_key.ends_with("/seekable.webm"));

    let content = format!("/api/v1/projects/{key}/attachments/{rec_id}/content");
    let r = s
        .req(Method::GET, &content, Some(ADMIN_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        r.headers()["etag"].to_str().unwrap(),
        format!("\"{derived_sha}\"")
    );
    assert_eq!(r.headers()["content-type"], "video/webm");
    assert_eq!(
        r.headers()["cache-control"],
        "private, max-age=31536000, immutable"
    );
    let body = r.bytes().await.unwrap().to_vec();
    assert_eq!(body.len() as i64, derived_size);
    assert_eq!(sha(&body), derived_sha);

    let mid = body.len() / 2;
    let r = s
        .req(Method::GET, &content, Some(ADMIN_TOKEN))
        .header("range", format!("bytes={mid}-{}", mid + 4095))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        r.headers()["content-range"].to_str().unwrap(),
        format!("bytes {mid}-{}/{}", mid + 4095, body.len())
    );
    assert_eq!(r.bytes().await.unwrap().as_ref(), &body[mid..mid + 4096]);
    let r = s
        .req(Method::HEAD, &content, Some(ADMIN_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.headers()["content-length"],
        body.len().to_string().as_str()
    );

    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), &body).unwrap();
    let mut m =
        matroska_demuxer::MatroskaFile::open(std::fs::File::open(file.path()).unwrap()).unwrap();
    let duration = m.info().duration().expect("the copy has a duration");
    assert!(duration > 5000.0, "{duration}");
    let target = (duration * 0.8) as u64;
    m.seek(target).unwrap();
    let mut frame = matroska_demuxer::Frame::default();
    assert!(m.next_frame(&mut frame).unwrap());
    let scale = m.info().timestamp_scale().get();
    let off = frame.timestamp as i64 - target as i64;
    assert!(
        off.unsigned_abs() * scale <= 1_000_000_000,
        "seek landed {off} units away"
    );

    for q in ["?original=1", "?download=1"] {
        let r = s
            .req(Method::GET, &format!("{content}{q}"), Some(ADMIN_TOKEN))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            r.headers()["etag"].to_str().unwrap(),
            format!("\"{rec_sha}\""),
            "{q}"
        );
        assert_eq!(
            r.bytes().await.unwrap().as_ref(),
            original.as_slice(),
            "{q}"
        );
    }
    let r = s
        .req(Method::GET, &content, Some(ADMIN_TOKEN))
        .header("range", "bytes=0-99")
        .header("if-range", format!("\"{rec_sha}\""))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::OK,
        "If-Range with the original's ETag"
    );

    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{key}/attachments/{rec_sha}/media-url"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert!(st.is_success(), "{st} {b}");
    let r = s
        .req(Method::GET, b["url"].as_str().unwrap(), None)
        .header("range", format!("bytes={mid}-"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        r.headers()["etag"].to_str().unwrap(),
        format!("\"{derived_sha}\"")
    );
    assert_eq!(r.bytes().await.unwrap().as_ref(), &body[mid..]);

    for (name, want) in [("video", None), ("broken", Some(broken.as_slice()))] {
        let (id, sha256, _, _) = by_name(name);
        let r = s
            .req(
                Method::GET,
                &format!("/api/v1/projects/{key}/attachments/{id}/content"),
                Some(ADMIN_TOKEN),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            r.headers()["etag"].to_str().unwrap(),
            format!("\"{sha256}\"")
        );
        let got = r.bytes().await.unwrap();
        if let Some(w) = want {
            assert_eq!(got.as_ref(), w);
        }
    }

    prepared(&db).await;
    let again: (String, i32) =
        sqlx::query_as("select status, attempts from attachment_media m join attachments a on a.id = m.attachment_id where a.original_name = 'broken.webm'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(again, ("failed".to_string(), 1), "a failure is not retried");

    let fs = extra.is_empty();
    let blob_dir = s.dir.path().join("blobs");
    if fs {
        assert!(
            files_under(&blob_dir)
                .iter()
                .any(|f| f.contains("/derived/"))
        );
    }
    sqlx::query("delete from attachment_links where attachment_id = $1")
        .bind(rec_id)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("delete from attachment_refs where attachment_id = $1")
        .bind(rec_id)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("update attachments set last_seen_at = now() - interval '2 days' where id = $1")
        .bind(rec_id)
        .execute(&db.pool)
        .await
        .unwrap();
    let store = fastproof_store::Store::new(db.pool.clone());
    assert_eq!(
        store
            .collect_attachments(Duration::from_secs(86_400), 1000)
            .await
            .unwrap(),
        1
    );
    let queued: Vec<String> = sqlx::query_scalar("select prefix from blob_deletions order by id")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert!(queued.contains(&derived_key), "{queued:?}");
    run_job(&db, "projects.purge").await;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let left: i64 = sqlx::query_scalar("select count(*) from blob_deletions")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        if left == 0 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "blob deletions left");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if fs {
        let files = files_under(&blob_dir);
        assert!(!files.iter().any(|f| f.contains("/derived/")), "{files:?}");
        assert!(!files.iter().any(|f| f.ends_with(&rec_sha)), "{files:?}");
    }
    let mut s = s;
    s.stop().await;
}

#[tokio::test]
async fn videos_are_prepared_and_served_fs() {
    scenario(&[]).await;
}

#[tokio::test]
async fn videos_are_prepared_and_served_s3() {
    let Some(env) = common::s3_env() else {
        eprintln!("FASTPROOF_TEST_S3_* not set: skipped");
        return;
    };
    let extra: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    scenario(&extra).await;
}

async fn upload_video(s: &Server, key: &str, lid: &str, name: &str, body: Vec<u8>) {
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/{key}/launches/{lid}/attachments"),
            Some(ADMIN_TOKEN),
        )
        .header("content-type", "video/webm")
        .header("x-fastproof-attachment-name", name)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
}

fn clip() -> Vec<u8> {
    let dir = report_dir()
        .join("test-results/mediarecorder-canvas-recorded-with-MediaRecorder/attachments");
    std::fs::read(
        std::fs::read_dir(dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path(),
    )
    .unwrap()
}

/// A spool that cannot be written is not a verdict on the file: the claim stays `running`
/// with a retry time and is prepared once the spool works again.
#[tokio::test]
async fn infrastructure_errors_are_retried() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_paused(&db, &["media.prepare"]).await;
    s.project("retry").await;
    let lid = s.launch("retry", None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    upload_video(&s, "retry", &lid, "clip.webm", clip()).await;
    let media = s.dir.path().join("spool/media");
    std::fs::remove_dir_all(&media).unwrap();
    std::fs::write(&media, b"not a directory").unwrap();

    let row = || async {
        sqlx::query_as::<_, (String, i32, Option<String>, bool)>(
            "select status, attempts, error, retry_at is not null from attachment_media",
        )
        .fetch_optional(&db.pool)
        .await
        .unwrap()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let first = loop {
        run_job(&db, "media.prepare").await;
        if let Some(r) = row().await
            && r.3
        {
            break r;
        }
        assert!(std::time::Instant::now() < deadline, "no retry recorded");
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    assert_eq!((first.0.as_str(), first.1), ("running", 1), "{first:?}");
    assert!(first.2.is_some());

    std::fs::remove_file(&media).unwrap();
    sqlx::query("update attachment_media set retry_at = now()")
        .execute(&db.pool)
        .await
        .unwrap();
    let rows = prepared(&db).await;
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].1.as_str(), rows[0].2), ("ready", 2), "{rows:?}");
}

/// Claims take one video per project, projects served longest ago first; a finish for a
/// deleted project is refused (the caller then deletes the copy it stored).
#[tokio::test]
async fn claims_are_fair_and_finish_checks_the_project() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_paused(&db, &["media.prepare"]).await;
    for p in ["busy", "quiet"] {
        s.project(p).await;
    }
    let busy = s.launch("busy", None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let quiet = s.launch("quiet", None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    for i in 0..5u8 {
        let mut v = clip();
        v.push(i);
        upload_video(&s, "busy", &busy, &format!("b{i}.webm"), v).await;
    }
    upload_video(&s, "quiet", &quiet, "q.webm", clip()).await;
    let store = fastproof_store::Store::new(db.pool.clone());
    let stale = Duration::from_secs(3600);
    let first = store.claim_media_jobs(4, stale).await.unwrap();
    let keys: Vec<&str> = first.iter().map(|j| j.project_key.as_str()).collect();
    assert_eq!(first.len(), 2, "one per project: {keys:?}");
    assert!(
        keys.contains(&"busy") && keys.contains(&"quiet"),
        "{keys:?}"
    );
    let second = store.claim_media_jobs(4, stale).await.unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].project_key, "busy");

    store
        .release_media_jobs(&[second[0].attachment_id])
        .await
        .unwrap();
    let again = store.claim_media_jobs(4, stale).await.unwrap();
    let back = again
        .iter()
        .find(|j| j.attachment_id == second[0].attachment_id)
        .expect("a released claim is due at once");
    assert_eq!(back.attempt, 1, "a released claim does not count");

    let q = first.iter().find(|j| j.project_key == "quiet").unwrap();
    sqlx::query("update projects set deleted_at = now() where key = 'quiet'")
        .execute(&db.pool)
        .await
        .unwrap();
    let ready = fastproof_store::media::MediaResult::Ready {
        format: "webm".into(),
        blob_key: format!(
            "projects/quiet/derived/{}/{}/seekable.webm",
            &q.sha256[..2],
            q.sha256
        ),
        sha256: q.sha256.clone(),
        size: 1,
    };
    assert!(
        !store
            .finish_media_job(q.attachment_id, &ready)
            .await
            .unwrap()
    );
    let b = first.iter().find(|j| j.project_key == "busy").unwrap();
    assert!(
        store
            .finish_media_job(
                b.attachment_id,
                &fastproof_store::media::MediaResult::Native
            )
            .await
            .unwrap()
    );
}
