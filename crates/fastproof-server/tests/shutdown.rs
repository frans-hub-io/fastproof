//! SIGTERM: the `fastproof` binary finishes the requests in flight, ends its event streams and
//! exits within `FASTPROOF_SHUTDOWN_TIMEOUT_SECONDS`. Unix only; skipped without
//! `FASTPROOF_TEST_DATABASE_URL`.

#![cfg(unix)]

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, TestDb};
use futures::StreamExt;
use serde_json::{Value, json};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn sigterm_finishes_requests_in_flight_and_exits_within_the_timeout() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let bin = env!("CARGO_BIN_EXE_fastproof");
    let dir = tempfile::tempdir().unwrap();
    let tokens = dir.path().join("tokens");
    std::fs::write(&tokens, format!("admin:admin:{ADMIN_TOKEN}\n")).unwrap();
    let (port, ops) = (free_port(), free_port());
    let mut child = Command::new(bin)
        .arg("serve")
        .env_clear()
        .env("FASTPROOF_DATABASE_URL", &db.url)
        .env("FASTPROOF_LISTEN", format!("127.0.0.1:{port}"))
        .env("FASTPROOF_OPS_LISTEN", format!("127.0.0.1:{ops}"))
        .env("FASTPROOF_BLOB_DIR", dir.path().join("blobs"))
        .env("FASTPROOF_SPOOL_DIR", dir.path().join("spool"))
        .env("FASTPROOF_ADMIN_TOKENS_FILE", &tokens)
        .env("FASTPROOF_SHUTDOWN_TIMEOUT_SECONDS", "8")
        .env("FASTPROOF_LOG_FORMAT", "text")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !reqwest::get(format!("http://127.0.0.1:{ops}/readyz"))
        .await
        .is_ok_and(|r| r.status().is_success())
    {
        assert!(Instant::now() < deadline, "serve did not become ready");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let r = http
        .post(format!("{base}/api/v1/projects"))
        .bearer_auth(ADMIN_TOKEN)
        .json(&json!({"key": "sd", "name": "sd"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let launch: Value = http
        .post(format!("{base}/api/v1/projects/sd/launches"))
        .bearer_auth(ADMIN_TOKEN)
        .json(&json!({"name": "nightly"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let lid = launch["id"].as_str().unwrap().to_string();

    // A browser following the project's events: the stream never ends by itself.
    let events = http
        .get(format!("{base}/api/v1/projects/sd/events"))
        .bearer_auth(ADMIN_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(events.status(), 200);
    let events = tokio::spawn(async move {
        let mut body = events.bytes_stream();
        while let Some(Ok(_)) = body.next().await {}
        Instant::now()
    });

    // An upload of results still being sent when SIGTERM arrives.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(4);
    let line = |i: usize| {
        format!(
            "{}\n",
            json!({"id": format!("t{i}"), "name": format!("case {i}"), "status": "passed"})
        )
        .into_bytes()
    };
    tx.send(Ok((0..10).flat_map(line).collect())).await.unwrap();
    let upload = tokio::spawn({
        let http = http.clone();
        let url = format!("{base}/api/v1/projects/sd/launches/{lid}/results");
        async move {
            let r = http
                .post(url)
                .bearer_auth(ADMIN_TOKEN)
                .header("content-type", "application/x-ndjson")
                .body(reqwest::Body::wrap_stream(
                    tokio_stream::wrappers::ReceiverStream::new(rx),
                ))
                .send()
                .await
                .unwrap();
            let st = r.status().as_u16();
            (st, r.json::<Value>().await.unwrap_or(Value::Null))
        }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;

    let term = Instant::now();
    let st = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(st.success());
    tokio::time::sleep(Duration::from_secs(2)).await;
    tx.send(Ok((10..20).flat_map(line).collect()))
        .await
        .unwrap();
    drop(tx);

    let (status, body) = upload.await.unwrap();
    assert_eq!(status, 202, "the request in flight is finished: {body}");
    assert_eq!(body["accepted"], 20);
    let ended = tokio::time::timeout(Duration::from_secs(40), events)
        .await
        .ok()
        .map(|e| e.unwrap().duration_since(term));
    let exit = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if term.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            panic!("fastproof is still running a minute after SIGTERM");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let took = term.elapsed();
    eprintln!("event stream ended after {ended:?}, process exited after {took:?} ({exit})");
    let ended = ended.expect("the event stream ends on shutdown");
    assert!(
        ended < Duration::from_secs(8),
        "the event stream ended {ended:?} after SIGTERM"
    );
    assert!(exit.success(), "exit status {exit}");
    assert!(
        took < Duration::from_secs(10),
        "stopped {took:?} after SIGTERM with an 8 s timeout"
    );
    let stored: i64 = sqlx::query_scalar("select count(*) from attempts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        stored, 20,
        "the results of the request in flight are stored"
    );
}
