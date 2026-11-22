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
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("fastproof {}", fastproof_server::VERSION)
    );
    let out = Command::new(bin).arg("--version").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("fastproof {}", fastproof_server::VERSION)
    );

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
    let version = async {
        let r = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/api/v1/version"))
            .bearer_auth(common::ADMIN_TOKEN)
            .send()
            .await?;
        r.json::<serde_json::Value>().await
    }
    .await;
    let anonymous = reqwest::get(format!("http://127.0.0.1:{port}/api/v1/version"))
        .await
        .map(|r| r.status().as_u16());
    let metrics = async {
        reqwest::get(format!("http://127.0.0.1:{ops}/metrics"))
            .await?
            .text()
            .await
    }
    .await;
    child.kill().unwrap();
    let _ = child.wait();
    assert!(ready, "serve did not become ready");
    assert_eq!(me.unwrap(), 200);
    assert_eq!(
        version.unwrap()["version"],
        fastproof_server::VERSION,
        "/api/v1/version"
    );
    assert_eq!(anonymous.unwrap(), 401);
    let metrics = metrics.unwrap();
    assert!(
        metrics.contains(&format!(
            "fastproof_build_info{{version=\"{}\"}} 1",
            fastproof_server::VERSION
        )),
        "{metrics}"
    );
    for name in [
        "process_resident_memory_bytes",
        "process_virtual_memory_bytes",
        "process_cpu_seconds_total",
        "process_open_fds",
        "process_start_time_seconds",
    ] {
        assert_eq!(
            metrics.contains(&format!("\n{name} ")),
            cfg!(target_os = "linux"),
            "{name}: {metrics}"
        );
    }
}

/// Forwards connections on `listener` to `target`.
async fn proxy(listener: tokio::net::TcpListener, target: String) {
    loop {
        let Ok((mut inbound, _)) = listener.accept().await else {
            return;
        };
        let target = target.clone();
        tokio::spawn(async move {
            if let Ok(mut out) = tokio::net::TcpStream::connect(&target).await {
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut out).await;
            }
        });
    }
}

async fn status(url: &str) -> Option<(u16, String)> {
    let r = reqwest::get(url).await.ok()?;
    let st = r.status().as_u16();
    Some((st, r.text().await.unwrap_or_default()))
}

async fn wait_until<F, Fut>(what: &str, secs: u64, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// `serve` started before PostgreSQL (and Kafka in Kafka mode) waits for them: `/healthz`
/// answers, `/readyz` says starting, every attempt is logged, and the server becomes ready
/// once they are reachable.
#[tokio::test]
async fn serve_waits_for_postgresql_and_kafka_at_start() {
    fastproof_blob::install_crypto_provider();
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut real = url::Url::parse(&db.url).unwrap();
    let pg_target = format!(
        "{}:{}",
        real.host_str().unwrap(),
        real.port().unwrap_or(5432)
    );
    let pg_port = free_port();
    real.set_port(Some(pg_port)).unwrap();
    let kafka = std::env::var("FASTPROOF_TEST_KAFKA_BROKERS")
        .ok()
        .filter(|v| !v.is_empty());
    let kafka_port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let tokens = dir.path().join("tokens");
    std::fs::write(&tokens, format!("admin:admin:{}\n", common::ADMIN_TOKEN)).unwrap();
    let log_path = dir.path().join("serve.log");
    let ops = free_port();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fastproof"));
    cmd.arg("serve")
        .env_clear()
        .env("FASTPROOF_DATABASE_URL", real.as_str())
        .env("FASTPROOF_DATABASE_WAIT", "2m")
        .env("FASTPROOF_LISTEN", format!("127.0.0.1:{}", free_port()))
        .env("FASTPROOF_OPS_LISTEN", format!("127.0.0.1:{ops}"))
        .env("FASTPROOF_BLOB_DIR", dir.path().join("blobs"))
        .env("FASTPROOF_SPOOL_DIR", dir.path().join("spool"))
        .env("FASTPROOF_ADMIN_TOKENS_FILE", &tokens)
        .env("FASTPROOF_LOG_FORMAT", "text");
    let log_file = std::fs::File::create(&log_path).unwrap();
    cmd.stdout(log_file.try_clone().unwrap()).stderr(log_file);
    if kafka.is_some() {
        cmd.env("FASTPROOF_INGEST_MODE", "kafka")
            .env("FASTPROOF_KAFKA_BROKERS", format!("127.0.0.1:{kafka_port}"))
            .env(
                "FASTPROOF_KAFKA_TOPIC_PREFIX",
                format!("fpwait{:08x}.", rand::random::<u32>()),
            )
            .env("FASTPROOF_KAFKA_CREATE_TOPICS", "true")
            .env("FASTPROOF_KAFKA_PARTITIONS", "2")
            .env("FASTPROOF_KAFKA_CONNECT_TIMEOUT_SECONDS", "120");
    }
    let mut child = cmd.spawn().unwrap();
    let log = || std::fs::read_to_string(&log_path).unwrap_or_default();
    let health = format!("http://127.0.0.1:{ops}/healthz");
    let ready = format!("http://127.0.0.1:{ops}/readyz");

    wait_until("/healthz while PostgreSQL is down", 20, || async {
        status(&health).await.is_some_and(|(s, _)| s == 200)
    })
    .await;
    wait_until("two logged attempts", 20, || async {
        log().matches("PostgreSQL is not reachable yet").count() >= 2
    })
    .await;
    let (st, body) = status(&ready).await.unwrap();
    assert_eq!(st, 503, "{body}");
    assert!(body.contains("starting"), "{body}");
    assert!(log().contains("connection_refused"), "{}", log());
    assert!(
        child.try_wait().unwrap().is_none(),
        "the server keeps waiting"
    );

    tokio::spawn(proxy(
        tokio::net::TcpListener::bind(("127.0.0.1", pg_port))
            .await
            .unwrap(),
        pg_target,
    ));
    if let Some(brokers) = &kafka {
        wait_until("a logged Kafka attempt", 60, || async {
            log().contains("Kafka is not reachable yet")
        })
        .await;
        assert_eq!(status(&ready).await.unwrap().0, 503);
        tokio::spawn(proxy(
            tokio::net::TcpListener::bind(("127.0.0.1", kafka_port))
                .await
                .unwrap(),
            brokers.split(',').next().unwrap().trim().to_string(),
        ));
    }
    let became_ready = {
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            if status(&ready).await.is_some_and(|(s, _)| s == 200) {
                break true;
            }
            if Instant::now() > deadline || child.try_wait().unwrap().is_some() {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    assert!(became_ready, "the server became ready:\n{}", log());
    assert!(log().contains("PostgreSQL is reachable"), "{}", log());
}

/// Without PostgreSQL, `serve` gives up after the wait (here set by the older
/// `FASTPROOF_DATABASE_CONNECT_TIMEOUT_SECONDS`) with a clear error.
#[tokio::test]
async fn serve_gives_up_after_the_connect_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let tokens = dir.path().join("tokens");
    std::fs::write(&tokens, format!("admin:admin:{}\n", common::ADMIN_TOKEN)).unwrap();
    let started = Instant::now();
    let out = tokio::task::spawn_blocking({
        let dir = dir.path().to_path_buf();
        move || {
            Command::new(env!("CARGO_BIN_EXE_fastproof"))
                .arg("serve")
                .env_clear()
                .env(
                    "FASTPROOF_DATABASE_URL",
                    format!("postgres://fp:secret-pw@127.0.0.1:{}/fp", free_port()),
                )
                .env("FASTPROOF_DATABASE_CONNECT_TIMEOUT_SECONDS", "3")
                .env("FASTPROOF_LISTEN", "127.0.0.1:0")
                .env("FASTPROOF_OPS_LISTEN", "127.0.0.1:0")
                .env("FASTPROOF_BLOB_DIR", dir.join("blobs"))
                .env("FASTPROOF_SPOOL_DIR", dir.join("spool"))
                .env("FASTPROOF_ADMIN_TOKENS_FILE", tokens)
                .env("FASTPROOF_LOG_FORMAT", "text")
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("PostgreSQL was not reachable within")
            && text.contains("FASTPROOF_DATABASE_WAIT"),
        "{text}"
    );
    assert!(text.contains("PostgreSQL is not reachable yet"), "{text}");
    assert!(!text.contains("secret-pw"), "the password is not logged");
    assert!(started.elapsed() < Duration::from_secs(30));
}

/// `migrate` started before PostgreSQL is reachable waits for it (one warning per attempt with
/// the error class) and then applies the migrations.
#[tokio::test]
async fn migrate_waits_for_postgresql() {
    fastproof_blob::install_crypto_provider();
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut url = url::Url::parse(&db.url).unwrap();
    let target = format!("{}:{}", url.host_str().unwrap(), url.port().unwrap_or(5432));
    let port = free_port();
    url.set_port(Some(port)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("migrate.log");
    let log_file = std::fs::File::create(&log_path).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_fastproof"))
        .arg("migrate")
        .env_clear()
        .env("FASTPROOF_DATABASE_URL", url.as_str())
        .env("FASTPROOF_DATABASE_WAIT", "2m")
        .env("FASTPROOF_LOG_FORMAT", "text")
        .stdout(log_file.try_clone().unwrap())
        .stderr(log_file)
        .spawn()
        .unwrap();
    let log = || std::fs::read_to_string(&log_path).unwrap_or_default();
    wait_until("two logged attempts", 20, || async {
        log().matches("PostgreSQL is not reachable yet").count() >= 2
    })
    .await;
    assert!(log().contains("connection_refused"), "{}", log());
    assert!(child.try_wait().unwrap().is_none(), "migrate keeps waiting");
    tokio::spawn(proxy(
        tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap(),
        target,
    ));
    let deadline = Instant::now() + Duration::from_secs(90);
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break Some(st);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let _ = child.wait();
    assert!(status.is_some_and(|s| s.success()), "{}", log());
    assert!(log().contains("PostgreSQL is reachable"), "{}", log());
    let n: i64 = sqlx::query_scalar("select count(*) from fastproof_schema_migrations")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n as usize, fastproof_store::migrator().iter().count());
}
