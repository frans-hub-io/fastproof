//! ClickHouse over its HTTP interface (`JSONEachRow` inserts and reads, parameterized
//! queries): the sink tables, the `series_daily` rollup and the search queries.

use std::time::Duration;

use anyhow::{Context, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fastproof_store::logs::{LogEntry, LogHits, LogLevel, LogLine, LogSearch, LogStream};
use fastproof_store::stream::{SimilarResult, SimilarSource, normalize_message};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::config::ClickHouseConfig;
use crate::http::{HttpBackend, error_text};
use crate::sink::{Deletions, SearchBackend, Sink, SinkBatch, flatten_steps, match_key};

/// Settings of every read query: a time limit and a memory limit per query.
const READ_SETTINGS: [(&str, &str); 3] = [
    ("max_execution_time", "30"),
    ("timeout_overflow_mode", "throw"),
    ("max_memory_usage", "2147483648"),
];

/// A query parameter value in ClickHouse's escaped format, so that backslashes, tabs and line
/// breaks in user text reach the query as written.
pub fn escape_param(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out
}

/// The ClickHouse backend.
#[derive(Clone, Debug)]
pub struct ClickHouse {
    http: HttpBackend,
    cfg: ClickHouseConfig,
}

fn ts(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

fn ts_ms(ms: i64) -> String {
    let clamped = ms.clamp(0, 4_102_444_800_000);
    DateTime::from_timestamp_millis(clamped)
        .map(ts)
        .unwrap_or_else(|| "1970-01-01 00:00:00.000".into())
}

/// Tokens of a search text, as the text index splits them.
pub fn tokens(q: &str) -> Vec<String> {
    q.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(16)
        .map(str::to_string)
        .collect()
}

impl ClickHouse {
    pub fn new(cfg: ClickHouseConfig) -> anyhow::Result<Self> {
        Ok(Self {
            http: HttpBackend::new(cfg.endpoint.clone(), "FASTPROOF_CLICKHOUSE")?,
            cfg,
        })
    }

    fn db(&self) -> &str {
        &self.cfg.database
    }

    /// Runs a statement; `params` bind `{name:Type}` placeholders (values are escaped).
    pub async fn exec(
        &self,
        sql: &str,
        params: &[(&str, String)],
        body: Option<String>,
    ) -> anyhow::Result<String> {
        self.exec_with(sql, params, body, &[]).await
    }

    async fn exec_with(
        &self,
        sql: &str,
        params: &[(&str, String)],
        body: Option<String>,
        settings: &[(&str, &str)],
    ) -> anyhow::Result<String> {
        let mut q: Vec<(String, String)> = Vec::new();
        for (k, v) in params {
            q.push((format!("param_{k}"), escape_param(v)));
        }
        for (k, v) in settings {
            q.push((k.to_string(), v.to_string()));
        }
        q.push(("date_time_input_format".into(), "best_effort".into()));
        // A statement without data goes in the body; with data, the statement is the `query`
        // parameter and the rows are the body.
        let body = match body {
            Some(b) => {
                q.push(("query".into(), sql.to_string()));
                b
            }
            None => sql.to_string(),
        };
        let rb = self
            .http
            .request(reqwest::Method::POST, "/")?
            .query(&q)
            .header(reqwest::header::CONTENT_LENGTH, body.len())
            .body(body);
        let r = rb.send().await.context("ClickHouse request failed")?;
        if !r.status().is_success() {
            bail!("ClickHouse: {}", error_text(r).await);
        }
        Ok(r.text().await?)
    }

    /// Reads rows of a `FORMAT JSONEachRow` query (with [`READ_SETTINGS`]).
    pub async fn rows<T: for<'de> Deserialize<'de>>(
        &self,
        sql: &str,
        params: &[(&str, String)],
    ) -> anyhow::Result<Vec<T>> {
        let text = self.exec_with(sql, params, None, &READ_SETTINGS).await?;
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).with_context(|| "decoding a ClickHouse row"))
            .collect()
    }

    async fn insert(
        &self,
        table: &str,
        rows: &[serde_json::Value],
        token: &str,
    ) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut body = String::new();
        for r in rows {
            body.push_str(&serde_json::to_string(r)?);
            body.push('\n');
        }
        let sql = format!(
            "INSERT INTO {}.{table} SETTINGS insert_deduplication_token = {{tok:String}}, async_insert = 0 FORMAT JSONEachRow",
            self.db()
        );
        self.exec(&sql, &[("tok", format!("{token}-{table}"))], Some(body))
            .await
            .with_context(|| format!("inserting into {table}"))?;
        Ok(())
    }

    /// Major and minor version of the server.
    pub async fn version(&self) -> anyhow::Result<(u32, u32)> {
        let v = self.exec("SELECT version()", &[], None).await?;
        let mut parts = v.trim().split('.');
        let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        Ok((major, minor))
    }

    fn schema(&self, text_index: bool) -> Vec<String> {
        let db = self.db();
        let ttl = self.cfg.raw_ttl_days;
        let msg_index = if text_index {
            "INDEX msg_idx lower(message) TYPE text(tokenizer = splitByNonAlpha)"
        } else {
            "INDEX msg_idx lower(message) TYPE tokenbf_v1(32768, 3, 0) GRANULARITY 1"
        };
        vec![
            format!("CREATE DATABASE IF NOT EXISTS {db}"),
            format!(
                "CREATE TABLE IF NOT EXISTS {db}.results (project LowCardinality(String), project_id UUID, launch_id UUID, \
                 launch_number Int64, launch_created_at DateTime64(3, 'UTC'), id UUID, series_id UUID, test_case_id UUID, \
                 name String, full_name String, status LowCardinality(String), flaky_in_run Bool, attempt_count UInt32, \
                 started_at Nullable(DateTime64(3, 'UTC')), duration_ms Nullable(Int64), message String, message_norm String, \
                 match_key String, fingerprint String, branch String, env_key String, version UInt64) \
                 ENGINE = ReplacingMergeTree(version) PARTITION BY toYYYYMM(launch_created_at) \
                 ORDER BY (project, launch_id, id) TTL toDateTime(launch_created_at) + INTERVAL {ttl} DAY \
                 SETTINGS non_replicated_deduplication_window = 1000"
            ),
            format!(
                "CREATE TABLE IF NOT EXISTS {db}.attempts (project LowCardinality(String), project_id UUID, launch_id UUID, \
                 launch_created_at DateTime64(3, 'UTC'), result_id UUID, id UUID, series_id UUID, client_key String, seq Int64, \
                 status LowCardinality(String), started_at Nullable(DateTime64(3, 'UTC')), duration_ms Nullable(Int64), \
                 message String, trace String, version UInt64) \
                 ENGINE = ReplacingMergeTree(version) PARTITION BY toYYYYMM(launch_created_at) \
                 ORDER BY (project, launch_id, result_id, id) TTL toDateTime(launch_created_at) + INTERVAL {ttl} DAY \
                 SETTINGS non_replicated_deduplication_window = 1000"
            ),
            format!(
                "CREATE TABLE IF NOT EXISTS {db}.steps (project LowCardinality(String), launch_id UUID, \
                 launch_created_at DateTime64(3, 'UTC'), result_id UUID, attempt_id UUID, idx UInt32, parent_idx Int32, \
                 depth UInt16, name String, status LowCardinality(String), duration_ms Nullable(Int64), message String, \
                 version UInt64) \
                 ENGINE = ReplacingMergeTree(version) PARTITION BY toYYYYMM(launch_created_at) \
                 ORDER BY (project, launch_id, attempt_id, idx) TTL toDateTime(launch_created_at) + INTERVAL {ttl} DAY \
                 SETTINGS non_replicated_deduplication_window = 1000"
            ),
            format!(
                "CREATE TABLE IF NOT EXISTS {db}.logs (project LowCardinality(String), launch_id UUID, \
                 launch_created_at DateTime64(3, 'UTC'), result_id String, attempt UInt32, seq UInt64, ts DateTime64(3, 'UTC'), \
                 level LowCardinality(String), stream LowCardinality(String), message String, step String, chunk_id Int64, \
                 {msg_index}) \
                 ENGINE = ReplacingMergeTree PARTITION BY toYYYYMM(launch_created_at) \
                 ORDER BY (project, launch_id, result_id, attempt, seq) TTL toDateTime(launch_created_at) + INTERVAL {ttl} DAY \
                 SETTINGS non_replicated_deduplication_window = 1000"
            ),
            format!(
                "CREATE TABLE IF NOT EXISTS {db}.series_daily (project LowCardinality(String), series_id UUID, day Date, \
                 branch String, env_key String, runs AggregateFunction(uniq, UUID), \
                 passed AggregateFunction(uniqIf, UUID, UInt8), failed AggregateFunction(uniqIf, UUID, UInt8), \
                 broken AggregateFunction(uniqIf, UUID, UInt8), skipped AggregateFunction(uniqIf, UUID, UInt8), \
                 flaky AggregateFunction(uniqIf, UUID, UInt8), \
                 duration AggregateFunction(quantiles(0.5, 0.9, 0.99), Int64)) \
                 ENGINE = AggregatingMergeTree PARTITION BY toYYYYMM(day) ORDER BY (project, series_id, day, branch, env_key) \
                 TTL day + INTERVAL {ttl} DAY"
            ),
            format!(
                "CREATE MATERIALIZED VIEW IF NOT EXISTS {db}.series_daily_mv TO {db}.series_daily AS \
                 SELECT project, series_id, toDate(launch_created_at) AS day, branch, env_key, uniqState(id) AS runs, \
                 uniqIfState(id, toUInt8(status = 'passed')) AS passed, uniqIfState(id, toUInt8(status = 'failed')) AS failed, \
                 uniqIfState(id, toUInt8(status = 'broken')) AS broken, uniqIfState(id, toUInt8(status = 'skipped')) AS skipped, \
                 uniqIfState(id, toUInt8(flaky_in_run)) AS flaky, \
                 quantilesState(0.5, 0.9, 0.99)(ifNull(duration_ms, toInt64(0))) AS duration \
                 FROM {db}.results WHERE status != 'running' GROUP BY project, series_id, day, branch, env_key"
            ),
        ]
    }
}

fn uuid_s(u: Uuid) -> String {
    u.to_string()
}

#[async_trait]
impl Sink for ClickHouse {
    fn name(&self) -> &'static str {
        "clickhouse"
    }

    async fn prepare(&self) -> anyhow::Result<()> {
        let (major, minor) = self
            .version()
            .await
            .context("reading the ClickHouse version")?;
        let text_index = (major, minor) >= (26, 2);
        if !text_index {
            tracing::warn!(
                major,
                minor,
                "ClickHouse is older than 26.2: log search uses a tokenbf_v1 skip index"
            );
        }
        for sql in self.schema(text_index) {
            self.exec(&sql, &[], None).await.with_context(|| {
                format!(
                    "creating the ClickHouse schema: {}",
                    sql.chars().take(80).collect::<String>()
                )
            })?;
        }
        Ok(())
    }

    async fn write(&self, b: &SinkBatch, token: &str) -> anyhow::Result<()> {
        let version = Utc::now().timestamp_micros().max(0) as u64;
        let results: Vec<serde_json::Value> = b
            .results
            .iter()
            .map(|r| {
                let message = r.message.clone().unwrap_or_default();
                json!({"project": r.project, "project_id": uuid_s(r.project_id), "launch_id": uuid_s(r.launch_id),
                    "launch_number": r.launch_number, "launch_created_at": ts(r.launch_created_at), "id": uuid_s(r.id),
                    "series_id": uuid_s(r.series_id), "test_case_id": uuid_s(r.test_case_id), "name": r.name,
                    "full_name": r.full_name, "status": r.status, "flaky_in_run": r.flaky_in_run,
                    "attempt_count": r.attempt_count.max(0), "started_at": r.started_at.map(ts),
                    "duration_ms": r.duration_ms, "message_norm": normalize_message(&message),
                    "match_key": match_key(r.fingerprint.as_deref(), &message),
                    "message": message, "fingerprint": r.fingerprint.clone().unwrap_or_default(),
                    "branch": r.branch.clone().unwrap_or_default(), "env_key": r.env_key.clone().unwrap_or_default(),
                    "version": r.updated_at.timestamp_micros().max(0) as u64})
            })
            .collect();
        let mut attempts = Vec::with_capacity(b.attempts.len());
        let mut steps = Vec::new();
        for a in &b.attempts {
            attempts.push(json!({"project": a.project, "project_id": uuid_s(a.project_id),
                "launch_id": uuid_s(a.launch_id), "launch_created_at": ts(a.launch_created_at),
                "result_id": uuid_s(a.result_id), "id": uuid_s(a.id), "series_id": uuid_s(a.series_id),
                "client_key": a.client_key, "seq": a.seq, "status": a.status, "started_at": a.started_at.map(ts),
                "duration_ms": a.duration_ms, "message": a.message.clone().unwrap_or_default(),
                "trace": a.trace.clone().unwrap_or_default(), "version": version}));
            for s in flatten_steps(&a.steps) {
                steps.push(json!({"project": a.project, "launch_id": uuid_s(a.launch_id),
                    "launch_created_at": ts(a.launch_created_at), "result_id": uuid_s(a.result_id),
                    "attempt_id": uuid_s(a.id), "idx": s.idx, "parent_idx": s.parent, "depth": s.depth,
                    "name": s.name, "status": s.status, "duration_ms": s.duration_ms, "message": s.message,
                    "version": version}));
            }
        }
        let mut logs = Vec::new();
        for c in &b.logs {
            for l in &c.lines {
                logs.push(json!({"project": c.project_key, "launch_id": uuid_s(c.launch_id),
                    "launch_created_at": ts(c.launch_created_at), "result_id": l.result_id, "attempt": l.attempt,
                    "seq": l.seq, "ts": ts_ms(l.ts), "level": l.level.as_str(), "stream": stream_str(l.stream),
                    "message": l.message, "step": l.step.clone().unwrap_or_default(), "chunk_id": c.id}));
            }
        }
        self.insert("results", &results, token).await?;
        self.insert("attempts", &attempts, token).await?;
        self.insert("steps", &steps, token).await?;
        self.insert("logs", &logs, token).await?;
        Ok(())
    }

    async fn delete(&self, d: &Deletions) -> anyhow::Result<()> {
        let db = self.db().to_string();
        let mut by_project: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
        for (p, l) in &d.launches {
            by_project
                .entry(p.as_str())
                .or_default()
                .push(l.to_string());
        }
        for (project, ids) in by_project {
            for part in ids.chunks(1000) {
                let ids = serde_json::to_string(part)?.replace('"', "'");
                for table in ["results", "attempts", "steps", "logs"] {
                    self.exec(
                        &format!(
                            "DELETE FROM {db}.{table} WHERE project = {{p:String}} AND launch_id IN {{ids:Array(UUID)}}"
                        ),
                        &[("p", project.to_string()), ("ids", ids.clone())],
                        None,
                    )
                    .await
                    .with_context(|| format!("deleting launches from {table}"))?;
                }
            }
        }
        for (project, at) in &d.projects {
            let params = [("p", project.clone()), ("at", ts(*at))];
            for table in ["results", "attempts", "steps", "logs"] {
                self.exec(
                    &format!(
                        "DELETE FROM {db}.{table} WHERE project = {{p:String}} AND launch_created_at <= {{at:DateTime64(3, 'UTC')}}"
                    ),
                    &params,
                    None,
                )
                .await
                .with_context(|| format!("deleting a project from {table}"))?;
            }
            self.exec(
                &format!(
                    "DELETE FROM {db}.series_daily WHERE project = {{p:String}} AND day <= toDate({{at:DateTime64(3, 'UTC')}})"
                ),
                &params,
                None,
            )
            .await
            .context("deleting a project from series_daily")?;
        }
        Ok(())
    }
}

/// The stream as its JSON name.
pub fn stream_str(s: LogStream) -> &'static str {
    match s {
        LogStream::Stdout => "stdout",
        LogStream::Stderr => "stderr",
        LogStream::Log => "log",
        LogStream::Step => "step",
    }
}

#[derive(Deserialize)]
struct LogRow {
    lid: Uuid,
    result_id: String,
    attempt: u32,
    seq: u64,
    ts_ms: i64,
    level: String,
    stream: String,
    message: String,
    step: String,
}

#[derive(Deserialize)]
struct SimilarRow {
    rid: Uuid,
    lid: Uuid,
    launch_number: i64,
    name: String,
    message: String,
    score: f64,
}

#[async_trait]
impl SearchBackend for ClickHouse {
    async fn search_logs(&self, project_key: &str, s: &LogSearch) -> anyhow::Result<LogHits> {
        let toks = tokens(&s.q);
        let mut sql = format!(
            "SELECT toString(launch_id) AS lid, result_id, attempt, seq, toUnixTimestamp64Milli(ts) AS ts_ms, level, \
             stream, message, step FROM {}.logs WHERE project = {{p:String}} \
             AND positionCaseInsensitiveUTF8(message, {{q:String}}) > 0",
            self.db()
        );
        let mut params: Vec<(&str, String)> =
            vec![("p", project_key.to_string()), ("q", s.q.clone())];
        if !toks.is_empty() {
            sql.push_str(" AND hasAllTokens(lower(message), {toks:Array(String)})");
            params.push(("toks", serde_json::to_string(&toks)?.replace('"', "'")));
        }
        if let Some(l) = s.launch_id {
            sql.push_str(" AND launch_id = toUUID({launch:String})");
            params.push(("launch", l.to_string()));
        }
        if let Some(f) = s.from {
            sql.push_str(" AND ts >= fromUnixTimestamp64Milli({from:Int64})");
            params.push(("from", f.timestamp_millis().to_string()));
        }
        if let Some(t) = s.to {
            sql.push_str(" AND ts <= fromUnixTimestamp64Milli({to:Int64})");
            params.push(("to", t.timestamp_millis().to_string()));
        }
        if let Some(l) = s.level {
            let levels: Vec<&str> = [
                LogLevel::Trace,
                LogLevel::Debug,
                LogLevel::Info,
                LogLevel::Warn,
                LogLevel::Error,
            ]
            .into_iter()
            .filter(|x| *x >= l)
            .map(LogLevel::as_str)
            .collect();
            sql.push_str(" AND level IN {levels:Array(String)}");
            params.push(("levels", serde_json::to_string(&levels)?.replace('"', "'")));
        }
        sql.push_str(" ORDER BY ts DESC LIMIT 1 BY launch_id, result_id, attempt, seq LIMIT {limit:UInt32} FORMAT JSONEachRow");
        params.push(("limit", s.limit.clamp(1, 1000).to_string()));
        let rows: Vec<LogRow> = self.rows(&sql, &params).await?;
        Ok(LogHits {
            items: rows
                .into_iter()
                .filter_map(|r| {
                    Some(LogEntry {
                        launch_id: r.lid,
                        line: LogLine {
                            result_id: r.result_id,
                            attempt: r.attempt,
                            seq: r.seq,
                            ts: r.ts_ms,
                            level: LogLevel::parse(&r.level)?,
                            stream: serde_json::from_value(serde_json::Value::String(r.stream))
                                .ok()?,
                            message: r.message,
                            step: Some(r.step).filter(|s| !s.is_empty()),
                        },
                    })
                })
                .collect(),
            partial: false,
        })
    }

    async fn similar(
        &self,
        project_key: &str,
        src: &SimilarSource,
        limit: i64,
    ) -> anyhow::Result<Vec<SimilarResult>> {
        let Some(message) = src.message.as_deref().filter(|m| !m.trim().is_empty()) else {
            return Ok(Vec::new());
        };
        let key = match_key(src.fingerprint.as_deref(), message);
        let norm = normalize_message(message);
        let sql = format!(
            "SELECT toString(id) AS rid, toString(launch_id) AS lid, launch_number, name, message, \
             if(match_key = {{key:String}}, 1., 1 - ngramDistanceUTF8(message_norm, {{norm:String}})) AS score \
             FROM {}.results FINAL WHERE project = {{p:String}} AND id != toUUID({{id:String}}) \
             AND status IN ('failed', 'broken') AND message != '' \
             AND (match_key = {{key:String}} OR ngramDistanceUTF8(message_norm, {{norm:String}}) < 0.7) \
             ORDER BY score DESC, launch_created_at DESC LIMIT {{limit:UInt32}} FORMAT JSONEachRow",
            self.db()
        );
        let rows: Vec<SimilarRow> = self
            .rows(
                &sql,
                &[
                    ("key", key),
                    ("norm", norm),
                    ("p", project_key.to_string()),
                    ("id", src.id.to_string()),
                    ("limit", limit.clamp(1, 100).to_string()),
                ],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| SimilarResult {
                result_id: r.rid,
                launch_id: r.lid,
                launch_number: r.launch_number,
                name: r.name,
                message: Some(r.message.chars().take(500).collect()),
                score: (r.score * 1000.0).round() / 1000.0,
                defect: None,
            })
            .collect())
    }

    async fn ping(&self) -> anyhow::Result<()> {
        tokio::time::timeout(Duration::from_secs(5), self.exec("SELECT 1", &[], None))
            .await
            .context("ClickHouse did not answer")??;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tokens_split_like_the_index() {
        assert_eq!(
            super::tokens("Connection REFUSED: db-1"),
            vec!["connection", "refused", "db", "1"]
        );
        assert!(super::tokens("  ::  ").is_empty());
        assert_eq!(super::ts_ms(-5), "1970-01-01 00:00:00.000");
    }

    #[test]
    fn params_are_escaped() {
        assert_eq!(super::escape_param("a\\b"), "a\\\\b");
        assert_eq!(super::escape_param("x\ty\nz"), "x\\ty\\nz");
        assert_eq!(super::escape_param("plain 'text'"), "plain 'text'");
    }
}
