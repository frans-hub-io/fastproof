//! Replays a ReportPortal launch through `/rp` of FastProof: the same calls an agent makes
//! (launch, items, logs, finish) with the original uuids and times, so the mapping is the one
//! live reports use and a repeated replay changes nothing.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{Context, bail};
use bytes::Bytes;
use fastproof_rp::ids;
use fastproof_rp::model::{Issue, ItemAttribute};
use reqwest::{Method, RequestBuilder};
use serde_json::{Value, json};

use super::super::client::Client;
use super::reader::{DefectTypes, LOG_CHUNK, RpItem, RpLaunch, RpLog, RpReader};

/// Which attachments are copied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attachments {
    None,
    Failed,
    All,
}

/// What a replay or a survey takes.
#[derive(Clone, Debug)]
pub struct Options {
    /// The lowest log level taken (`None`: no logs).
    pub min_level: Option<&'static str>,
    pub attachments: Attachments,
    pub max_attachment: u64,
}

/// What a launch brought (or, in a dry run, would bring).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub items: u64,
    pub logs: u64,
    pub log_bytes: u64,
    pub files: u64,
    pub file_bytes: u64,
    pub skipped_files: u64,
    pub warnings: Vec<String>,
}

impl Counts {
    pub fn add(&mut self, o: &Counts) {
        self.items += o.items;
        self.logs += o.logs;
        self.log_bytes += o.log_bytes;
        self.files += o.files;
        self.file_bytes += o.file_bytes;
        self.skipped_files += o.skipped_files;
        self.warnings.extend(o.warnings.iter().cloned());
    }
}

fn rank(level: &str) -> u8 {
    match level {
        "trace" => 0,
        "debug" => 1,
        "warn" => 3,
        "error" => 4,
        _ => 2,
    }
}

/// The level a `--logs` value names.
pub fn min_level(logs: &str) -> anyhow::Result<Option<&'static str>> {
    Ok(match logs {
        "none" => None,
        "error" => Some("error"),
        "warn" => Some("warn"),
        "all" => Some("trace"),
        _ => bail!("--logs must be none, error, warn or all"),
    })
}

/// One replayed attempt of an item.
#[derive(Debug)]
pub struct Flat {
    pub item: RpItem,
    pub parent_uuid: Option<String>,
    pub retry: bool,
    pub retry_of: Option<String>,
    /// The test result it belongs to failed (for `--attachments failed`).
    pub failing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ev {
    Start(usize),
    Finish(usize),
}

/// The calls of a launch in the order an agent makes them: a parent starts before its
/// children and finishes after them, siblings in start order, earlier attempts of a test
/// (`retries`) as finished leaves just before it.
pub fn plan(items: Vec<RpItem>) -> (Vec<Flat>, Vec<Ev>) {
    let uuid_of: HashMap<i64, String> = items.iter().map(|i| (i.id, i.uuid.clone())).collect();
    let by_id: HashMap<i64, usize> = items.iter().enumerate().map(|(n, i)| (i.id, n)).collect();
    let mut children: HashMap<Option<i64>, Vec<usize>> = HashMap::new();
    for (n, i) in items.iter().enumerate() {
        let parent = i.parent.filter(|p| by_id.contains_key(p) && *p != i.id);
        children.entry(parent).or_default().push(n);
    }
    for v in children.values_mut() {
        v.sort_by_key(|&n| (items[n].start_ms, items[n].id));
    }
    let failing = |n: usize| -> bool {
        let mut cur = Some(n);
        let mut guard = 0;
        while let Some(c) = cur {
            let i = &items[c];
            if i.has_stats {
                return i.status.eq_ignore_ascii_case("FAILED");
            }
            cur = i.parent.and_then(|p| by_id.get(&p).copied());
            guard += 1;
            if guard > 10_000 {
                break;
            }
        }
        false
    };
    let mut flat: Vec<Flat> = Vec::with_capacity(items.len());
    let mut evs = Vec::with_capacity(items.len() * 2);
    let roots = children.get(&None).cloned().unwrap_or_default();
    let mut visit_order: Vec<(usize, Option<String>)> = Vec::new();
    for r in roots.iter().rev() {
        visit_order.push((*r, None));
    }
    let mut open: Vec<usize> = Vec::new();
    while let Some((n, parent_uuid)) = visit_order.pop() {
        if n == usize::MAX {
            let f = open.pop().expect("a finish for every start");
            evs.push(Ev::Finish(f));
            continue;
        }
        let it = &items[n];
        let fail = failing(n);
        let mut first: Option<String> = None;
        let mut retries = it.retries.clone();
        retries.sort_by_key(|r| (r.start_ms, r.id));
        for r in retries {
            let at = flat.len();
            flat.push(Flat {
                retry: first.is_some(),
                retry_of: first.clone(),
                failing: r.status.eq_ignore_ascii_case("FAILED"),
                parent_uuid: parent_uuid.clone(),
                item: RpItem {
                    parent: it.parent,
                    retries: Vec::new(),
                    ..r
                },
            });
            first.get_or_insert_with(|| flat[at].item.uuid.clone());
            evs.push(Ev::Start(at));
            evs.push(Ev::Finish(at));
        }
        let at = flat.len();
        flat.push(Flat {
            item: RpItem {
                retries: Vec::new(),
                ..it.clone()
            },
            parent_uuid: parent_uuid.clone(),
            retry: first.is_some(),
            retry_of: first,
            failing: fail,
        });
        evs.push(Ev::Start(at));
        open.push(at);
        visit_order.push((usize::MAX, None));
        let uuid = uuid_of.get(&it.id).cloned();
        for c in children.get(&Some(it.id)).into_iter().flatten().rev() {
            visit_order.push((*c, uuid.clone()));
        }
    }
    (flat, evs)
}

/// Loads the log lines of the items in the order they are replayed, a chunk of items per
/// request, and hands them out one item at a time.
struct LogFeed<'a> {
    reader: &'a RpReader,
    order: Vec<i64>,
    index: HashMap<i64, usize>,
    loaded: usize,
    cache: HashMap<i64, Vec<RpLog>>,
    min: Option<&'static str>,
}

impl<'a> LogFeed<'a> {
    fn new(reader: &'a RpReader, flat: &[Flat], evs: &[Ev], min: Option<&'static str>) -> Self {
        let mut order = Vec::new();
        let mut seen = HashSet::new();
        for e in evs {
            if let Ev::Start(n) = e
                && seen.insert(flat[*n].item.id)
            {
                order.push(flat[*n].item.id);
            }
        }
        let index = order.iter().enumerate().map(|(n, i)| (*i, n)).collect();
        Self {
            reader,
            order,
            index,
            loaded: 0,
            cache: HashMap::new(),
            min,
        }
    }

    async fn take(&mut self, id: i64) -> anyhow::Result<Vec<RpLog>> {
        let Some(min) = self.min else {
            return Ok(Vec::new());
        };
        let Some(&at) = self.index.get(&id) else {
            return Ok(Vec::new());
        };
        while at >= self.loaded {
            let end = (self.loaded + LOG_CHUNK).min(self.order.len());
            let ids = &self.order[self.loaded..end];
            let logs = self
                .reader
                .logs(ids, Some(min))
                .await
                .context("reading logs from ReportPortal")?;
            for l in logs {
                if let Some(i) = l.item_id
                    && rank(l.level) >= rank(min)
                {
                    self.cache.entry(i).or_default().push(l);
                }
            }
            self.loaded = end;
        }
        let mut v = self.cache.remove(&id).unwrap_or_default();
        v.sort_by_key(|l| l.time_ms);
        Ok(v)
    }
}

/// Counts what a replay would take, and writes nothing.
pub async fn survey_launch(
    reader: &RpReader,
    l: &RpLaunch,
    opts: &Options,
) -> anyhow::Result<Counts> {
    let items = reader.items(l.id).await?;
    let (flat, evs) = plan(items);
    let mut feed = LogFeed::new(reader, &flat, &evs, opts.min_level);
    let mut c = Counts {
        items: flat.len() as u64,
        ..Default::default()
    };
    for e in &evs {
        if let Ev::Start(n) = e {
            for log in feed.take(flat[*n].item.id).await? {
                c.logs += 1;
                c.log_bytes += log.message.len() as u64;
                if log.file.is_some() && wants_file(opts, flat[*n].failing) {
                    c.files += 1;
                }
            }
        }
    }
    Ok(c)
}

fn wants_file(o: &Options, failing: bool) -> bool {
    match o.attachments {
        Attachments::None => false,
        Attachments::Failed => failing,
        Attachments::All => true,
    }
}

/// The writer side: calls of `/rp` on a FastProof server, with retries.
pub struct Writer {
    client: Client,
    project: String,
    retry_delay: Duration,
}

impl Writer {
    pub fn new(client: Client, project: &str, retry_delay: Duration) -> Self {
        Self {
            client,
            project: project.to_string(),
            retry_delay,
        }
    }

    fn path(&self, rest: &str) -> String {
        format!("rp/api/v2/{}/{rest}", Client::enc(&self.project))
    }

    async fn send(
        &self,
        make: impl Fn() -> anyhow::Result<RequestBuilder>,
    ) -> anyhow::Result<Value> {
        let mut attempt = 0u32;
        loop {
            let wait = match make()?.send().await {
                Ok(r) if r.status().is_success() => {
                    let b = r.bytes().await.unwrap_or_default();
                    return Ok(serde_json::from_slice(&b).unwrap_or(Value::Null));
                }
                Ok(r) => {
                    let st = r.status();
                    let retry_after = r
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(|s| Duration::from_secs(s.min(30)));
                    if !matches!(st.as_u16(), 429 | 502 | 503 | 504) || attempt >= 6 {
                        let body: Value = r.json().await.unwrap_or_default();
                        bail!(
                            "FastProof answered {} {}: {}",
                            st.as_u16(),
                            body["errorCode"],
                            body["message"].as_str().unwrap_or("request failed")
                        );
                    }
                    retry_after.unwrap_or_else(|| self.backoff(attempt))
                }
                Err(e) => {
                    if attempt >= 6 {
                        return Err(anyhow::Error::new(e))
                            .context("contacting the FastProof server");
                    }
                    self.backoff(attempt)
                }
            };
            attempt += 1;
            tokio::time::sleep(wait).await;
        }
    }

    fn backoff(&self, attempt: u32) -> Duration {
        (self.retry_delay * 2u32.saturating_pow(attempt)).min(Duration::from_secs(20))
    }

    async fn json(&self, method: Method, rest: &str, body: Value) -> anyhow::Result<Value> {
        let path = self.path(rest);
        self.send(|| Ok(self.client.request(method.clone(), &path)?.json(&body)))
            .await
    }

    async fn multipart(
        &self,
        rest: &str,
        entries: &Value,
        files: &[(String, String, Bytes)],
    ) -> anyhow::Result<()> {
        let (ct, body) = multipart_body(entries, files);
        let path = self.path(rest);
        self.send(|| {
            Ok(self
                .client
                .request(Method::POST, &path)?
                .header("content-type", ct.clone())
                .body(body.clone()))
        })
        .await?;
        Ok(())
    }

    /// Waits until the server has finished the launch (settling takes a moment).
    async fn wait_finished(&self, uuid: &str, within: Duration) -> bool {
        let path = format!(
            "rp/api/v1/{}/launch/uuid/{}",
            Client::enc(&self.project),
            Client::enc(uuid)
        );
        let deadline = std::time::Instant::now() + within;
        loop {
            let r = self.send(|| self.client.request(Method::GET, &path)).await;
            if let Ok(v) = r
                && v["status"] != "IN_PROGRESS"
            {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
}

/// A multipart body: `json_request_part` holds the entries, the files follow.
pub fn multipart_body(entries: &Value, files: &[(String, String, Bytes)]) -> (String, Bytes) {
    let boundary = format!("fastproof-{:016x}", rand::random::<u64>());
    let mut b = Vec::new();
    b.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"json_request_part\"\r\nContent-Type: application/json\r\n\r\n").as_bytes());
    b.extend_from_slice(entries.to_string().as_bytes());
    b.extend_from_slice(b"\r\n");
    for (name, ct, data) in files {
        b.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {ct}\r\n\r\n")
                .as_bytes(),
        );
        b.extend_from_slice(data);
        b.extend_from_slice(b"\r\n");
    }
    b.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (
        format!("multipart/form-data; boundary={boundary}"),
        Bytes::from(b),
    )
}

/// A name that is safe as a file name and a header value.
pub fn safe_name(raw: &str, fallback: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let s: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect();
    let s = s.trim_matches('.').to_string();
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

fn extension(content_type: &str) -> &'static str {
    match content_type.split(';').next().unwrap_or("").trim() {
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "text/plain" => ".txt",
        "text/html" => ".html",
        "application/json" => ".json",
        "application/zip" => ".zip",
        "application/pdf" => ".pdf",
        "video/webm" => ".webm",
        "video/mp4" => ".mp4",
        _ => "",
    }
}

/// An issue the FastProof server understands: custom defect types of ReportPortal become the
/// built-in type of their group, and their name leads the comment.
pub fn map_issue(issue: &Issue, types: &DefectTypes) -> Issue {
    let mut out = issue.clone();
    let Some(t) = issue.issue_type.as_deref().map(str::trim) else {
        return out;
    };
    if t.eq_ignore_ascii_case("NOT_ISSUE") || t.eq_ignore_ascii_case("not_issue_flag") {
        return out;
    }
    let lower = t.to_ascii_lowercase();
    if ids::GROUPS
        .iter()
        .any(|g| lower == g.locator || t.eq_ignore_ascii_case(g.type_ref))
    {
        return out;
    }
    let (group, name) = match types.get(t) {
        Some((type_ref, name)) => (
            ids::GROUPS.iter().find(|g| g.type_ref == type_ref),
            name.as_str(),
        ),
        None => (
            ids::GROUPS
                .iter()
                .find(|g| lower.starts_with(&format!("{}_", g.prefix))),
            "",
        ),
    };
    let g = group.unwrap_or(&ids::GROUPS[4]);
    out.issue_type = Some(g.locator.to_string());
    if !name.is_empty() {
        let c = issue.comment.as_deref().unwrap_or_default();
        out.comment = Some(if c.is_empty() {
            format!("[{name}]")
        } else {
            format!("[{name}] {c}")
        });
    }
    out
}

fn attrs(a: &[ItemAttribute]) -> Value {
    serde_json::to_value(a).unwrap_or(Value::Null)
}

/// Replays one launch. The launch is complete when this returns `Ok`.
pub async fn replay_launch(
    reader: &RpReader,
    w: &Writer,
    types: &DefectTypes,
    l: &RpLaunch,
    opts: &Options,
) -> anyhow::Result<Counts> {
    let items = reader.items(l.id).await?;
    let (flat, evs) = plan(items);
    let mut feed = LogFeed::new(reader, &flat, &evs, opts.min_level);
    let mut c = Counts::default();
    let end = l.end_ms.unwrap_or(l.start_ms);

    let mut attributes = l.attributes.clone();
    attributes.push(ItemAttribute {
        key: Some("imported".into()),
        value: Some("reportportal".into()),
        system: false,
    });
    w.json(
        Method::POST,
        "launch",
        json!({"name": l.name, "uuid": l.uuid, "startTime": l.start_ms,
            "description": l.description, "attributes": attrs(&attributes),
            "mode": if l.debug { "DEBUG" } else { "DEFAULT" }}),
    )
    .await
    .context("starting the launch")?;

    let mut files_seen: HashMap<String, Option<(String, String, Bytes)>> = HashMap::new();
    for e in &evs {
        match *e {
            Ev::Start(n) => {
                let f = &flat[n];
                let it = &f.item;
                let rest = match &f.parent_uuid {
                    Some(p) => format!("item/{}", Client::enc(p)),
                    None => "item".to_string(),
                };
                w.json(
                    Method::POST,
                    &rest,
                    json!({"name": it.name, "type": it.kind, "startTime": it.start_ms,
                        "launchUuid": l.uuid, "uuid": it.uuid, "description": it.description,
                        "attributes": attrs(&it.attributes), "parameters": it.parameters,
                        "codeRef": it.code_ref, "testCaseId": it.test_case_id,
                        "hasStats": it.has_stats, "retry": f.retry, "retryOf": f.retry_of}),
                )
                .await
                .with_context(|| format!("starting item {}", it.uuid))?;
                c.items += 1;
                let logs = feed.take(it.id).await?;
                send_logs(reader, w, l, f, logs, opts, &mut files_seen, &mut c).await?;
            }
            Ev::Finish(n) => {
                let it = &flat[n].item;
                let status = match it.status.trim().to_ascii_uppercase().as_str() {
                    "" => Value::Null,
                    "IN_PROGRESS" => json!("INTERRUPTED"),
                    s => json!(s),
                };
                let mut body = json!({"endTime": it.end_ms.unwrap_or(end), "status": status,
                    "launchUuid": l.uuid});
                if let Some(i) = &it.issue {
                    body["issue"] = serde_json::to_value(map_issue(i, types))?;
                }
                w.json(
                    Method::PUT,
                    &format!("item/{}", Client::enc(&it.uuid)),
                    body,
                )
                .await
                .with_context(|| format!("finishing item {}", it.uuid))?;
            }
        }
    }
    w.json(
        Method::PUT,
        &format!("launch/{}/finish", Client::enc(&l.uuid)),
        json!({"endTime": end, "status": if l.status.is_empty() { Value::Null } else { json!(l.status) }}),
    )
    .await
    .context("finishing the launch")?;
    if !w.wait_finished(&l.uuid, Duration::from_secs(180)).await {
        c.warnings.push(format!(
            "launch {}: FastProof was still settling it after 180 s; it finishes on its own",
            l.uuid
        ));
    }
    Ok(c)
}

async fn flush_text(w: &Writer, text: &mut Vec<Value>, bytes: &mut usize) -> anyhow::Result<()> {
    let batch = std::mem::take(text);
    *bytes = 0;
    if batch.is_empty() {
        return Ok(());
    }
    w.json(Method::POST, "log", Value::Array(batch))
        .await
        .map(|_| ())
}

#[allow(clippy::too_many_arguments)]
async fn send_logs(
    reader: &RpReader,
    w: &Writer,
    l: &RpLaunch,
    f: &Flat,
    logs: Vec<RpLog>,
    opts: &Options,
    files_seen: &mut HashMap<String, Option<(String, String, Bytes)>>,
    c: &mut Counts,
) -> anyhow::Result<()> {
    let mut text: Vec<Value> = Vec::new();
    let mut text_bytes = 0usize;
    for log in logs {
        let mut entry = json!({"launchUuid": l.uuid, "itemUuid": f.item.uuid,
            "time": log.time_ms.max(0), "message": log.message,
            "level": log.level.to_ascii_uppercase()});
        if let Some(u) = log.uuid.as_deref().filter(|u| ids::valid_uuid(u)) {
            entry["uuid"] = json!(u);
        }
        c.logs += 1;
        c.log_bytes += log.message.len() as u64;
        let file = match &log.file {
            Some(r) if wants_file(opts, f.failing) => {
                if !files_seen.contains_key(&r.id) {
                    let got = match reader.file(&r.id, opts.max_attachment).await {
                        Ok(Some(file)) => {
                            let ct = file
                                .content_type
                                .or_else(|| Some(r.content_type.clone()).filter(|t| !t.is_empty()))
                                .unwrap_or_else(|| "application/octet-stream".into());
                            let name = safe_name(
                                file.file_name.as_deref().unwrap_or_default(),
                                &format!(
                                    "attachment-{}{}",
                                    safe_name(&r.id, "file"),
                                    extension(&ct)
                                ),
                            );
                            Some((name, ct, Bytes::from(file.bytes)))
                        }
                        Ok(None) => None,
                        Err(e) => {
                            c.warnings.push(format!(
                                "item {}: attachment {} not copied: {e:#}",
                                f.item.uuid, r.id
                            ));
                            None
                        }
                    };
                    if files_seen.len() > 8 {
                        files_seen.clear();
                    }
                    files_seen.insert(r.id.clone(), got);
                }
                files_seen.get(&r.id).cloned().flatten()
            }
            _ => None,
        };
        match file {
            Some((name, ct, bytes)) => {
                flush_text(w, &mut text, &mut text_bytes).await?;
                entry["file"] = json!({"name": name});
                c.files += 1;
                c.file_bytes += bytes.len() as u64;
                w.multipart("log", &Value::Array(vec![entry]), &[(name, ct, bytes)])
                    .await
                    .with_context(|| format!("sending a log file of item {}", f.item.uuid))?;
            }
            None => {
                if log.file.is_some() && wants_file(opts, f.failing) {
                    c.skipped_files += 1;
                }
                text_bytes += log.message.len() + 200;
                text.push(entry);
                if text.len() >= 500 || text_bytes >= 400 * 1024 {
                    flush_text(w, &mut text, &mut text_bytes).await?;
                }
            }
        }
    }
    flush_text(w, &mut text, &mut text_bytes).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(id: i64, uuid: &str, parent: Option<i64>, start: i64) -> RpItem {
        RpItem {
            id,
            uuid: uuid.into(),
            name: uuid.into(),
            kind: "STEP".into(),
            start_ms: start,
            parent,
            has_stats: true,
            ..Default::default()
        }
    }

    fn render(items: Vec<RpItem>) -> Vec<String> {
        let (flat, evs) = plan(items);
        evs.iter()
            .map(|e| match e {
                Ev::Start(n) => format!("+{}", flat[*n].item.uuid),
                Ev::Finish(n) => format!("-{}", flat[*n].item.uuid),
            })
            .collect()
    }

    #[test]
    fn parents_start_first_and_finish_last_siblings_by_start() {
        let items = vec![
            it(3, "b", Some(1), 20),
            it(1, "suite", None, 5),
            it(2, "a", Some(1), 10),
            it(4, "a1", Some(2), 11),
            it(5, "other", None, 30),
        ];
        assert_eq!(
            render(items),
            [
                "+suite", "+a", "+a1", "-a1", "-a", "+b", "-b", "-suite", "+other", "-other"
            ]
        );
    }

    #[test]
    fn retries_come_first_as_attempts_of_the_first() {
        let mut main = it(2, "t3", Some(1), 30);
        main.retries = vec![it(9, "t2", None, 20), it(8, "t1", None, 10)];
        let (flat, evs) = plan(vec![it(1, "s", None, 1), main]);
        let starts: Vec<(&str, bool, Option<&str>)> = evs
            .iter()
            .filter_map(|e| match e {
                Ev::Start(n) => Some((
                    flat[*n].item.uuid.as_str(),
                    flat[*n].retry,
                    flat[*n].retry_of.as_deref(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            starts,
            [
                ("s", false, None),
                ("t1", false, None),
                ("t2", true, Some("t1")),
                ("t3", true, Some("t1"))
            ]
        );
        assert_eq!(flat[1].parent_uuid.as_deref(), Some("s"));
    }

    #[test]
    fn an_item_with_a_missing_parent_is_a_root_and_steps_inherit_failure() {
        let mut test = it(2, "t", None, 1);
        test.status = "FAILED".into();
        let mut step = it(3, "st", Some(2), 2);
        step.has_stats = false;
        step.status = "PASSED".into();
        let orphan = it(4, "o", Some(99), 3);
        let (flat, _) = plan(vec![test, step, orphan]);
        assert!(flat[0].failing && flat[1].failing && !flat[2].failing);
        assert_eq!(flat[2].parent_uuid, None);
    }

    #[test]
    fn custom_defect_types_become_their_group() {
        let mut types = DefectTypes::new();
        types.insert(
            "pb_ab12".into(),
            ("PRODUCT_BUG".into(), "Flaky infra".into()),
        );
        let i = Issue {
            issue_type: Some("pb_ab12".into()),
            comment: Some("see log".into()),
            ..Default::default()
        };
        let m = map_issue(&i, &types);
        assert_eq!(m.issue_type.as_deref(), Some("pb001"));
        assert_eq!(m.comment.as_deref(), Some("[Flaky infra] see log"));
        let m = map_issue(
            &Issue {
                issue_type: Some("ab_zz".into()),
                ..Default::default()
            },
            &types,
        );
        assert_eq!(m.issue_type.as_deref(), Some("ab001"));
        let keep = map_issue(
            &Issue {
                issue_type: Some("si001".into()),
                ..Default::default()
            },
            &types,
        );
        assert_eq!(keep.issue_type.as_deref(), Some("si001"));
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(safe_name("../../etc/pa ss\"wd.png", "x"), "pa_ss_wd.png");
        assert_eq!(safe_name("", "attachment-1.png"), "attachment-1.png");
        assert_eq!(safe_name("..", "f"), "f");
    }
}
