//! The test database harness itself (`support/testdb.rs`): names, removal of stale databases,
//! and a drop that does not wait for a password login in progress. Needs
//! `FASTPROOF_TEST_DATABASE_URL`; the login case needs a server that asks for a password.

#[path = "support/testdb.rs"]
mod testdb;

use std::time::{Duration, Instant};

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgConnection};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn names_carry_their_creation_time() {
    let n = testdb::new_name();
    assert!(n.starts_with("fastproof_test_t"), "{n}");
    let t = testdb::created_at(&n).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(now - t < 5);
    assert_eq!(
        testdb::created_at("fastproof_test_t1795000000_0123456789abcdef"),
        Some(1_795_000_000)
    );
    for old in [
        "fastproof_test_0123456789abcdef",
        "fastproof_test_t12_xyz",
        "fastproof_tmlocks_b0",
        "fastproof_test_tx_0123456789abcdef",
    ] {
        assert_eq!(testdb::created_at(old), None, "{old}");
    }
}

async fn exists(admin: &PgConnectOptions, name: &str) -> bool {
    let mut c = admin.connect().await.unwrap();
    let n: i64 = sqlx::query_scalar("select count(*) from pg_database where datname = $1")
        .bind(name)
        .fetch_one(&mut c)
        .await
        .unwrap();
    n == 1
}

async fn create_named(admin: &PgConnectOptions, name: &str) {
    let mut c = admin.connect().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("create database {name}")))
        .execute(&mut c)
        .await
        .unwrap();
}

#[tokio::test]
async fn stale_test_databases_are_removed() {
    let Some(admin) = testdb::admin() else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let old = format!(
        "fastproof_test_t{}_{:016x}",
        now - testdb::STALE_AFTER.as_secs() - 60,
        rand::random::<u64>()
    );
    let recent = format!(
        "fastproof_test_t{}_{:016x}",
        now - testdb::STALE_AFTER.as_secs() + 600,
        rand::random::<u64>()
    );
    create_named(&admin, &old).await;
    create_named(&admin, &recent).await;
    let dropped = testdb::remove_stale(&admin).await;
    assert!(dropped.contains(&old), "{dropped:?}");
    assert!(!dropped.contains(&recent), "{dropped:?}");
    assert!(!exists(&admin, &old).await);
    assert!(exists(&admin, &recent).await);
    testdb::drop_database(&admin, &recent);
}

/// Starts a login and stops after the server asked for a password; `None` when it asked for
/// none (a `trust` server).
async fn login_waiting_for_password(admin: &PgConnectOptions) -> Option<tokio::net::TcpStream> {
    let mut s = tokio::net::TcpStream::connect((admin.get_host(), admin.get_port()))
        .await
        .unwrap();
    let mut body = Vec::new();
    body.extend_from_slice(&196_608i32.to_be_bytes());
    for (k, v) in [("user", admin.get_username()), ("database", "postgres")] {
        body.extend_from_slice(k.as_bytes());
        body.push(0);
        body.extend_from_slice(v.as_bytes());
        body.push(0);
    }
    body.push(0);
    let mut msg = ((body.len() + 4) as i32).to_be_bytes().to_vec();
    msg.extend_from_slice(&body);
    s.write_all(&msg).await.unwrap();
    let mut head = [0u8; 9];
    s.read_exact(&mut head).await.unwrap();
    let auth = i32::from_be_bytes(head[5..9].try_into().unwrap());
    (head[0] == b'R' && auth != 0).then_some(s)
}

/// A database dropped while a password login to the server is waiting for its client: the
/// test does not wait for the login (PostgreSQL would hold the drop until the login ends), and
/// the database is gone soon after the login goes away.
#[tokio::test]
async fn a_login_in_progress_does_not_hold_up_the_drop() {
    let Some(admin) = testdb::admin() else {
        return;
    };
    let name = testdb::create(&admin).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(admin.clone().database(&name))
        .await
        .unwrap();
    sqlx::query("select 1").execute(&pool).await.unwrap();
    let mut held: Option<PgConnection> =
        Some(admin.clone().database(&name).connect().await.unwrap());
    let login = login_waiting_for_password(&admin).await;
    if login.is_none() {
        eprintln!("the server asks for no password: only the plain drop is checked");
    }
    let started = Instant::now();
    drop(pool.close());
    testdb::drop_database(&admin, &name);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the drop waited {:?}",
        started.elapsed()
    );
    drop(login);
    drop(held.take());
    let deadline = Instant::now() + Duration::from_secs(30);
    while exists(&admin, &name).await {
        assert!(Instant::now() < deadline, "{name} was not dropped");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
