//! The read client of the ReportPortal REST API: pagination, retries, a rate limit, TLS, and the
//! shapes of launches, items and log entries as `rp import` needs them. It never writes to
//! ReportPortal and sends the API key only to the configured URL (redirects are not followed).

use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use fastproof_rp::model::{ExternalIssue, Issue, ItemAttribute, ItemParameter};
use fastproof_rp::time as rp_time;
use reqwest::{Response, StatusCode};
use serde_json::Value;
use tokio::sync::Mutex;

use super::super::client::Client;

/// Most items of a ReportPortal page.
pub const PAGE_SIZE: usize = 300;
/// A JSON answer is read up to this size.
const MAX_JSON_BYTES: usize = 64 * 1024 * 1024;
/// Items whose logs one request asks for.
pub const LOG_CHUNK: usize = 50;
/// The body of an error answer is read up to this size.
const MAX_ERROR_BYTES: usize = 64 * 1024;

/// What one launch may bring from ReportPortal before its import is refused, so that a broken
/// or hostile server cannot make the CLI read forever or exhaust its memory.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Items of a launch, earlier attempts included (the server's own default limit).
    pub max_items: usize,
    /// Bytes of the item pages of a launch.
    pub max_item_bytes: usize,
    /// Bytes of the log pages of one chunk of items.
    pub max_log_bytes: usize,
    /// Pages of one list.
    pub max_pages: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_items: 1_000_000,
            max_item_bytes: 512 << 20,
            max_log_bytes: 256 << 20,
            max_pages: 100_000,
        }
    }
}

/// A failed ReportPortal request.
#[derive(Debug)]
pub enum RpError {
    /// The answer is larger than the limit of the request.
    TooLarge(usize),
    /// ReportPortal answered with this status.
    Status {
        status: StatusCode,
        path: String,
        message: String,
    },
}

impl std::fmt::Display for RpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(max) => write!(f, "the answer is larger than {max} bytes"),
            Self::Status {
                status,
                path,
                message,
            } => write!(f, "ReportPortal answered {status} for {path}: {message}"),
        }
    }
}

impl std::error::Error for RpError {}

/// A JSON page: the parsed answer, its size and a hash of its bytes.
struct Page {
    json: Value,
    bytes: usize,
    hash: u64,
}

/// Spaces requests out: at most `rate` per second over all tasks.
pub struct Limiter {
    interval: Duration,
    next: Mutex<Instant>,
}

impl Limiter {
    pub fn new(per_second: f64) -> Self {
        Self {
            interval: Duration::from_secs_f64(1.0 / per_second.max(0.01)),
            next: Mutex::new(Instant::now()),
        }
    }

    pub async fn wait(&self) {
        let mut next = self.next.lock().await;
        let now = Instant::now();
        if *next > now {
            tokio::time::sleep(*next - now).await;
        }
        *next = (*next).max(now) + self.interval;
    }
}

/// Connection settings of the source ReportPortal.
pub struct RpConn {
    pub url: String,
    pub project: String,
    pub key: String,
    pub ca_file: Option<std::path::PathBuf>,
    pub allow_http: bool,
    pub rate: f64,
    pub retry_delay: Duration,
}

pub struct RpReader {
    base: url::Url,
    project: String,
    key: String,
    http: reqwest::Client,
    limiter: Limiter,
    retry_delay: Duration,
    limits: Limits,
}

impl std::fmt::Debug for RpReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpReader")
            .field("base", &self.base.as_str())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct RpLaunch {
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub number: i64,
    pub start_ms: i64,
    pub end_ms: Option<i64>,
    pub status: String,
    pub debug: bool,
    pub description: Option<String>,
    pub attributes: Vec<ItemAttribute>,
}

impl RpLaunch {
    pub fn finished(&self) -> bool {
        !self.status.eq_ignore_ascii_case("IN_PROGRESS") && self.end_ms.is_some()
    }
}

#[derive(Clone, Debug, Default)]
pub struct RpItem {
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub kind: String,
    pub start_ms: i64,
    pub end_ms: Option<i64>,
    pub status: String,
    pub parent: Option<i64>,
    pub has_stats: bool,
    pub code_ref: Option<String>,
    pub test_case_id: Option<String>,
    pub parameters: Vec<ItemParameter>,
    pub attributes: Vec<ItemAttribute>,
    pub description: Option<String>,
    pub issue: Option<Issue>,
    pub retries: Vec<RpItem>,
}

#[derive(Clone, Debug)]
pub struct RpLog {
    pub id: Option<i64>,
    pub item_id: Option<i64>,
    pub uuid: Option<String>,
    pub time_ms: i64,
    pub message: String,
    /// `error`, `warn`, `info`, `debug` or `trace`.
    pub level: &'static str,
    pub file: Option<RpFileRef>,
}

#[derive(Clone, Debug)]
pub struct RpFileRef {
    pub id: String,
    pub content_type: String,
}

/// A downloaded attachment.
#[derive(Debug)]
pub struct RpFile {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
    pub file_name: Option<String>,
}

/// The defect types of the source project: `locator -> (typeRef, longName)`.
pub type DefectTypes = std::collections::HashMap<String, (String, String)>;

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn num(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn list<T: serde::de::DeserializeOwned>(v: &Value) -> Vec<T> {
    serde_json::from_value::<Option<Vec<T>>>(v.clone())
        .ok()
        .flatten()
        .unwrap_or_default()
}

pub fn launch_from(v: &Value) -> Option<RpLaunch> {
    let id = num(&v["id"])?;
    Some(RpLaunch {
        id,
        uuid: text(&v["uuid"]).unwrap_or_else(|| id.to_string()),
        name: text(&v["name"]).unwrap_or_else(|| format!("launch {id}")),
        number: num(&v["number"]).unwrap_or(0),
        start_ms: rp_time::parse(&v["startTime"]).unwrap_or(0),
        end_ms: rp_time::parse(&v["endTime"]),
        status: text(&v["status"]).unwrap_or_default(),
        debug: v["mode"]
            .as_str()
            .is_some_and(|m| m.eq_ignore_ascii_case("debug")),
        description: text(&v["description"]),
        attributes: list(&v["attributes"]),
    })
}

pub fn item_from(v: &Value) -> Option<RpItem> {
    let id = num(&v["id"])?;
    let issue = match &v["issue"] {
        Value::Object(_) => serde_json::from_value::<Issue>(v["issue"].clone()).ok(),
        _ => None,
    };
    Some(RpItem {
        id,
        uuid: text(&v["uuid"])?,
        name: text(&v["name"]).unwrap_or_else(|| "unnamed".into()),
        kind: text(&v["type"]).unwrap_or_else(|| "STEP".into()),
        start_ms: rp_time::parse(&v["startTime"]).unwrap_or(0),
        end_ms: rp_time::parse(&v["endTime"]),
        status: text(&v["status"]).unwrap_or_default(),
        parent: num(&v["parent"]),
        has_stats: !matches!(&v["hasStats"], Value::Bool(false)),
        code_ref: text(&v["codeRef"]),
        test_case_id: text(&v["testCaseId"]),
        parameters: list(&v["parameters"]),
        attributes: list(&v["attributes"]),
        description: text(&v["description"]),
        issue,
        retries: v["retries"]
            .as_array()
            .map(|a| a.iter().filter_map(item_from).collect())
            .unwrap_or_default(),
    })
}

pub fn log_from(v: &Value) -> Option<RpLog> {
    let file = match &v["binaryContent"] {
        Value::Object(o) => text(o.get("id").unwrap_or(&Value::Null)).map(|id| RpFileRef {
            id,
            content_type: text(o.get("contentType").unwrap_or(&Value::Null)).unwrap_or_default(),
        }),
        _ => None,
    };
    Some(RpLog {
        id: num(&v["id"]),
        item_id: num(&v["itemId"]).or_else(|| num(&v["item"])),
        uuid: text(&v["uuid"]),
        time_ms: rp_time::parse(&v["logTime"])
            .or_else(|| rp_time::parse(&v["time"]))
            .unwrap_or(0),
        message: v["message"].as_str().unwrap_or_default().to_string(),
        level: fastproof_rp::status::log_level(&v["level"]),
        file,
    })
}

impl RpReader {
    pub fn new(c: &RpConn) -> anyhow::Result<Self> {
        fastproof_blob::install_crypto_provider();
        let mut base = url::Url::parse(c.url.trim()).context("--rp-url: not a URL")?;
        let loopback = match base.host() {
            Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(a)) => a.is_loopback(),
            Some(url::Host::Ipv6(a)) => a.is_loopback(),
            None => false,
        };
        match base.scheme() {
            "https" => {}
            "http" if loopback || c.allow_http => {}
            "http" => bail!(
                "--rp-url: the API key would travel unencrypted; use https (or --rp-allow-http)"
            ),
            _ => bail!("--rp-url: expected an https URL"),
        }
        if !base.username().is_empty() || base.password().is_some() {
            bail!(
                "--rp-url: put no credentials in the URL; the API key comes from RP_API_KEY or --rp-key-file"
            );
        }
        if base.query().is_some() || base.fragment().is_some() {
            bail!("--rp-url: expected the base URL of ReportPortal, without a query or fragment");
        }
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        if c.key.trim().is_empty() {
            bail!("the ReportPortal API key is required (RP_API_KEY or --rp-key-file)");
        }
        if !c.key.trim().bytes().all(|b| b.is_ascii_graphic()) {
            bail!(
                "the ReportPortal API key holds spaces or control characters; check the key file"
            );
        }
        if c.project.trim().is_empty() {
            bail!("--rp-project is required");
        }
        let mut b = reqwest::Client::builder()
            .user_agent(format!("fastproof-cli/{}", crate::VERSION))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(600))
            .redirect(reqwest::redirect::Policy::none());
        if let Some(f) = &c.ca_file {
            let pem = std::fs::read(f).with_context(|| format!("reading {}", f.display()))?;
            let certs = reqwest::Certificate::from_pem_bundle(&pem)
                .with_context(|| format!("{}: not a PEM certificate", f.display()))?;
            for cert in certs {
                b = b.add_root_certificate(cert);
            }
        }
        Ok(Self {
            base,
            project: c.project.trim().to_string(),
            key: c.key.trim().to_string(),
            http: b.build()?,
            limiter: Limiter::new(c.rate),
            retry_delay: c.retry_delay,
            limits: Limits::default(),
        })
    }

    /// The same reader with other limits.
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    fn url(&self, path: &str) -> anyhow::Result<url::Url> {
        Ok(self.base.join(path)?)
    }

    fn project_path(&self, rest: &str) -> String {
        format!("api/v1/{}/{rest}", Client::enc(&self.project))
    }

    async fn get(&self, url: url::Url, max: usize) -> anyhow::Result<(Response, Vec<u8>)> {
        let mut attempt = 0u32;
        loop {
            self.limiter.wait().await;
            let sent = self
                .http
                .get(url.clone())
                .bearer_auth(&self.key)
                .send()
                .await;
            let wait = match sent {
                Ok(r) if r.status().is_success() => {
                    let mut r = r;
                    if r.content_length().is_some_and(|n| n > max as u64) {
                        return Err(RpError::TooLarge(max).into());
                    }
                    let mut body = Vec::new();
                    while let Some(chunk) = r.chunk().await.context("reading the answer")? {
                        if body.len() + chunk.len() > max {
                            return Err(RpError::TooLarge(max).into());
                        }
                        body.extend_from_slice(&chunk);
                    }
                    return Ok((r, body));
                }
                Ok(r) => {
                    let st = r.status();
                    let retry_after = r
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .map(|s| Duration::from_secs(s.min(60)));
                    if st.is_redirection() {
                        bail!(
                            "{} answered {st}: the URL redirects; give the final URL as --rp-url",
                            url.path()
                        );
                    }
                    if !retryable(st) || attempt >= 5 {
                        let body = read_some(r, MAX_ERROR_BYTES).await;
                        let msg = serde_json::from_slice::<Value>(&body)
                            .ok()
                            .and_then(|v| v["message"].as_str().map(|m| super::shown(m, 300)))
                            .unwrap_or_default();
                        return Err(RpError::Status {
                            status: st,
                            path: url.path().to_string(),
                            message: msg,
                        }
                        .into());
                    }
                    retry_after.unwrap_or_else(|| self.backoff(attempt))
                }
                Err(e) => {
                    if attempt >= 5 {
                        return Err(anyhow!(e)).context("contacting ReportPortal");
                    }
                    self.backoff(attempt)
                }
            };
            attempt += 1;
            tokio::time::sleep(wait).await;
        }
    }

    fn backoff(&self, attempt: u32) -> Duration {
        (self.retry_delay * 2u32.saturating_pow(attempt)).min(Duration::from_secs(30))
    }

    async fn get_page(&self, path: &str) -> anyhow::Result<Page> {
        let (_, body) = self.get(self.url(path)?, MAX_JSON_BYTES).await?;
        let json =
            serde_json::from_slice(&body).context("ReportPortal answered with invalid JSON")?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hasher::write(&mut h, &body);
        Ok(Page {
            json,
            bytes: body.len(),
            hash: std::hash::Hasher::finish(&h),
        })
    }

    async fn get_json(&self, path: &str) -> anyhow::Result<Value> {
        Ok(self.get_page(path).await?.json)
    }

    /// `GET /api/info`: the version of the server.
    pub async fn version(&self) -> anyhow::Result<String> {
        let v = self.get_json("api/info").await?;
        Ok(v["build"]["version"]
            .as_str()
            .unwrap_or("unknown")
            .to_string())
    }

    /// The defect types of the project (empty when the project settings are not readable).
    pub async fn defect_types(&self) -> DefectTypes {
        let mut out = DefectTypes::new();
        let Ok(v) = self.get_json(&self.project_path("settings")).await else {
            return out;
        };
        if let Some(sub) = v["subTypes"].as_object() {
            for (group, types) in sub {
                for t in types.as_array().into_iter().flatten() {
                    if let Some(l) = t["locator"].as_str() {
                        let name = t["longName"].as_str().unwrap_or_default().to_string();
                        let r = t["typeRef"].as_str().unwrap_or(group).to_string();
                        out.insert(l.to_string(), (r, name));
                    }
                }
            }
        }
        out
    }

    /// One page of launches in ascending start order. `page` starts at 1; `None` ends the list.
    pub async fn launches_page(
        &self,
        page: usize,
        debug: bool,
        since_ms: Option<i64>,
        name: Option<&str>,
    ) -> anyhow::Result<Option<Vec<RpLaunch>>> {
        let mut q = format!("page.page={page}&page.size={PAGE_SIZE}&page.sort=startTime,ASC");
        if let Some(s) = since_ms {
            q.push_str(&format!("&filter.gte.startTime={s}"));
        }
        if let Some(n) = name {
            q.push_str(&format!("&filter.eq.name={}", Client::enc(n)));
        }
        let kind = if debug { "launch/mode" } else { "launch" };
        let v = self
            .get_json(&self.project_path(&format!("{kind}?{q}")))
            .await?;
        Ok(page_of(&v, page, launch_from))
    }

    /// One page of the items of a launch (all of them, steps included).
    async fn items_page(&self, launch_id: i64, page: usize) -> anyhow::Result<Page> {
        let q = format!(
            "filter.eq.launchId={launch_id}&page.page={page}&page.size={PAGE_SIZE}&page.sort=id,ASC"
        );
        self.get_page(&self.project_path(&format!("item?{q}")))
            .await
    }

    /// All items of a launch, within the limits. A page that brings no new item ends the list
    /// (a server that ignores the page number).
    pub async fn items(&self, launch_id: i64) -> anyhow::Result<Vec<RpItem>> {
        let lim = self.limits;
        let mut all: Vec<RpItem> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let (mut count, mut bytes) = (0usize, 0usize);
        for page in 1.. {
            if page > lim.max_pages {
                bail!(
                    "launch {launch_id}: more than {} pages of items",
                    lim.max_pages
                );
            }
            let p = self.items_page(launch_id, page).await?;
            bytes += p.bytes;
            if bytes > lim.max_item_bytes {
                bail!(
                    "launch {launch_id}: its items are larger than {} MiB; it is not imported",
                    lim.max_item_bytes >> 20
                );
            }
            let Some(items) = page_of(&p.json, page, item_from) else {
                break;
            };
            let mut fresh = 0;
            for i in items {
                if seen.insert(i.id) {
                    fresh += 1;
                    count += 1 + i.retries.len();
                    all.push(i);
                }
            }
            if count > lim.max_items {
                bail!(
                    "launch {launch_id}: more than {} items; it is not imported",
                    lim.max_items
                );
            }
            if fresh == 0 {
                break;
            }
        }
        Ok(all)
    }

    /// The log entries of up to `LOG_CHUNK` items, at least `level` (`None`: all). Entries of
    /// other items are dropped; the pages of a chunk are bounded in number and bytes, and a page
    /// equal to the previous one ends the list.
    pub async fn logs(
        &self,
        item_ids: &[i64],
        min_level: Option<&str>,
    ) -> anyhow::Result<Vec<RpLog>> {
        let lim = self.limits;
        let wanted: std::collections::HashSet<i64> = item_ids.iter().copied().collect();
        let ids = item_ids
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let mut out = Vec::new();
        let (mut bytes, mut last) = (0usize, None);
        let mut seen = std::collections::HashSet::new();
        for page in 1.. {
            if page > lim.max_pages {
                bail!("more than {} pages of logs", lim.max_pages);
            }
            let mut q = format!(
                "filter.in.item={ids}&page.page={page}&page.size={PAGE_SIZE}&page.sort=logTime,ASC"
            );
            if let Some(l) = min_level {
                q.push_str(&format!("&filter.gte.level={}", l.to_ascii_uppercase()));
            }
            let p = self
                .get_page(&self.project_path(&format!("log?{q}")))
                .await?;
            if last == Some(p.hash) {
                break;
            }
            last = Some(p.hash);
            bytes += p.bytes;
            if bytes > lim.max_log_bytes {
                bail!(
                    "the logs of {} items are larger than {} MiB; import fewer levels (--logs warn or error)",
                    item_ids.len(),
                    lim.max_log_bytes >> 20
                );
            }
            let Some(entries) = page_of(&p.json, page, log_from) else {
                break;
            };
            let before = seen.len();
            let mut anonymous = false;
            for l in entries {
                match l.id {
                    Some(id) if !seen.insert(id) => continue,
                    Some(_) => {}
                    None => anonymous = true,
                }
                if l.item_id.is_some_and(|i| wanted.contains(&i)) {
                    out.push(l);
                }
            }
            if seen.len() == before && !anonymous {
                break;
            }
        }
        Ok(out)
    }

    /// An attachment (`GET /api/v1/data/{project}/{id}`) up to `max` bytes; `None` when it is
    /// larger or gone.
    pub async fn file(&self, id: &str, max: u64) -> anyhow::Result<Option<RpFile>> {
        if id.is_empty() || id.len() > 512 || id.bytes().all(|b| b == b'.') {
            bail!("unusable attachment id");
        }
        let path = format!(
            "api/v1/data/{}/{}",
            Client::enc(&self.project),
            Client::enc(id)
        );
        match self.get(self.url(&path)?, max as usize).await {
            Ok((r, bytes)) => {
                let h = |n: &str| {
                    r.headers()
                        .get(n)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string)
                };
                let file_name = h("content-disposition").and_then(|d| {
                    d.split(';').find_map(|p| {
                        p.trim()
                            .strip_prefix("filename=")
                            .map(|f| f.trim_matches('"').to_string())
                    })
                });
                Ok(Some(RpFile {
                    bytes,
                    content_type: h("content-type"),
                    file_name,
                }))
            }
            Err(e) => match e.downcast_ref::<RpError>() {
                Some(RpError::TooLarge(_)) => Ok(None),
                Some(RpError::Status { status, .. }) if *status == StatusCode::NOT_FOUND => {
                    Ok(None)
                }
                _ => Err(e),
            },
        }
    }
}

/// Up to `cap` bytes of a body; the rest is not read.
async fn read_some(mut r: Response, cap: usize) -> Vec<u8> {
    let mut body = Vec::new();
    while body.len() < cap {
        match r.chunk().await {
            Ok(Some(c)) => body.extend_from_slice(&c[..c.len().min(cap - body.len())]),
            _ => break,
        }
    }
    body
}

fn retryable(s: StatusCode) -> bool {
    matches!(s.as_u16(), 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

/// The parsed content of a page, or `None` when it is the one after the last.
fn page_of<T>(v: &Value, page: usize, f: impl Fn(&Value) -> Option<T>) -> Option<Vec<T>> {
    let content = v["content"].as_array()?;
    if content.is_empty() {
        return None;
    }
    if let Some(total) = v["page"]["totalPages"].as_u64()
        && page as u64 > total
    {
        return None;
    }
    Some(content.iter().filter_map(f).collect())
}

/// The external ids of an issue as plain tuples (for tests and logs).
pub fn tickets(i: &Issue) -> Vec<&ExternalIssue> {
    i.external_system_issues.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shapes_are_read_leniently() {
        let l = launch_from(&json!({"id": 7, "uuid": "u1", "name": "n", "number": "3",
            "startTime": "2026-01-02T03:04:05.123456Z", "endTime": 1767323045123i64,
            "status": "FAILED", "mode": "DEBUG", "attributes": [{"key":"a","value":"b"},
            {"value":"tag"}]}))
        .unwrap();
        assert_eq!((l.id, l.number, l.debug), (7, 3, true));
        assert_eq!(l.start_ms, 1767323045123);
        assert_eq!(l.attributes.len(), 2);
        assert!(l.finished());
        let i = item_from(
            &json!({"id": 1, "uuid": "i", "type": "STEP", "hasStats": false,
            "parent": 9, "startTime": 5, "issue": {"issueType": "pb001", "comment": "c",
            "externalSystemIssues": [{"ticketId": "T-1", "url": "https://x.example.com/T-1"}]},
            "retries": [{"id": 0, "uuid": "r"}]}),
        )
        .unwrap();
        assert!(!i.has_stats);
        assert_eq!(i.parent, Some(9));
        assert_eq!(i.retries.len(), 1);
        assert_eq!(tickets(i.issue.as_ref().unwrap()).len(), 1);
        let g = log_from(
            &json!({"itemId": 4, "logTime": 10, "message": "m", "level": "ERROR",
            "binaryContent": {"id": "f1", "contentType": "image/png"}}),
        )
        .unwrap();
        assert_eq!((g.item_id, g.level), (Some(4), "error"));
        assert_eq!(g.file.unwrap().id, "f1");
    }

    #[test]
    fn refuses_plain_http_credentials_in_urls_and_missing_keys() {
        let conn = |url: &str, key: &str| RpConn {
            url: url.into(),
            project: "p".into(),
            key: key.into(),
            ca_file: None,
            allow_http: false,
            rate: 10.0,
            retry_delay: Duration::from_millis(1),
        };
        assert!(RpReader::new(&conn("http://rp.example.com", "k")).is_err());
        assert!(RpReader::new(&conn("http://127.0.0.1:8080", "k")).is_ok());
        assert!(RpReader::new(&conn("https://u:p@rp.example.com", "k")).is_err());
        assert!(RpReader::new(&conn("https://rp.example.com", " ")).is_err());
        assert!(RpReader::new(&conn("https://rp.example.com/sub", "k")).is_ok());
        assert!(RpReader::new(&conn("https://rp.example.com/?access_token=k", "k")).is_err());
        assert!(RpReader::new(&conn("https://rp.example.com/#k", "k")).is_err());
        assert!(RpReader::new(&conn("https://rp.example.com", "k\r\nX-Evil: 1")).is_err());
        assert!(RpReader::new(&conn("https://rp.example.com", "k k")).is_err());
    }
}
