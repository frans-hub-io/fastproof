//! Live logs of attempts: zstd-compressed chunks per attempt (`attempt_log_chunks`), idempotent
//! by `(resultId, attempt, seq)`, with a `log.chunk` outbox event per chunk and a notification
//! on [`LOGS_CHANNEL`] for the live fan-out of every replica.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{Store, codec, no_nul, opt_no_nul, ts_ms};

/// Channel of log chunk notifications (`pg_notify`), JSON payload [`LogNotice`].
pub const LOGS_CHANNEL: &str = "fastproof_logs";
/// Lines per stored chunk, at most.
pub const CHUNK_LINES: usize = 1000;
/// Decoded bytes per stored chunk, at most (a longer run of lines starts a new chunk).
pub const CHUNK_BYTES: usize = 1024 * 1024;
/// Notifications carry the lines when they fit; otherwise listeners read the chunk.
const NOTIFY_INLINE_BYTES: usize = 7000;
/// Chunks a read or search decodes per request, at most.
pub const MAX_SCAN_CHUNKS: i64 = 400;
/// Decoded bytes a read or search handles per request, at most.
pub const MAX_SCAN_BYTES: usize = 64 * 1024 * 1024;

/// Severity of a log line.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    #[serde(alias = "warning")]
    Warn,
    #[serde(alias = "fatal", alias = "critical")]
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(s.trim().to_ascii_lowercase())).ok()
    }

    fn bit(self) -> i16 {
        1 << (self as i16)
    }

    /// The bit mask of this level and every more severe one.
    pub fn at_least_mask(self) -> i16 {
        let mut m = 0;
        for l in [
            LogLevel::Trace,
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
        ] {
            if l >= self {
                m |= l.bit();
            }
        }
        m
    }
}

/// Where a line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum LogStream {
    Stdout,
    Stderr,
    Log,
    Step,
}

/// One log line as clients send it and as the API returns it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogLine {
    /// Client id of the result (the `id` of its `ReportResult`).
    pub result_id: String,
    /// Zero-based retry number.
    pub attempt: u32,
    /// Sequence number within the attempt; a line sent again with the same number is a no-op.
    pub seq: u64,
    /// Time, milliseconds since the Unix epoch (an RFC 3339 string is accepted too).
    #[serde(deserialize_with = "de_ts")]
    #[schema(value_type = i64)]
    pub ts: i64,
    pub level: LogLevel,
    pub stream: LogStream,
    pub message: String,
    /// Name of the step the line belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
}

fn de_ts<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    use serde::de::Error;
    match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f as i64))
            .ok_or_else(|| D::Error::custom("ts must be milliseconds since the epoch")),
        serde_json::Value::String(t) => DateTime::parse_from_rfc3339(t.trim())
            .map(|t| t.timestamp_millis())
            .map_err(|_| {
                D::Error::custom("ts must be milliseconds since the epoch or an RFC 3339 time")
            }),
        _ => Err(D::Error::custom("ts must be milliseconds since the epoch")),
    }
}

/// A line as stored in a chunk (the chunk holds the result and attempt).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredLine {
    q: i64,
    t: i64,
    l: LogLevel,
    s: LogStream,
    m: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    p: Option<String>,
}

/// A log line with its launch, as reads and searches return it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub launch_id: Uuid,
    #[serde(flatten)]
    pub line: LogLine,
}

/// A chunk written by [`insert_log_lines`].
#[derive(Clone, Debug)]
pub struct WrittenChunk {
    pub id: i64,
    pub result_id: String,
    pub attempt: u32,
    pub lines: Vec<LogLine>,
    pub bytes: usize,
}

/// A notification on [`LOGS_CHANNEL`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogNotice {
    pub project: String,
    #[serde(rename = "launchId")]
    pub launch_id: Uuid,
    #[serde(rename = "chunkId")]
    pub chunk_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<Vec<LogLine>>,
}

/// A stored chunk.
#[derive(Clone, Debug)]
pub struct LogChunk {
    pub id: i64,
    pub project_id: Uuid,
    pub project_key: String,
    pub launch_id: Uuid,
    pub launch_created_at: DateTime<Utc>,
    pub lines: Vec<LogLine>,
}

fn line_bytes(l: &LogLine) -> usize {
    l.message.len() + l.step.as_ref().map_or(0, |s| s.len()) + l.result_id.len() + 32
}

/// Splits lines into runs of at most [`CHUNK_LINES`] lines and [`CHUNK_BYTES`] bytes (a
/// single longer line is a run of its own).
fn byte_chunks(lines: &[LogLine]) -> Vec<&[LogLine]> {
    let mut out = Vec::new();
    let (mut start, mut bytes) = (0, 0);
    for (i, l) in lines.iter().enumerate() {
        let b = line_bytes(l);
        if i > start && (i - start >= CHUNK_LINES || bytes + b > CHUNK_BYTES) {
            out.push(&lines[start..i]);
            start = i;
            bytes = 0;
        }
        bytes += b;
    }
    if start < lines.len() {
        out.push(&lines[start..]);
    }
    out
}

fn decode_lines(result_id: &str, attempt: i32, z: &[u8]) -> anyhow::Result<Vec<LogLine>> {
    let stored: Vec<StoredLine> = codec::decode(z)?;
    Ok(stored
        .into_iter()
        .map(|s| LogLine {
            result_id: result_id.to_string(),
            attempt: attempt.max(0) as u32,
            seq: s.q.max(0) as u64,
            ts: s.t,
            level: s.l,
            stream: s.s,
            message: s.m,
            step: s.p,
        })
        .collect())
}

/// Writes lines of one launch as chunks, skipping `(resultId, attempt, seq)` already stored,
/// and records a `log.chunk` outbox event per chunk. Call inside a transaction; notify with
/// [`notify_log_chunks`] in the same transaction.
pub async fn insert_log_lines(
    conn: &mut PgConnection,
    project_id: Uuid,
    project_key: &str,
    launch_id: Uuid,
    lines: Vec<LogLine>,
) -> anyhow::Result<Vec<WrittenChunk>> {
    let mut groups: BTreeMap<(String, u32), BTreeMap<u64, LogLine>> = BTreeMap::new();
    for mut l in lines {
        l.result_id = no_nul(&l.result_id);
        l.message = no_nul(&l.message);
        l.step = opt_no_nul(l.step.as_deref());
        groups
            .entry((l.result_id.clone(), l.attempt))
            .or_default()
            .entry(l.seq)
            .or_insert(l);
    }
    let mut out = Vec::new();
    for ((result_id, attempt), by_seq) in groups {
        let (Some(lo), Some(hi)) = (by_seq.keys().next(), by_seq.keys().next_back()) else {
            continue;
        };
        let existing: Vec<Vec<u8>> = sqlx::query_scalar(
            "select lines_z from attempt_log_chunks where launch_id = $1 and result_key = $2 \
             and attempt = $3 and first_seq <= $5 and last_seq >= $4",
        )
        .bind(launch_id)
        .bind(&result_id)
        .bind(attempt as i32)
        .bind(*lo as i64)
        .bind(*hi as i64)
        .fetch_all(&mut *conn)
        .await?;
        let mut stored_seqs = HashSet::new();
        for z in existing {
            for l in decode_lines(&result_id, attempt as i32, &z)? {
                stored_seqs.insert(l.seq);
            }
        }
        let fresh: Vec<LogLine> = by_seq
            .into_values()
            .filter(|l| !stored_seqs.contains(&l.seq))
            .collect();
        for part in byte_chunks(&fresh) {
            let stored: Vec<StoredLine> = part
                .iter()
                .map(|l| StoredLine {
                    q: l.seq.min(i64::MAX as u64) as i64,
                    t: l.ts,
                    l: l.level,
                    s: l.stream,
                    m: l.message.clone(),
                    p: l.step.clone(),
                })
                .collect();
            let levels = part.iter().fold(0i16, |m, l| m | l.level.bit());
            let bytes: usize = part.iter().map(line_bytes).sum();
            let first = part.first().map(|l| l.seq as i64).unwrap_or_default();
            let last = part.last().map(|l| l.seq as i64).unwrap_or_default();
            let first_ts = part.iter().map(|l| l.ts).min();
            let last_ts = part.iter().map(|l| l.ts).max();
            let id: i64 = sqlx::query_scalar(
                "insert into attempt_log_chunks (project_id, launch_id, result_key, attempt, first_seq, last_seq, \
                   line_count, levels, bytes, first_ts, last_ts, lines_z) \
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) returning id",
            )
            .bind(project_id)
            .bind(launch_id)
            .bind(&result_id)
            .bind(attempt as i32)
            .bind(first)
            .bind(last)
            .bind(part.len() as i32)
            .bind(levels)
            .bind(bytes.min(i32::MAX as usize) as i32)
            .bind(ts_ms(first_ts))
            .bind(ts_ms(last_ts))
            .bind(codec::encode(&stored)?)
            .fetch_one(&mut *conn)
            .await?;
            crate::events::emit(
                &mut *conn,
                "log.chunk",
                Some(project_key),
                serde_json::json!({"id": id, "launchId": launch_id, "resultId": result_id,
                    "attempt": attempt, "firstSeq": first, "lastSeq": last, "lines": part.len()}),
            )
            .await?;
            out.push(WrittenChunk {
                id,
                result_id: result_id.clone(),
                attempt,
                lines: part.to_vec(),
                bytes,
            });
        }
    }
    Ok(out)
}

/// Sends one [`LogNotice`] per chunk (delivered when the transaction commits).
pub async fn notify_log_chunks(
    conn: &mut PgConnection,
    project_key: &str,
    launch_id: Uuid,
    chunks: &[WrittenChunk],
) -> anyhow::Result<()> {
    for c in chunks {
        let mut n = LogNotice {
            project: project_key.to_string(),
            launch_id,
            chunk_id: c.id,
            lines: None,
        };
        if c.bytes < NOTIFY_INLINE_BYTES {
            n.lines = Some(c.lines.clone());
        }
        let mut payload = serde_json::to_string(&n)?;
        if payload.len() >= 7900 {
            n.lines = None;
            payload = serde_json::to_string(&n)?;
        }
        sqlx::query("select pg_notify($1, $2)")
            .bind(LOGS_CHANNEL)
            .bind(payload)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// Filters of stored log reads.
#[derive(Clone, Debug, Default)]
pub struct LogFilter {
    /// Result keys (client ids of attempts); empty for every result.
    pub result_ids: Vec<String>,
    pub attempt: Option<u32>,
    pub level: Option<LogLevel>,
    /// Case-insensitive substring of the message.
    pub q: Option<String>,
    /// Cursor `chunkId:seq`: lines before it.
    pub before: Option<String>,
    pub limit: i64,
}

impl LogFilter {
    /// Whether a line passes the level and text filters.
    pub fn matches(&self, l: &LogLine, q_lower: Option<&str>) -> bool {
        if let Some(min) = self.level
            && l.level < min
        {
            return false;
        }
        if let Some(q) = q_lower
            && !contains_ci(&l.message, q)
        {
            return false;
        }
        true
    }
}

/// Case-insensitive substring test; `needle` must be lower case.
pub fn contains_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.is_ascii() && needle.is_ascii() {
        let h = haystack.as_bytes();
        let n = needle.as_bytes();
        return h
            .windows(n.len())
            .any(|w| w.iter().zip(n).all(|(a, b)| a.to_ascii_lowercase() == *b));
    }
    haystack.to_lowercase().contains(needle)
}

fn parse_cursor(s: &str) -> Option<(i64, i64)> {
    let (a, b) = s.split_once(':')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// Matching log lines of a search.
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
pub struct LogHits {
    pub items: Vec<LogEntry>,
    /// True when the scan stopped at its budget before the time range was covered.
    pub partial: bool,
}

/// Filters of a project-wide log search.
#[derive(Clone, Debug, Default)]
pub struct LogSearch {
    pub q: String,
    pub launch_id: Option<Uuid>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub level: Option<LogLevel>,
    pub limit: i64,
}

impl Store {
    /// Adds `bytes` to the log volume of a launch unless that exceeds `max`; false when it would.
    pub async fn charge_log_bytes(
        &self,
        launch_id: Uuid,
        bytes: i64,
        max: i64,
    ) -> anyhow::Result<bool> {
        let r = sqlx::query(
            "update launches set log_bytes = log_bytes + $2 where id = $1 and log_bytes + $2 <= $3",
        )
        .bind(launch_id)
        .bind(bytes)
        .bind(max)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// The launches among `ids` that exist in the project (search hits from analytics
    /// backends are checked against it, so deleted launches and projects never show).
    pub async fn live_launches(
        &self,
        project_id: Uuid,
        ids: impl IntoIterator<Item = Uuid>,
    ) -> anyhow::Result<HashSet<Uuid>> {
        let ids: Vec<Uuid> = ids
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        let rows: Vec<Uuid> =
            sqlx::query_scalar("select id from launches where project_id = $1 and id = any($2)")
                .bind(project_id)
                .bind(&ids)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().collect())
    }

    /// Writes log lines of one launch in one transaction and notifies the live fan-out.
    pub async fn write_log_lines(
        &self,
        project_id: Uuid,
        project_key: &str,
        launch_id: Uuid,
        lines: Vec<LogLine>,
    ) -> anyhow::Result<Vec<WrittenChunk>> {
        let mut tx = self.pool.begin().await?;
        let chunks = insert_log_lines(&mut tx, project_id, project_key, launch_id, lines).await?;
        notify_log_chunks(&mut tx, project_key, launch_id, &chunks).await?;
        tx.commit().await?;
        Ok(chunks)
    }

    /// Chunks by id with their project and launch (for sinks and the live fan-out).
    /// Client ids of the attempts of a result that were sent through the results API: the
    /// `resultId` their log lines carry.
    pub async fn result_client_ids(
        &self,
        launch_id: Uuid,
        result_id: Uuid,
    ) -> anyhow::Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "select substr(client_key, 3) from attempts where launch_id = $1 and result_id = $2 \
             and client_key like 'r:%' order by seq limit 100",
        )
        .bind(launch_id)
        .bind(result_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn log_chunks(&self, ids: &[i64]) -> anyhow::Result<Vec<LogChunk>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "select c.id, c.project_id, p.key, c.launch_id, l.created_at, c.result_key, c.attempt, c.lines_z \
             from attempt_log_chunks c join launches l on l.id = c.launch_id join projects p on p.id = c.project_id \
             where c.id = any($1) order by c.id",
        )
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let result_id: String = r.try_get("result_key")?;
            let attempt: i32 = r.try_get("attempt")?;
            let z: Vec<u8> = r.try_get("lines_z")?;
            out.push(LogChunk {
                id: r.try_get("id")?,
                project_id: r.try_get("project_id")?,
                project_key: r.try_get("key")?,
                launch_id: r.try_get("launch_id")?,
                launch_created_at: r.try_get("created_at")?,
                lines: decode_lines(&result_id, attempt, &z)?,
            });
        }
        Ok(out)
    }

    /// Stored lines of a launch, oldest first, the last `limit` before the cursor.
    pub async fn launch_logs(
        &self,
        launch_id: Uuid,
        f: &LogFilter,
    ) -> anyhow::Result<crate::Page<LogLine>> {
        let cursor = match f.before.as_deref() {
            None => None,
            Some(c) => Some(parse_cursor(c).ok_or_else(|| anyhow::anyhow!("invalid cursor"))?),
        };
        let mask = f.level.map(LogLevel::at_least_mask).unwrap_or(0x1f);
        let q_lower =
            f.q.as_deref()
                .map(|q| q.to_lowercase())
                .filter(|q| !q.is_empty());
        let mut rows = sqlx::query(
            "select id, result_key, attempt, bytes, lines_z from attempt_log_chunks where launch_id = $1 \
             and (cardinality($2::text[]) = 0 or result_key = any($2)) and ($3::int is null or attempt = $3) \
             and (levels & $4) <> 0 and ($5::bigint is null or id <= $5) order by id desc limit $6",
        )
        .bind(launch_id)
        .bind(&f.result_ids)
        .bind(f.attempt.map(|a| a as i32))
        .bind(mask)
        .bind(cursor.map(|c| c.0))
        .bind(MAX_SCAN_CHUNKS)
        .fetch(&self.pool);
        let limit = f.limit.max(1) as usize;
        let mut picked: Vec<(i64, LogLine)> = Vec::new();
        let mut decoded = 0usize;
        let mut stopped_at: Option<(i64, i64)> = None;
        let mut seen_rows = 0i64;
        let mut last_chunk = None;
        'chunks: while let Some(r) = rows.try_next().await? {
            seen_rows += 1;
            let id: i64 = r.try_get("id")?;
            let size: i32 = r.try_get("bytes")?;
            if decoded > 0 && decoded + size.max(0) as usize > MAX_SCAN_BYTES {
                stopped_at = Some((id, i64::MAX));
                break;
            }
            let result_id: String = r.try_get("result_key")?;
            let attempt: i32 = r.try_get("attempt")?;
            let z: Vec<u8> = r.try_get("lines_z")?;
            let lines = decode_lines(&result_id, attempt, &z)?;
            decoded += lines.iter().map(line_bytes).sum::<usize>();
            last_chunk = Some(id);
            for l in lines.into_iter().rev() {
                if let Some((cid, cseq)) = cursor
                    && id == cid
                    && l.seq as i64 >= cseq
                {
                    continue;
                }
                if !f.matches(&l, q_lower.as_deref()) {
                    continue;
                }
                let seq = l.seq as i64;
                picked.push((id, l));
                if picked.len() >= limit {
                    stopped_at = Some((id, seq));
                    break 'chunks;
                }
            }
            if decoded > MAX_SCAN_BYTES {
                stopped_at = Some((id, 0));
                break;
            }
        }
        drop(rows);
        let exhausted_rows = seen_rows < MAX_SCAN_CHUNKS && stopped_at.is_none();
        let next = match stopped_at {
            Some((id, seq)) => Some(format!("{id}:{seq}")),
            None if !exhausted_rows => last_chunk.map(|id| format!("{id}:0")),
            None => None,
        };
        picked.reverse();
        Ok(crate::Page {
            items: picked.into_iter().map(|(_, l)| l).collect(),
            next,
        })
    }

    /// Searches stored lines of a project by a case-insensitive substring, newest chunks
    /// first, within a scan budget.
    pub async fn search_logs(&self, project_id: Uuid, s: &LogSearch) -> anyhow::Result<LogHits> {
        let to = s.to.unwrap_or_else(Utc::now);
        let from = s.from.unwrap_or_else(|| to - chrono::Duration::days(7));
        let mask = s.level.map(LogLevel::at_least_mask).unwrap_or(0x1f);
        let q = s.q.to_lowercase();
        let mut rows = sqlx::query(
            "select id, launch_id, result_key, attempt, bytes, lines_z from attempt_log_chunks where project_id = $1 \
             and created_at >= $2 and created_at <= $3 and ($4::uuid is null or launch_id = $4) \
             and (levels & $5) <> 0 order by created_at desc, id desc limit $6",
        )
        .bind(project_id)
        .bind(from)
        .bind(to)
        .bind(s.launch_id)
        .bind(mask)
        .bind(MAX_SCAN_CHUNKS * 5)
        .fetch(&self.pool);
        let mut hits = LogHits::default();
        let mut decoded = 0usize;
        let mut seen_rows = 0i64;
        while let Some(r) = rows.try_next().await? {
            seen_rows += 1;
            let size: i32 = r.try_get("bytes")?;
            if decoded > 0 && decoded + size.max(0) as usize > MAX_SCAN_BYTES * 2 {
                hits.partial = true;
                return Ok(hits);
            }
            let launch_id: Uuid = r.try_get("launch_id")?;
            let result_id: String = r.try_get("result_key")?;
            let attempt: i32 = r.try_get("attempt")?;
            let z: Vec<u8> = r.try_get("lines_z")?;
            let lines = decode_lines(&result_id, attempt, &z)?;
            decoded += lines.iter().map(line_bytes).sum::<usize>();
            for l in lines {
                if s.level.is_some_and(|min| l.level < min) || !contains_ci(&l.message, &q) {
                    continue;
                }
                if !in_range(l.ts, s.from, s.to) {
                    continue;
                }
                hits.items.push(LogEntry { launch_id, line: l });
                if hits.items.len() as i64 >= s.limit {
                    return Ok(hits);
                }
            }
            if decoded > MAX_SCAN_BYTES * 2 {
                hits.partial = true;
                return Ok(hits);
            }
        }
        hits.partial = seen_rows >= MAX_SCAN_CHUNKS * 5;
        Ok(hits)
    }
}

fn in_range(ts: i64, from: Option<DateTime<Utc>>, to: Option<DateTime<Utc>>) -> bool {
    from.is_none_or(|f| ts >= f.timestamp_millis()) && to.is_none_or(|t| ts <= t.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_and_text() {
        assert_eq!(LogLevel::parse("WARNING"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::parse("nope"), None);
        assert_eq!(LogLevel::Warn.at_least_mask(), 0b11000);
        assert!(contains_ci("Connection REFUSED by host", "refused"));
        assert!(!contains_ci("abc", "abcd"));
        assert!(contains_ci("Ошибка СЕТИ", "сети"));
        let l: LogLine = serde_json::from_str(
            r#"{"resultId":"r1","attempt":0,"seq":3,"ts":1,"level":"info","stream":"stdout","message":"m"}"#,
        )
        .unwrap();
        assert_eq!(l.seq, 3);
        let l: LogLine = serde_json::from_str(
            r#"{"resultId":"r1","attempt":0,"seq":3,"ts":"1970-01-01T00:00:01.5Z","level":"info","stream":"step","message":"m","step":"a › b"}"#,
        )
        .unwrap();
        assert_eq!(l.ts, 1500);
        assert!(serde_json::from_str::<LogLine>(r#"{"resultId":"r1"}"#).is_err());
    }

    #[test]
    fn chunks_are_bounded_by_bytes() {
        let line = |n: usize| LogLine {
            result_id: "r".into(),
            attempt: 0,
            seq: 0,
            ts: 0,
            level: LogLevel::Info,
            stream: LogStream::Stdout,
            message: "x".repeat(n),
            step: None,
        };
        let big: Vec<LogLine> = (0..40).map(|_| line(64 * 1024)).collect();
        let parts = byte_chunks(&big);
        assert!(parts.len() >= 3, "{}", parts.len());
        assert!(
            parts
                .iter()
                .all(|p| p.iter().map(line_bytes).sum::<usize>() <= CHUNK_BYTES)
        );
        assert_eq!(parts.iter().map(|p| p.len()).sum::<usize>(), 40);
        let small: Vec<LogLine> = (0..2500).map(|_| line(10)).collect();
        assert_eq!(
            byte_chunks(&small)
                .iter()
                .map(|p| p.len())
                .collect::<Vec<_>>(),
            vec![1000, 1000, 500]
        );
        let huge = vec![line(2 * CHUNK_BYTES)];
        assert_eq!(byte_chunks(&huge).len(), 1);
    }
}
