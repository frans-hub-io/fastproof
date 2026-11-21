//! Test harness: a fresh `fastproof_test_*` database per test (dropped afterwards, also on
//! failure) and servers on random ports. Tests need `FASTPROOF_TEST_DATABASE_URL`; S3 tests
//! also `FASTPROOF_TEST_S3_*`. Without them they pass without doing anything.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fastproof_server::{Config, Running};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgPool};

pub const ADMIN_TOKEN: &str = "admin-token-for-tests-0123456789";

pub struct TestDb {
    pub admin: PgConnectOptions,
    pub name: String,
    pub url: String,
    pub pool: PgPool,
}

impl TestDb {
    pub async fn new() -> Option<Self> {
        fastproof_blob::install_crypto_provider();
        let Ok(url) = std::env::var("FASTPROOF_TEST_DATABASE_URL") else {
            eprintln!("FASTPROOF_TEST_DATABASE_URL is not set: skipped");
            return None;
        };
        let admin: PgConnectOptions = url.parse().expect("FASTPROOF_TEST_DATABASE_URL");
        let name = format!("fastproof_test_{:016x}", rand::random::<u64>());
        let mut conn = admin
            .connect()
            .await
            .expect("connecting to the test server");
        sqlx::query(sqlx::AssertSqlSafe(format!("create database {name}")))
            .execute(&mut conn)
            .await
            .unwrap();
        let mut u = url::Url::parse(&url).unwrap();
        u.set_path(&format!("/{name}"));
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(admin.clone().database(&name))
            .await
            .unwrap();
        Some(Self {
            admin,
            name,
            url: u.to_string(),
            pool,
        })
    }

    /// Drops the database while servers are connected (readiness tests).
    pub async fn drop_now(&self) {
        let mut c = self.admin.connect().await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "drop database if exists {} with (force)",
            self.name
        )))
        .execute(&mut c)
        .await
        .unwrap();
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let admin = self.admin.clone();
        let name = self.name.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                if let Ok(mut c) = admin.connect().await {
                    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                        "drop database if exists {name} with (force)"
                    )))
                    .execute(&mut c)
                    .await;
                }
            });
        })
        .join()
        .unwrap();
    }
}

/// S3 settings of the test server, when configured.
pub fn s3_env() -> Option<Vec<(String, String)>> {
    let var = |k: &str| std::env::var(format!("FASTPROOF_TEST_S3_{k}")).ok();
    let endpoint = var("ENDPOINT")?;
    Some(vec![
        ("FASTPROOF_BLOB_BACKEND".into(), "s3".into()),
        ("FASTPROOF_S3_ENDPOINT".into(), endpoint),
        (
            "FASTPROOF_S3_REGION".into(),
            var("REGION").unwrap_or_else(|| "us-east-1".into()),
        ),
        ("FASTPROOF_S3_BUCKET".into(), var("BUCKET")?),
        ("FASTPROOF_S3_ACCESS_KEY".into(), var("ACCESS_KEY")?),
        ("FASTPROOF_S3_SECRET_KEY".into(), var("SECRET_KEY")?),
    ])
}

pub struct Server {
    pub running: Option<Running>,
    pub base: String,
    pub ops: String,
    pub dir: tempfile::TempDir,
    pub http: reqwest::Client,
}

impl Server {
    pub async fn start(db: &TestDb) -> Self {
        Self::start_with(db, &[]).await
    }

    pub async fn start_with(db: &TestDb, extra: &[(&str, &str)]) -> Self {
        Self::start_cfg(db, extra, |_| {}).await
    }

    pub async fn start_cfg(
        db: &TestDb,
        extra: &[(&str, &str)],
        tweak: impl FnOnce(&mut Config),
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let tokens = dir.path().join("tokens");
        std::fs::write(&tokens, format!("admin:admin:{ADMIN_TOKEN}\n")).unwrap();
        let mut vars: HashMap<String, String> = [
            ("FASTPROOF_DATABASE_URL", db.url.as_str()),
            ("FASTPROOF_LISTEN", "127.0.0.1:0"),
            ("FASTPROOF_OPS_LISTEN", "127.0.0.1:0"),
            ("FASTPROOF_ADMIN_TOKENS_FILE", tokens.to_str().unwrap()),
            ("FASTPROOF_INGEST_WORKERS", "2"),
            ("FASTPROOF_DATABASE_MAX_CONNECTIONS", "8"),
            ("FASTPROOF_LOG_FORMAT", "text"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        vars.insert(
            "FASTPROOF_BLOB_DIR".into(),
            dir.path().join("blobs").to_string_lossy().into(),
        );
        vars.insert(
            "FASTPROOF_SPOOL_DIR".into(),
            dir.path().join("spool").to_string_lossy().into(),
        );
        for (k, v) in extra {
            vars.insert(k.to_string(), v.to_string());
        }
        let mut cfg = Config::from_lookup(&|k| vars.get(k).cloned()).unwrap();
        tweak(&mut cfg);
        let running = fastproof_server::start(cfg, Some(prometheus()))
            .await
            .unwrap();
        let base = format!("http://{}", running.addr);
        let ops = format!("http://{}", running.ops_addr);
        Self {
            running: Some(running),
            base,
            ops,
            dir,
            http: reqwest::Client::new(),
        }
    }

    pub async fn stop(&mut self) {
        if let Some(r) = self.running.take() {
            r.shutdown().await;
        }
    }

    pub fn req(&self, method: Method, path: &str, token: Option<&str>) -> reqwest::RequestBuilder {
        let rb = self.http.request(method, format!("{}{path}", self.base));
        match token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    pub async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut rb = self.req(method, path, token);
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let r = rb.send().await.unwrap();
        let status = r.status();
        let text = r.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    pub async fn get(&self, path: &str, token: &str) -> (StatusCode, Value) {
        self.call(Method::GET, path, Some(token), None).await
    }

    pub async fn post(&self, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
        self.call(Method::POST, path, Some(token), Some(body)).await
    }

    pub async fn project(&self, key: &str) {
        let (s, b) = self
            .post(
                "/api/v1/projects",
                ADMIN_TOKEN,
                json!({"key": key, "name": key}),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
    }

    pub async fn launch(&self, project: &str, external: Option<&str>) -> Value {
        let mut body = json!({"name": "nightly"});
        if let Some(e) = external {
            body["externalId"] = json!(e);
        }
        let (s, b) = self
            .post(
                &format!("/api/v1/projects/{project}/launches"),
                ADMIN_TOKEN,
                body,
            )
            .await;
        assert!(s.is_success(), "{s} {b}");
        b
    }

    /// A managed token with a role in some scopes.
    pub async fn token(&self, name: &str, role: &str, scopes: &[&str]) -> String {
        let (s, b) = self
            .post(
                "/api/v1/tokens",
                ADMIN_TOKEN,
                json!({"name": name, "role": role, "scopes": scopes, "expiresInDays": 1}),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        b["token"].as_str().unwrap().to_string()
    }

    /// Uploads bytes and returns the upload.
    pub async fn upload(
        &self,
        project: &str,
        launch: &str,
        bytes: Vec<u8>,
        query: &str,
    ) -> (StatusCode, Value) {
        let r = self
            .req(
                Method::POST,
                &format!("/api/v1/projects/{project}/launches/{launch}/uploads{query}"),
                Some(ADMIN_TOKEN),
            )
            .header("content-type", "application/octet-stream")
            .body(bytes)
            .send()
            .await
            .unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    /// Waits until the upload is done or failed.
    pub async fn wait_upload(&self, project: &str, launch: &str, id: &str) -> Value {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let (_, b) = self
                .get(
                    &format!("/api/v1/projects/{project}/launches/{launch}/uploads"),
                    ADMIN_TOKEN,
                )
                .await;
            if let Some(u) = b["items"]
                .as_array()
                .and_then(|a| a.iter().find(|u| u["id"] == id))
                && (u["status"] == "done" || u["status"] == "failed")
            {
                return u.clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "upload {id} not processed: {b}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Uploads and waits.
    pub async fn ingest(&self, project: &str, launch: &str, bytes: Vec<u8>, query: &str) -> Value {
        let (s, u) = self.upload(project, launch, bytes, query).await;
        assert_eq!(s, StatusCode::ACCEPTED, "{u}");
        self.wait_upload(project, launch, u["id"].as_str().unwrap())
            .await
    }

    pub async fn launch_get(&self, project: &str, launch: &str) -> Value {
        self.get(
            &format!("/api/v1/projects/{project}/launches/{launch}"),
            ADMIN_TOKEN,
        )
        .await
        .1
    }

    /// All results of a launch.
    pub async fn results(&self, project: &str, launch: &str, query: &str) -> Vec<Value> {
        let mut out = Vec::new();
        let mut before: Option<String> = None;
        loop {
            let mut q = format!("?limit=500{query}");
            if let Some(b) = &before {
                q.push_str(&format!("&before={b}"));
            }
            let (s, b) = self
                .get(
                    &format!("/api/v1/projects/{project}/launches/{launch}/results{q}"),
                    ADMIN_TOKEN,
                )
                .await;
            assert_eq!(s, StatusCode::OK, "{b}");
            out.extend(b["items"].as_array().unwrap().iter().cloned());
            match b["next"].as_str() {
                Some(n) => before = Some(n.to_string()),
                None => return out,
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(r) = self.running.take() {
            let stop = r.stopped();
            stop.cancel();
            drop(r);
        }
    }
}

/// One Prometheus recorder for the whole test process.
pub fn prometheus() -> metrics_exporter_prometheus::PrometheusHandle {
    static H: std::sync::OnceLock<metrics_exporter_prometheus::PrometheusHandle> =
        std::sync::OnceLock::new();
    H.get_or_init(|| {
        franshub_observe::install_prometheus().unwrap_or_else(|_| {
            franshub_observe::prometheus_builder()
                .unwrap()
                .build_recorder()
                .handle()
        })
    })
    .clone()
}

pub fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(rel)
}

/// A tar.gz of a directory (or of selected files of it).
pub fn tar_gz(dir: &Path, filter: &dyn Fn(&str) -> bool) -> Vec<u8> {
    let files: Vec<(PathBuf, String)> = fastproof_server::cli::archive::files_under(dir, 100_000)
        .unwrap()
        .into_iter()
        .filter(|(_, rel)| filter(rel))
        .collect();
    fastproof_server::cli::archive::tar_gz_bytes(&files).unwrap()
}

pub fn read(rel: &str) -> Vec<u8> {
    std::fs::read(fixture(rel)).unwrap()
}
