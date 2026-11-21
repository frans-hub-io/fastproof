//! The HTTP client of the CLI commands.

use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use fastproof_store::launches::LaunchStart;
use fastproof_store::{Launch, Upload};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use uuid::Uuid;

/// A FastProof server.
#[derive(Clone)]
pub struct Client {
    base: url::Url,
    token: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base.as_str())
            .finish_non_exhaustive()
    }
}

/// An error answered by the server.
#[derive(Debug)]
pub struct ServerError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "server answered {} {}: {}",
            self.status.as_u16(),
            self.code,
            self.message
        )
    }
}

impl std::error::Error for ServerError {}

/// The list wrapper `{items}`.
#[derive(serde::Deserialize)]
pub struct Items<T> {
    pub items: Vec<T>,
}

impl Client {
    pub fn new(base: &str, token: &str) -> anyhow::Result<Self> {
        fastproof_blob::install_crypto_provider();
        let mut base = url::Url::parse(base.trim()).context("FASTPROOF_URL: not a URL")?;
        if !matches!(base.scheme(), "http" | "https") {
            bail!("FASTPROOF_URL: expected an http(s) URL");
        }
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        if token.trim().is_empty() {
            bail!("FASTPROOF_TOKEN (or --token-file) is required");
        }
        let http = reqwest::Client::builder()
            .user_agent(concat!("fastproof-cli/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self {
            base,
            token: token.trim().to_string(),
            http,
        })
    }

    pub fn request(&self, method: Method, path: &str) -> anyhow::Result<RequestBuilder> {
        let url = self.base.join(path.trim_start_matches('/'))?;
        Ok(self.http.request(method, url).bearer_auth(&self.token))
    }

    async fn check(resp: Response) -> anyhow::Result<Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        Err(ServerError {
            status,
            code: body["code"].as_str().unwrap_or("error").to_string(),
            message: body["message"]
                .as_str()
                .unwrap_or("request failed")
                .to_string(),
        }
        .into())
    }

    pub async fn send<T: DeserializeOwned>(&self, rb: RequestBuilder) -> anyhow::Result<T> {
        let resp = rb.send().await.context("contacting the FastProof server")?;
        Ok(Self::check(resp).await?.json().await?)
    }

    fn enc(s: &str) -> String {
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    }

    /// Creates a test plan and returns its id.
    pub async fn create_test_plan(
        &self,
        project: &str,
        body: &serde_json::Value,
    ) -> anyhow::Result<Uuid> {
        let rb = self
            .request(
                Method::POST,
                &format!("api/v1/projects/{}/test-plans", Self::enc(project)),
            )?
            .json(body);
        let v: serde_json::Value = self.send(rb).await?;
        v["id"]
            .as_str()
            .and_then(|i| i.parse().ok())
            .ok_or_else(|| anyhow!("the server did not return a plan id"))
    }

    /// A test plan as Allure `testplan.json`.
    pub async fn testplan_json(
        &self,
        project: &str,
        id: Uuid,
    ) -> anyhow::Result<serde_json::Value> {
        let rb = self.request(
            Method::GET,
            &format!(
                "api/v1/projects/{}/test-plans/{id}/testplan.json",
                Self::enc(project)
            ),
        )?;
        self.send(rb).await
    }

    /// Starts or joins a launch.
    pub async fn start_launch(&self, project: &str, start: &LaunchStart) -> anyhow::Result<Launch> {
        let rb = self
            .request(
                Method::POST,
                &format!("api/v1/projects/{}/launches", Self::enc(project)),
            )?
            .json(start);
        self.send(rb).await
    }

    pub async fn finish_launch(
        &self,
        project: &str,
        id: Uuid,
        status: &str,
    ) -> anyhow::Result<Launch> {
        let rb = self
            .request(
                Method::POST,
                &format!(
                    "api/v1/projects/{}/launches/{id}/finish",
                    Self::enc(project)
                ),
            )?
            .json(&serde_json::json!({"status": status}));
        self.send(rb).await
    }

    pub async fn launch(&self, project: &str, id: Uuid) -> anyhow::Result<Launch> {
        let rb = self.request(
            Method::GET,
            &format!("api/v1/projects/{}/launches/{id}", Self::enc(project)),
        )?;
        self.send(rb).await
    }

    /// The launch with an external id, if any.
    pub async fn launch_by_external(
        &self,
        project: &str,
        external_id: &str,
    ) -> anyhow::Result<Option<Launch>> {
        let rb = self
            .request(
                Method::GET,
                &format!("api/v1/projects/{}/launches", Self::enc(project)),
            )?
            .query(&[("externalId", external_id), ("limit", "1")]);
        Ok(self
            .send::<Items<Launch>>(rb)
            .await?
            .items
            .into_iter()
            .next())
    }

    /// The quality gate of a launch.
    pub async fn gate(&self, project: &str, id: Uuid) -> anyhow::Result<serde_json::Value> {
        let rb = self.request(
            Method::GET,
            &format!("api/v1/projects/{}/launches/{id}/gate", Self::enc(project)),
        )?;
        self.send(rb).await
    }

    /// Sends an upload body.
    pub async fn upload(
        &self,
        project: &str,
        launch: Uuid,
        format: &str,
        file_name: Option<&str>,
        body: reqwest::Body,
    ) -> anyhow::Result<Upload> {
        let mut rb = self
            .request(
                Method::POST,
                &format!(
                    "api/v1/projects/{}/launches/{launch}/uploads",
                    Self::enc(project)
                ),
            )?
            .query(&[("format", format)])
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(body);
        if let Some(n) = file_name {
            rb = rb.query(&[("fileName", n)]);
        }
        self.send(rb).await
    }

    pub async fn uploads(&self, project: &str, launch: Uuid) -> anyhow::Result<Vec<Upload>> {
        let rb = self.request(
            Method::GET,
            &format!(
                "api/v1/projects/{}/launches/{launch}/uploads",
                Self::enc(project)
            ),
        )?;
        Ok(self.send::<Items<Upload>>(rb).await?.items)
    }

    /// Polls until no upload of the launch is queued or processing, or `timeout` passes;
    /// returns how many still are.
    pub async fn wait_launch_uploads(
        &self,
        project: &str,
        launch: Uuid,
        timeout: Duration,
    ) -> anyhow::Result<usize> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut delay = Duration::from_millis(200);
        loop {
            let open = super::pending_uploads(&self.uploads(project, launch).await?);
            if open == 0 || tokio::time::Instant::now() >= deadline {
                return Ok(open);
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(3));
        }
    }

    /// Polls until the given uploads are done or failed, or `timeout` passes.
    pub async fn wait_uploads(
        &self,
        project: &str,
        launch: Uuid,
        ids: &[Uuid],
        timeout: Duration,
    ) -> anyhow::Result<Vec<Upload>> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut delay = Duration::from_millis(200);
        loop {
            let all = self.uploads(project, launch).await?;
            let mine: Vec<Upload> = all.into_iter().filter(|u| ids.contains(&u.id)).collect();
            if mine.len() == ids.len()
                && mine
                    .iter()
                    .all(|u| u.status == "done" || u.status == "failed")
            {
                return Ok(mine);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(anyhow!("timed out waiting for the uploads to be processed"));
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(3));
        }
    }
}
