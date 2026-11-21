//! The `fastproof` binary: `migrate` with database settings only, and `serve` from the
//! environment. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::TestDb;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn migrate_needs_only_the_database_and_serve_starts() {
    fastproof_blob::install_crypto_provider();
    let Some(db) = TestDb::new().await else {
        return;
    };
    let bin = env!("CARGO_BIN_EXE_fastproof");
    let out = Command::new(bin)
        .arg("migrate")
        .env_clear()
        .env("FASTPROOF_DATABASE_URL", &db.url)
        .env("FASTPROOF_LOG_FORMAT", "text")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let n: i64 = sqlx::query_scalar("select count(*) from fastproof_schema_migrations")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n as usize, fastproof_store::migrator().iter().count());
    let p: i64 = sqlx::query_scalar("select count(*) from platform_schema_migrations")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(p > 0);
    let log =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(log.contains("sslmode") && log.contains("prefer"), "{log}");
    assert!(!log.contains(&db.url), "the URL is not logged");

    let out = Command::new(bin).arg("version").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("fastproof "));

    let dir = tempfile::tempdir().unwrap();
    let tokens = dir.path().join("tokens");
    std::fs::write(&tokens, format!("admin:admin:{}\n", common::ADMIN_TOKEN)).unwrap();
    let (port, ops) = (free_port(), free_port());
    let mut child = Command::new(bin)
        .arg("serve")
        .env_clear()
        .env("FASTPROOF_DATABASE_URL", &db.url)
        .env("FASTPROOF_MIGRATE", "off")
        .env("FASTPROOF_LISTEN", format!("127.0.0.1:{port}"))
        .env("FASTPROOF_OPS_LISTEN", format!("127.0.0.1:{ops}"))
        .env("FASTPROOF_BLOB_DIR", dir.path().join("blobs"))
        .env("FASTPROOF_SPOOL_DIR", dir.path().join("spool"))
        .env("FASTPROOF_ADMIN_TOKENS_FILE", &tokens)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let ready = loop {
        if let Ok(r) = reqwest::get(format!("http://127.0.0.1:{ops}/readyz")).await
            && r.status().is_success()
        {
            break true;
        }
        if Instant::now() > deadline {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let me = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/api/v1/me"))
        .bearer_auth(common::ADMIN_TOKEN)
        .send()
        .await
        .map(|r| r.status().as_u16());
    child.kill().unwrap();
    let _ = child.wait();
    assert!(ready, "serve did not become ready");
    assert_eq!(me.unwrap(), 200);
}
