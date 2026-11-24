//! Log lines and attachments of the ReportPortal endpoint (design §7).
//!
//! Lines go to the native logs under the client id of the result they belong to
//! (`rp:<uuid>`, with the step path), or under `rp-container:<uuid>` and `rp-launch`. A
//! per-replica writer batches the lines of all requests: it takes what has arrived while the
//! previous batch was written (at most [`BATCH_LINES`]), reserves sequence numbers with one
//! update per project, writes, and only then answers the requests. Files of a multipart
//! batch are spooled, stored content-addressed like native attachments and linked to the
//! result or step whose line carries them.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use fastproof_blob::BlobKey;
use fastproof_rp::assemble::{Attached, ErrorText};
use fastproof_rp::classify::Role;
use fastproof_rp::model::SaveLog;
use fastproof_rp::multipart::{self, Reader};
use fastproof_rp::time::{bounded_or_now, now_ms};
use fastproof_rp::{clean_text, ids};
use fastproof_store::Project;
use fastproof_store::attachments::NewAttachment;
use fastproof_store::logs::{LogLevel, LogLine, LogStream};
use franshub_auth::Auth;
use franshub_http::ApiError;
use futures::StreamExt;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    LaunchInfo, MAX_BATCH_ENTRIES, MAX_JSON_PART_BYTES, MAX_PART_HEADER_BYTES, MAX_PARTS, RpError,
    RpResult, json, parse, parse_bytes, version,
};
use crate::api::attachments::clean_content_type;
use crate::api::uploads::clean_file_name;
use crate::state::AppState;

/// Lines the writer handles in one batch, at most.
pub const BATCH_LINES: usize = 1000;

/// Which counter numbers a line.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SeqOn {
    Item(String),
    Launch(String),
}

/// A line ready to be numbered and written.
#[derive(Clone, Debug)]
pub struct Line {
    pub project_id: Uuid,
    pub project_key: String,
    pub launch_id: Uuid,
    pub seq_on: SeqOn,
    pub log_key: String,
    pub step: Option<String>,
    pub level: LogLevel,
    pub ts: i64,
    pub message: String,
    /// The result or fixture whose error text an ERROR line sets and whose settle time any
    /// line moves.
    pub error_uuid: Option<String>,
}

/// Lines of one request and where to answer.
pub struct LogJob {
    pub lines: Vec<Line>,
    pub reply: oneshot::Sender<Result<(), RpError>>,
}

/// Hands lines to the writer and waits until they are stored.
pub async fn submit(s: &AppState, lines: Vec<Line>) -> RpResult<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let (tx, rx) = oneshot::channel();
    let busy = || {
        RpError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "the log writer is not available; retry later",
        )
    };
    s.rp.logs
        .send(LogJob { lines, reply: tx })
        .await
        .map_err(|_| busy())?;
    rx.await.map_err(|_| busy())?
}

/// The writer of the replica's log lines.
pub async fn writer(s: AppState, mut rx: mpsc::Receiver<LogJob>, stop: CancellationToken) {
    loop {
        let first = tokio::select! {
            _ = stop.cancelled() => return,
            j = rx.recv() => match j {
                Some(j) => j,
                None => return,
            },
        };
        let mut n = first.lines.len();
        let mut jobs = vec![first];
        while n < BATCH_LINES {
            match rx.try_recv() {
                Ok(j) => {
                    n += j.lines.len();
                    jobs.push(j);
                }
                Err(_) => break,
            }
        }
        let outcome = write(&s, &jobs).await;
        for (i, j) in jobs.into_iter().enumerate() {
            let r = match &outcome {
                Ok(refused) => match refused.get(&i) {
                    Some(max) => Err(RpError::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        format!(
                            "the launch exceeds FASTPROOF_LOG_MAX_BYTES_PER_LAUNCH ({max} bytes of log lines)"
                        ),
                    )),
                    None => Ok(()),
                },
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "writing ReportPortal log lines failed");
                    Err(RpError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "writing log lines failed; retry later",
                    ))
                }
            };
            let _ = j.reply.send(r);
        }
    }
}

/// Writes the lines of a batch; returns the jobs refused by the per-launch byte budget.
async fn write(s: &AppState, jobs: &[LogJob]) -> anyhow::Result<HashMap<usize, i64>> {
    let limits = s.cfg.log_limits;
    let mut bytes: BTreeMap<Uuid, i64> = BTreeMap::new();
    for j in jobs {
        for l in &j.lines {
            *bytes.entry(l.launch_id).or_default() +=
                (l.message.len() + l.step.as_ref().map_or(0, |x| x.len()) + l.log_key.len()) as i64;
        }
    }
    let mut refused_launches = Vec::new();
    for (launch, b) in &bytes {
        if !s
            .store
            .charge_log_bytes(*launch, *b, limits.max_bytes_per_launch)
            .await?
        {
            refused_launches.push(*launch);
        }
    }
    let mut refused = HashMap::new();
    let mut lines: Vec<&Line> = Vec::new();
    for (i, j) in jobs.iter().enumerate() {
        if j.lines
            .iter()
            .any(|l| refused_launches.contains(&l.launch_id))
        {
            refused.insert(i, limits.max_bytes_per_launch);
            continue;
        }
        lines.extend(j.lines.iter());
    }
    let mut by_project: BTreeMap<Uuid, Vec<&Line>> = BTreeMap::new();
    for l in lines {
        by_project.entry(l.project_id).or_default().push(l);
    }
    let settle = s.cfg.rp.settle.as_millis() as i64;
    let mut out: BTreeMap<Uuid, (Uuid, String, Vec<LogLine>)> = BTreeMap::new();
    for (project_id, lines) in by_project {
        let mut items: BTreeMap<String, i64> = BTreeMap::new();
        let mut launches: BTreeMap<String, i64> = BTreeMap::new();
        let mut notes: BTreeMap<String, Option<serde_json::Value>> = BTreeMap::new();
        for l in &lines {
            match &l.seq_on {
                SeqOn::Item(u) => *items.entry(u.clone()).or_default() += 1,
                SeqOn::Launch(u) => *launches.entry(u.clone()).or_default() += 1,
            }
            if let Some(e) = &l.error_uuid {
                let note = notes.entry(e.clone()).or_default();
                if l.level == LogLevel::Error {
                    *note = Some(serde_json::to_value(ErrorText::of(&l.message))?);
                }
            }
        }
        let wants: Vec<(String, i64)> = items.into_iter().collect();
        let mut next = s.store.rp_reserve_seqs(project_id, &wants).await?;
        let mut next_launch: HashMap<String, i64> = HashMap::new();
        for (u, n) in launches {
            if let Some(first) = s.store.rp_reserve_launch_seq(project_id, &u, n).await? {
                next_launch.insert(u, first);
            }
        }
        let notes: Vec<(String, Option<serde_json::Value>)> = notes.into_iter().collect();
        let late = s.store.rp_note_lines(project_id, &notes, settle).await?;
        if !late.is_empty() {
            metrics::counter!("fastproof_rp_late_total", "kind" => "log")
                .increment(late.len() as u64);
            s.store.rp_hold(project_id, &late).await?;
        }
        for l in lines {
            let counter = match &l.seq_on {
                SeqOn::Item(u) => next.get_mut(u),
                SeqOn::Launch(u) => next_launch.get_mut(u),
            };
            let Some(seq) = counter else {
                continue;
            };
            let mut line = LogLine {
                result_id: l.log_key.clone(),
                attempt: 0,
                seq: (*seq).max(0) as u64,
                ts: l.ts,
                level: l.level,
                stream: LogStream::Log,
                message: l.message.clone(),
                step: l.step.clone(),
            };
            *seq += 1;
            if crate::api::logs::validate(&mut line, limits.max_line_bytes).is_err() {
                continue;
            }
            out.entry(l.launch_id)
                .or_insert_with(|| (project_id, l.project_key.clone(), Vec::new()))
                .2
                .push(line);
        }
    }
    for (launch_id, (project_id, key, lines)) in out {
        let n = lines.len() as u64;
        if s.kafka_gateway() {
            crate::stream::produce_logs(s, &key, launch_id, lines)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        } else {
            s.store
                .write_log_lines(project_id, &key, launch_id, lines)
                .await?;
        }
        metrics::counter!("fastproof_log_lines_received_total").increment(n);
    }
    Ok(refused)
}

/// Where a file of a line goes: the item that gets it and the result or fixture it belongs
/// to (a late file of an emitted one is dropped).
struct Target {
    item: String,
    owner: Option<String>,
}

/// A line of a `SaveLogRQ`, after checking the caller may write into its launch.
async fn prepare(
    s: &AppState,
    auth: &Auth,
    p: &Project,
    rq: &SaveLog,
    file: Option<&str>,
    checked: &mut HashMap<String, LaunchInfo>,
) -> Result<(Line, Option<Target>), RpError> {
    let item = rq
        .item_uuid
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty());
    let (info, seq_on, log_key, step, error_uuid, target) = match item {
        Some(u) => {
            let route = s
                .rp_route(p.id, u)
                .await?
                .ok_or_else(|| RpError::not_found("test item"))?;
            let info = match checked.get(&route.rp_launch) {
                Some(i) => i.clone(),
                None => s.rp_launch_info(p, &route.rp_launch).await?,
            };
            let r = &route.parent;
            let seq_on = match &r.seq_uuid {
                Some(x) => SeqOn::Item(x.clone()),
                None => SeqOn::Launch(route.rp_launch.clone()),
            };
            let target = match r.role {
                Role::Result | Role::Fixture => Some(Target {
                    item: r.uuid.clone(),
                    owner: Some(r.uuid.clone()),
                }),
                Role::Step if r.error_uuid.is_some() => Some(Target {
                    item: r.uuid.clone(),
                    owner: r.error_uuid.clone(),
                }),
                _ => None,
            };
            (
                info,
                seq_on,
                r.log_key.clone(),
                r.log_step.clone(),
                r.error_uuid.clone(),
                target,
            )
        }
        None => {
            let lu = rq
                .launch_uuid
                .as_deref()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .ok_or_else(|| RpError::bad_request("launchUuid or itemUuid is required"))?;
            let info = match checked.get(lu) {
                Some(i) => i.clone(),
                None => s.rp_launch_info(p, lu).await?,
            };
            let uuid = info.uuid.clone();
            (
                info,
                SeqOn::Launch(uuid),
                fastproof_rp::classify::LAUNCH_LOG_KEY.to_string(),
                None,
                None,
                None,
            )
        }
    };
    if !checked.contains_key(&info.uuid) {
        s.rp_contributor(auth, p, &info).await?;
        s.rp_count_items(p, &info, 0).await?;
        checked.insert(info.uuid.clone(), info.clone());
    }
    let mut message = clean_text(rq.message.as_deref().unwrap_or_default());
    if let Some(f) = file {
        message.push_str(&format!(" [attachment: {f}]"));
    }
    let level =
        LogLevel::parse(fastproof_rp::status::log_level(&rq.level)).unwrap_or(LogLevel::Info);
    Ok((
        Line {
            project_id: p.id,
            project_key: p.key.clone(),
            launch_id: info.launch.id,
            seq_on,
            log_key,
            step,
            level,
            ts: bounded_or_now(&rq.time, now_ms()),
            message,
            error_uuid,
        },
        target,
    ))
}

fn log_id(rq: &SaveLog) -> String {
    rq.uuid
        .as_deref()
        .filter(|u| ids::valid_uuid(u))
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

/// `POST log/entry`: one line.
pub async fn entry(
    State(s): State<AppState>,
    auth: Result<Auth, ApiError>,
    Path((v, project)): Path<(String, String)>,
    body: Result<Bytes, BytesRejection>,
) -> RpResult<Response> {
    version(&v)?;
    let (auth, p) = s
        .rp_project(auth, &project, &s.perms.launches_write)
        .await?;
    let rq: SaveLog = parse(&s, body, false)?;
    let mut checked = HashMap::new();
    let (line, _) = prepare(&s, &auth, &p, &rq, None, &mut checked).await?;
    submit(&s, vec![line]).await?;
    Ok(json(StatusCode::CREATED, json!({"id": log_id(&rq)})))
}

/// `POST log`: a JSON line or array of lines, or a multipart batch (`json_request_part` with
/// up to 1000 lines, plus the files they name by `file.name`).
pub async fn batch(
    State(s): State<AppState>,
    auth: Result<Auth, ApiError>,
    Path((v, project)): Path<(String, String)>,
    req: Request,
) -> RpResult<Response> {
    version(&v)?;
    let (auth, p) = s
        .rp_project(auth, &project, &s.perms.launches_write)
        .await?;
    let ct = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if let Some(boundary) = multipart::boundary(&ct) {
        return multipart_batch(&s, &auth, &p, req.into_body(), &boundary).await;
    }
    let base = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !(base == "application/json" || base.is_empty()) {
        return Err(RpError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected application/json or multipart/form-data",
        ));
    }
    let body = read_limited(req.into_body(), MAX_JSON_PART_BYTES).await?;
    let first = body.iter().find(|b| !b.is_ascii_whitespace()).copied();
    let mut checked = HashMap::new();
    if first == Some(b'[') {
        let entries: Vec<SaveLog> = parse_bytes(&s, &body)?;
        let (lines, responses) = lines_of(&s, &auth, &p, &entries, &mut [], &mut checked).await?;
        submit(&s, lines).await?;
        return Ok(json(StatusCode::CREATED, json!({"responses": responses})));
    }
    let rq: SaveLog = parse_bytes(&s, &body)?;
    let (line, _) = prepare(&s, &auth, &p, &rq, None, &mut checked).await?;
    submit(&s, vec![line]).await?;
    Ok(json(StatusCode::CREATED, json!({"id": log_id(&rq)})))
}

async fn read_limited(body: Body, max: usize) -> RpResult<Vec<u8>> {
    let mut stream = body.into_data_stream();
    let mut buf = Vec::new();
    while let Some(c) = stream.next().await {
        let c = c.map_err(|e| RpError::bad_request(format!("reading the body failed: {e}")))?;
        if buf.len() + c.len() > max {
            return Err(RpError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("the JSON body exceeds {max} bytes"),
            ));
        }
        buf.extend_from_slice(&c);
    }
    Ok(buf)
}

/// A file part spooled to disk.
struct Spooled {
    filename: String,
    content_type: Option<String>,
    path: PathBuf,
    sha256: String,
    size: u64,
    /// Larger than `FASTPROOF_RP_MAX_ATTACHMENT_BYTES` (not kept).
    too_large: bool,
    /// The attachment stored from it.
    stored: Option<Uuid>,
}

/// Spool files of one request, removed when it ends.
#[derive(Default)]
struct SpoolDir(Vec<PathBuf>);

impl Drop for SpoolDir {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn multipart_error(e: multipart::Error) -> RpError {
    match e {
        multipart::Error::TooLarge(n) => RpError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("the body exceeds FASTPROOF_RP_MAX_REQUEST_BYTES ({n} bytes)"),
        ),
        multipart::Error::TooManyParts(n) => RpError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("more than {n} parts"),
        ),
        other => RpError::bad_request(other.to_string()),
    }
}

async fn multipart_batch(
    s: &AppState,
    auth: &Auth,
    p: &Project,
    body: Body,
    boundary: &str,
) -> RpResult<Response> {
    let _slot = s.guards.spool().await?;
    let dir = s.cfg.spool_dir.join("attachments");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(anyhow::Error::from)?;
    let mut reader = Reader::new(
        body.into_data_stream(),
        boundary,
        multipart::Limits {
            max_parts: MAX_PARTS,
            max_header_bytes: MAX_PART_HEADER_BYTES,
            max_body_bytes: s.cfg.rp.max_request_bytes,
        },
    );
    let mut spool = SpoolDir::default();
    let mut json_part: Option<Vec<u8>> = None;
    let mut files: Vec<Spooled> = Vec::new();
    let max_file = s.cfg.rp.max_attachment_bytes;
    while let Some(head) = reader.next_part().await.map_err(multipart_error)? {
        if head.name.as_deref() == Some("json_request_part") && json_part.is_none() {
            let mut buf = Vec::new();
            while let Some(c) = reader.chunk().await.map_err(multipart_error)? {
                if buf.len() + c.len() > MAX_JSON_PART_BYTES {
                    return Err(RpError::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        format!("json_request_part exceeds {MAX_JSON_PART_BYTES} bytes"),
                    ));
                }
                buf.extend_from_slice(&c);
            }
            json_part = Some(buf);
            continue;
        }
        let Some(filename) = head.filename.clone() else {
            continue;
        };
        let path = dir.join(Uuid::now_v7().to_string());
        spool.0.push(path.clone());
        let mut f = tokio::fs::File::create(&path)
            .await
            .map_err(anyhow::Error::from)?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut too_large = false;
        while let Some(c) = reader.chunk().await.map_err(multipart_error)? {
            size += c.len() as u64;
            if size > max_file {
                too_large = true;
                continue;
            }
            hasher.update(&c);
            f.write_all(&c).await.map_err(anyhow::Error::from)?;
        }
        f.flush().await.map_err(anyhow::Error::from)?;
        drop(f);
        if too_large {
            let _ = tokio::fs::remove_file(&path).await;
        }
        files.push(Spooled {
            filename,
            content_type: head.content_type.clone(),
            path,
            sha256: hex::encode(hasher.finalize()),
            size,
            too_large,
            stored: None,
        });
    }
    let json_part =
        json_part.ok_or_else(|| RpError::bad_request("json_request_part is required"))?;
    let entries: Vec<SaveLog> = parse_bytes(s, &json_part)?;
    let mut checked = HashMap::new();
    let (lines, responses) = lines_of(s, auth, p, &entries, &mut files, &mut checked).await?;
    submit(s, lines).await?;
    Ok(json(StatusCode::CREATED, json!({"responses": responses})))
}

/// Stores a spooled file as an attachment of the project (deduplicated by content).
async fn store_file(
    s: &AppState,
    p: &Project,
    launch_id: Uuid,
    f: &Spooled,
) -> anyhow::Result<Uuid> {
    let existing = s
        .store
        .touch_attachments(p.id, std::slice::from_ref(&f.sha256))
        .await?;
    let key = BlobKey::attachment(&p.key, &f.sha256)?;
    if !existing.contains_key(&f.sha256) {
        s.store.cancel_blob_deletions(&[key.to_string()]).await?;
        s.blobs.put_file(&key, &f.path).await?;
    }
    let ids = s
        .store
        .insert_attachments(
            p.id,
            launch_id,
            &[NewAttachment {
                sha256: f.sha256.clone(),
                size: f.size as i64,
                content_type: clean_content_type(f.content_type.as_deref()),
                original_name: clean_file_name(&f.filename),
                blob_key: key.to_string(),
            }],
        )
        .await?;
    ids.get(&f.sha256)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("attachment row missing"))
}

/// Lines (and files) of a batch with one answer per entry: `{"id"}`, or `{"message"}` for an
/// entry that was refused (an unknown item, a missing or too large file).
async fn lines_of(
    s: &AppState,
    auth: &Auth,
    p: &Project,
    entries: &[SaveLog],
    files: &mut [Spooled],
    checked: &mut HashMap<String, LaunchInfo>,
) -> RpResult<(Vec<Line>, Vec<serde_json::Value>)> {
    if entries.len() > MAX_BATCH_ENTRIES {
        return Err(RpError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("more than {MAX_BATCH_ENTRIES} log entries"),
        ));
    }
    let mut lines = Vec::with_capacity(entries.len());
    let mut responses = Vec::with_capacity(entries.len());
    for rq in entries {
        let file_name = rq
            .file
            .as_ref()
            .and_then(|f| f.name.as_deref())
            .filter(|n| !n.is_empty());
        let shown = file_name.map(|n| clean_file_name(n).unwrap_or_else(|| "attachment".into()));
        let (line, target) = match prepare(s, auth, p, rq, shown.as_deref(), checked).await {
            Ok(x) => x,
            Err(e) if e.status == StatusCode::NOT_FOUND || e.status == StatusCode::BAD_REQUEST => {
                responses.push(json!({"message": e.message, "stackTrace": ""}));
                continue;
            }
            Err(e) => return Err(e),
        };
        if let Some(name) = file_name {
            let known = files.iter().any(|f| f.filename == name);
            let f = files
                .iter_mut()
                .find(|f| f.filename == name && !f.too_large);
            let Some(f) = f else {
                let why = if known {
                    "exceeds FASTPROOF_RP_MAX_ATTACHMENT_BYTES"
                } else {
                    "is not in the request"
                };
                responses.push(json!({"message": format!("Binary data cannot be saved: the file {name:?} {why}"), "stackTrace": ""}));
                continue;
            };
            let id = match f.stored {
                Some(id) => id,
                None => {
                    let id = store_file(s, p, line.launch_id, f).await?;
                    f.stored = Some(id);
                    id
                }
            };
            match &target {
                Some(t) => {
                    let attached = json!([Attached {
                        id: id.to_string(),
                        name: shown.clone(),
                        content_type: f
                            .content_type
                            .as_deref()
                            .map(|c| clean_content_type(Some(c)))
                            .filter(|c| c != "application/octet-stream"),
                    }]);
                    if !s
                        .store
                        .rp_item_attach(p.id, &t.item, t.owner.as_deref(), &attached)
                        .await?
                    {
                        metrics::counter!("fastproof_rp_late_total", "kind" => "attachment")
                            .increment(1);
                        if let Some(o) = &t.owner {
                            s.store.rp_hold(p.id, std::slice::from_ref(o)).await?;
                        }
                        tracing::warn!(project = %p.key, item = %t.item, "a ReportPortal attachment came after its result was emitted; dropped");
                    }
                }
                None => {
                    metrics::counter!("fastproof_rp_late_total", "kind" => "container_attachment")
                        .increment(1);
                }
            }
        }
        responses.push(json!({"id": log_id(rq)}));
        lines.push(line);
    }
    Ok((lines, responses))
}
