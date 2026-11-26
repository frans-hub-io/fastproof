//! ClickHouse over its HTTP interface (`JSONEachRow` inserts and reads, parameterized
//! queries): the sink tables, the `series_daily` rollup, the search queries and, from schema
//! version 2, the tables and reads of FastProof Analysis (contract `76` §7.9).

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, bail};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use fastproof_store::analysis::types::{
    CrossProjectSimilarItem, DurationRegression, ErrorTimelinePoint, FailureGroupBy,
    FlakyTrendPoint, LogMatch, LogStreamName, MemoryRow, RetryCost, RuleLogMatch, SeriesRun,
    SlowStep,
};
use fastproof_store::logs::{LogEntry, LogHits, LogLevel, LogLine, LogSearch, LogStream};
use fastproof_store::stream::{SimilarResult, SimilarSource, normalize_message};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::config::ClickHouseConfig;
use crate::http::{HttpBackend, error_text};
use crate::sink::{Deletions, SearchBackend, Sink, SinkBatch, flatten_steps, match_key};

/// Settings of every read query: a time limit and a memory limit per query, 64-bit integers
/// as JSON numbers, read-only, and cancelled when the caller goes away (a dropped request,
/// such as an analytics call whose client disconnected, must not keep running on the server
/// after its heavy-query slot was released).
const READ_SETTINGS: [(&str, &str); 6] = [
    ("max_execution_time", "30"),
    ("timeout_overflow_mode", "throw"),
    ("max_memory_usage", "2147483648"),
    ("output_format_json_quote_64bit_integers", "0"),
    ("readonly", "2"),
    ("cancel_http_readonly_queries_on_client_close", "1"),
];

/// Settings of the rollups: the read limits with a longer time limit, still below the HTTP
/// client's timeout (120 s), so a statement never outlives the request that started it.
const ROLLUP_SETTINGS: [(&str, &str); 3] = [
    ("max_execution_time", "110"),
    ("timeout_overflow_mode", "throw"),
    ("max_memory_usage", "2147483648"),
];

/// Bytes of one ClickHouse answer read into memory, at most (larger answers fail).
pub const MAX_ANSWER_BYTES: usize = 128 << 20;

/// The schema version this build writes and reads (component `fastproof` of `schema_version`).
pub const SCHEMA_VERSION: u32 = 2;
/// The component of `schema_version` this crate owns.
pub const SCHEMA_COMPONENT: &str = "fastproof";

/// A ClickHouse array literal of strings for an `Array(String)` (or `Array(UUID)`, `Array(Date)`)
/// query parameter: each item quoted with `\`, `'` and control characters escaped, so no item
/// can end the literal. Array parameters are sent as they are (not in the escaped format).
pub fn str_array<S: AsRef<str>>(items: &[S]) -> String {
    let mut out = String::from("[");
    for (i, s) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('\'');
        for c in s.as_ref().chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\'' => out.push_str("\\'"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\0' => out.push_str("\\0"),
                c => out.push(c),
            }
        }
        out.push('\'');
    }
    out.push(']');
    out
}

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
            // An array parameter is read as a quoted literal (see [`str_array`]); every other
            // type in the escaped format.
            let value = if sql.contains(&format!("{{{k}:Array("))
                || sql.contains(&format!("{{{k}: Array("))
            {
                v.clone()
            } else {
                escape_param(v)
            };
            q.push((format!("param_{k}"), value));
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
        let mut r = rb.send().await.context("ClickHouse request failed")?;
        if !r.status().is_success() {
            bail!("ClickHouse: {}", error_text(r).await);
        }
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = r.chunk().await? {
            if buf.len() + chunk.len() > MAX_ANSWER_BYTES {
                bail!(
                    "the ClickHouse answer is larger than {} MiB",
                    MAX_ANSWER_BYTES >> 20
                );
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(match String::from_utf8(buf) {
            Ok(t) => t,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        })
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

    /// The `schema_version` table (component, version, applied at).
    fn schema_version_table(&self) -> String {
        format!(
            "CREATE TABLE IF NOT EXISTS {}.schema_version (component LowCardinality(String), version UInt32, \
             applied_at DateTime64(3, 'UTC')) ENGINE = ReplacingMergeTree(applied_at) ORDER BY component",
            self.db()
        )
    }

    /// The statements of schema version `v` (each idempotent).
    fn schema_step(&self, v: u32) -> Vec<String> {
        let db = self.db();
        let agg = self.cfg.agg_ttl_days;
        match v {
            2 => vec![
                format!(
                    "ALTER TABLE {db}.results ADD COLUMN IF NOT EXISTS defect_type_id Nullable(UUID), \
                     ADD COLUMN IF NOT EXISTS defect_group LowCardinality(String) DEFAULT '', \
                     ADD COLUMN IF NOT EXISTS defect_source LowCardinality(String) DEFAULT '', \
                     ADD COLUMN IF NOT EXISTS muted Bool DEFAULT false, \
                     ADD COLUMN IF NOT EXISTS canonical_fingerprint String DEFAULT '', \
                     ADD COLUMN IF NOT EXISTS suite String DEFAULT ''"
                ),
                format!(
                    "ALTER TABLE {db}.attempts ADD COLUMN IF NOT EXISTS fingerprint String DEFAULT '', \
                     ADD COLUMN IF NOT EXISTS frames Array(String) DEFAULT [], \
                     ADD COLUMN IF NOT EXISTS trace_id String DEFAULT ''"
                ),
                format!(
                    "CREATE TABLE IF NOT EXISTS {db}.triage (project LowCardinality(String), project_id UUID, \
                     launch_id UUID, launch_created_at DateTime64(3, 'UTC'), result_id UUID, decision_id Int64, \
                     batch_id UUID, fingerprint String, via LowCardinality(String), source LowCardinality(String), \
                     by String, at DateTime64(3, 'UTC'), before_type_id Nullable(UUID), \
                     before_group LowCardinality(String), after_type_id Nullable(UUID), \
                     after_group LowCardinality(String), message_norm String, test_case_id UUID, branch String, \
                     env_key String, reverts_batch Nullable(UUID), version UInt64) \
                     ENGINE = ReplacingMergeTree(version) PARTITION BY toYYYYMM(at) \
                     ORDER BY (project, launch_id, result_id, decision_id) \
                     TTL toDateTime(at) + INTERVAL {agg} DAY SETTINGS non_replicated_deduplication_window = 1000"
                ),
                format!(
                    "CREATE TABLE IF NOT EXISTS {db}.fingerprint_daily (project LowCardinality(String), \
                     fingerprint String, day Date, branch String, env_key String, \
                     occurrences AggregateFunction(uniq, UUID), tests AggregateFunction(uniq, UUID), \
                     launches AggregateFunction(uniq, UUID)) \
                     ENGINE = AggregatingMergeTree PARTITION BY toYYYYMM(day) \
                     ORDER BY (project, fingerprint, day, branch, env_key) TTL day + INTERVAL {agg} DAY"
                ),
                format!(
                    "CREATE MATERIALIZED VIEW IF NOT EXISTS {db}.fingerprint_daily_mv TO {db}.fingerprint_daily AS \
                     SELECT project, fingerprint, toDate(launch_created_at) AS day, branch, env_key, \
                     uniqState(id) AS occurrences, uniqState(test_case_id) AS tests, uniqState(launch_id) AS launches \
                     FROM {db}.results WHERE fingerprint != '' AND status IN ('failed', 'broken') \
                     GROUP BY project, fingerprint, day, branch, env_key"
                ),
                format!(
                    "CREATE TABLE IF NOT EXISTS {db}.defect_daily (project LowCardinality(String), day Date, \
                     branch String, env_key String, grp LowCardinality(String), results UInt64, version UInt64) \
                     ENGINE = ReplacingMergeTree(version) PARTITION BY toYYYYMM(day) \
                     ORDER BY (project, day, branch, env_key, grp) TTL day + INTERVAL {agg} DAY"
                ),
            ],
            _ => Vec::new(),
        }
    }

    /// The schema version stored for component `fastproof` (0 before versioning).
    pub async fn schema_version(&self) -> anyhow::Result<u32> {
        #[derive(Deserialize)]
        struct V {
            v: u32,
        }
        let rows: Vec<V> = self
            .rows(
                &format!(
                    "SELECT max(version) AS v FROM {}.schema_version FINAL WHERE component = {{c:String}} \
                     FORMAT JSONEachRow",
                    self.db()
                ),
                &[("c", SCHEMA_COMPONENT.to_string())],
            )
            .await?;
        Ok(rows.first().map_or(0, |r| r.v))
    }

    /// Runs the steps above the stored version, recording each after it succeeded.
    async fn upgrade(&self) -> anyhow::Result<()> {
        self.exec(&self.schema_version_table(), &[], None)
            .await
            .context("creating schema_version")?;
        let stored = self.schema_version().await?;
        for v in (stored + 1)..=SCHEMA_VERSION {
            for sql in self.schema_step(v) {
                self.exec(&sql, &[], None).await.with_context(|| {
                    format!(
                        "ClickHouse schema version {v}: {}",
                        sql.chars().take(80).collect::<String>()
                    )
                })?;
            }
            self.exec(
                &format!(
                    "INSERT INTO {}.schema_version (component, version, applied_at) \
                     VALUES ({{c:String}}, {{v:UInt32}}, now64(3))",
                    self.db()
                ),
                &[("c", SCHEMA_COMPONENT.to_string()), ("v", v.to_string())],
                None,
            )
            .await
            .with_context(|| format!("recording ClickHouse schema version {v}"))?;
            tracing::info!(version = v, "ClickHouse schema upgraded");
        }
        Ok(())
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
        self.upgrade().await
    }

    fn wants_triage(&self) -> bool {
        true
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
                    "version": r.updated_at.timestamp_micros().max(0) as u64,
                    "defect_type_id": r.defect_type_id.map(uuid_s), "defect_group": r.defect_group,
                    "defect_source": r.defect_source, "muted": r.muted,
                    "canonical_fingerprint": r.canonical_fingerprint.clone().unwrap_or_default(),
                    "suite": r.suite})
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
                "trace": a.trace.clone().unwrap_or_default(), "version": version,
                "fingerprint": a.fingerprint.clone().unwrap_or_default(), "frames": a.frames,
                "trace_id": a.trace_id.clone().unwrap_or_default()}));
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
        let triage: Vec<serde_json::Value> = b
            .triage
            .iter()
            .map(|t| {
                json!({"project": t.project, "project_id": uuid_s(t.project_id),
                    "launch_id": uuid_s(t.launch_id), "launch_created_at": ts(t.launch_created_at),
                    "result_id": uuid_s(t.result_id), "decision_id": t.decision_id,
                    "batch_id": uuid_s(t.batch_id), "fingerprint": t.fingerprint, "via": t.via,
                    "source": t.source, "by": t.by, "at": ts(t.at),
                    "before_type_id": t.before_type_id.map(uuid_s), "before_group": t.before_group,
                    "after_type_id": t.after_type_id.map(uuid_s), "after_group": t.after_group,
                    "message_norm": t.message_norm, "test_case_id": uuid_s(t.test_case_id),
                    "branch": t.branch, "env_key": t.env_key, "reverts_batch": t.reverts_batch.map(uuid_s),
                    "version": t.decision_id.max(0) as u64})
            })
            .collect();
        self.insert("results", &results, token).await?;
        self.insert("attempts", &attempts, token).await?;
        self.insert("steps", &steps, token).await?;
        self.insert("logs", &logs, token).await?;
        self.insert("triage", &triage, token).await?;
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
                let ids = str_array(part);
                let days = self.launch_days(project, &ids).await?;
                for table in ["results", "attempts", "steps", "logs", "triage"] {
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
                // The raw rows are gone; the aggregates of their days are rebuilt best effort:
                // a failure here must not hold the sink (and every other project) on this batch.
                if let Err(e) = self.rebuild_days(project, &days).await {
                    metrics::counter!("fastproof_clickhouse_rebuild_errors_total").increment(1);
                    tracing::warn!(project, error = %format!("{e:#}"), "rebuilding the daily aggregates of deleted launches failed");
                }
            }
        }
        for (project, at) in &d.projects {
            let params = [("p", project.clone()), ("at", ts(*at))];
            self.exec(
                &format!(
                    "DELETE FROM {db}.triage WHERE project = {{p:String}} AND launch_created_at <= {{at:DateTime64(3, 'UTC')}}"
                ),
                &params,
                None,
            )
            .await
            .context("deleting a project from triage")?;
            for table in ["fingerprint_daily", "defect_daily"] {
                self.exec(
                    &format!(
                        "DELETE FROM {db}.{table} WHERE project = {{p:String}} AND day <= toDate({{at:DateTime64(3, 'UTC')}})"
                    ),
                    &params,
                    None,
                )
                .await
                .with_context(|| format!("deleting a project from {table}"))?;
            }
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

/// The analytics filters of one project: the window `[from, to)` and an optional launch
/// branch and environment.
#[derive(Clone, Debug)]
pub struct Scope<'a> {
    pub project: &'a str,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub branch: Option<&'a str>,
    pub env: Option<&'a str>,
}

type Params = Vec<(&'static str, String)>;

impl Scope<'_> {
    /// The `WHERE` conditions of the scope on a table with `project`, `launch_created_at` and
    /// (with `launch_columns`) `branch` and `env_key`; values go to `params`.
    fn sql(&self, params: &mut Params, launch_columns: bool) -> String {
        params.push(("p", self.project.to_string()));
        params.push(("from", ts(self.from)));
        params.push(("to", ts(self.to)));
        let mut w = String::from(
            "project = {p:String} AND launch_created_at >= {from:DateTime64(3, 'UTC')} \
             AND launch_created_at < {to:DateTime64(3, 'UTC')}",
        );
        if launch_columns {
            if let Some(b) = self.branch {
                w.push_str(" AND branch = {branch:String}");
                params.push(("branch", b.to_string()));
            }
            if let Some(e) = self.env {
                w.push_str(" AND env_key = {env:String}");
                params.push(("env", e.to_string()));
            }
        }
        w
    }
}

/// Aliases of a project (`fingerprint` → the error it is merged into), at most this many are
/// sent with a query; beyond that the `canonical_fingerprint` written with each result is used.
pub const MAX_ALIASES: usize = 10_000;

/// Characters of a matched log line sent back for a retroactive log rule (the excerpt of a
/// pattern mark is at most this long).
pub const LOG_MATCH_CHARS: usize = 300;

/// Days of `defect_daily` the hourly rollup recomputes.
pub const ROLLUP_DAYS: u32 = 14;

/// A group of failures as ClickHouse counts it (the label is added by the caller).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct GroupCount {
    pub key: String,
    pub failures: i64,
    pub tests: i64,
    pub launches: i64,
}

/// Per-project numbers of the portfolio that ClickHouse counts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct PortfolioCounts {
    pub project: String,
    pub launches: i64,
    pub passed: i64,
    pub ran: i64,
    pub failures: i64,
    pub flaky: i64,
}

#[derive(Deserialize)]
struct KeyDay {
    key: String,
    day: NaiveDate,
    n: i64,
}

#[derive(Deserialize)]
struct Total {
    n: i64,
}

#[derive(Deserialize)]
struct TimelineRow {
    day: NaiveDate,
    occurrences: i64,
    launches: i64,
}

#[derive(Deserialize)]
struct HistoryRow {
    fingerprint: String,
    type_id: Uuid,
    grp: String,
    msg: String,
    tcs: Vec<Uuid>,
    envs: Vec<String>,
    brs: Vec<String>,
    sample_result: Uuid,
    sample_launch: Uuid,
    confirmed: i64,
    first_ms: i64,
    last_ms: i64,
    last_by: String,
}

#[derive(Deserialize)]
struct MatchRow {
    lid: Uuid,
    result_id: String,
    attempt: u32,
    seq: u64,
    message: String,
}

#[derive(Deserialize)]
struct RunRow {
    rid: Uuid,
    lid: Uuid,
    at_ms: i64,
    status: String,
    flaky_in_run: bool,
    duration_ms: Option<i64>,
    env_key: String,
    branch: String,
}

#[derive(Deserialize)]
struct FpsRow {
    rid: Uuid,
    fps: Vec<String>,
}

#[derive(Deserialize)]
struct DurationRow {
    sid: Uuid,
    tc: Uuid,
    name: String,
    p50r: i64,
    p50b: i64,
    runs: i64,
}

#[derive(Deserialize)]
struct RetryRow {
    suite: String,
    wasted_ms: i64,
    retries: i64,
    tests: i64,
}

#[derive(Deserialize)]
struct FlakyRow {
    day: NaiveDate,
    flaky: i64,
    runs: i64,
}

#[derive(Deserialize)]
struct StepRow {
    name: String,
    p50: i64,
    p95: i64,
    n: i64,
}

#[derive(Deserialize)]
struct CrossRow {
    project: String,
    rid: Uuid,
    lid: Uuid,
    launch_number: i64,
    name: String,
    message: String,
    score: f64,
    grp: String,
}

#[derive(Deserialize)]
struct DayRow {
    day: NaiveDate,
}

fn ms_time(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_default()
}

fn days_of(from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<NaiveDate> {
    let mut out = Vec::new();
    let mut d = from.date_naive();
    let last = to.date_naive();
    while d <= last && out.len() <= 3660 {
        out.push(d);
        d += chrono::Duration::days(1);
    }
    out
}

impl ClickHouse {
    /// The failure grouping expression; `aliases` canonicalizes merged fingerprints.
    fn group_key(by: FailureGroupBy, aliases: &[(String, String)], params: &mut Params) -> String {
        match by {
            FailureGroupBy::Fingerprint if aliases.is_empty() || aliases.len() > MAX_ALIASES => {
                "if(canonical_fingerprint != '', canonical_fingerprint, fingerprint)".into()
            }
            FailureGroupBy::Fingerprint => {
                params.push((
                    "af",
                    str_array(&aliases.iter().map(|a| a.0.as_str()).collect::<Vec<_>>()),
                ));
                params.push((
                    "at",
                    str_array(&aliases.iter().map(|a| a.1.as_str()).collect::<Vec<_>>()),
                ));
                "transform(fingerprint, {af:Array(String)}, {at:Array(String)}, fingerprint)".into()
            }
            FailureGroupBy::DefectGroup => {
                "if(defect_group = '', 'toInvestigate', defect_group)".into()
            }
            FailureGroupBy::Suite => "suite".into(),
            FailureGroupBy::Env => "env_key".into(),
        }
    }

    /// Failures of a project grouped by `by`: the top `limit` groups (most failures first), the
    /// failures of every group per day (for the trend) and the total. A result counts once
    /// (rows written again after a triage are folded with `FINAL` where the group can change).
    pub async fn failures(
        &self,
        sc: &Scope<'_>,
        by: FailureGroupBy,
        aliases: &[(String, String)],
        limit: usize,
    ) -> anyhow::Result<(i64, Vec<GroupCount>, Vec<(String, NaiveDate, i64)>)> {
        let db = self.db();
        let mut params = Params::new();
        let w = sc.sql(&mut params, true);
        let key = Self::group_key(by, aliases, &mut params);
        let fin = if by == FailureGroupBy::DefectGroup {
            " FINAL"
        } else {
            ""
        };
        let only_fp = if by == FailureGroupBy::Fingerprint {
            " AND fingerprint != ''"
        } else {
            ""
        };
        let total: Vec<Total> = self
            .rows(
                &format!(
                    "SELECT uniqExact(id) AS n FROM {db}.results WHERE {w} AND status IN ('failed', 'broken') \
                     FORMAT JSONEachRow"
                ),
                &params,
            )
            .await?;
        let mut p2 = params.clone();
        p2.push(("limit", limit.clamp(1, 1000).to_string()));
        let groups: Vec<GroupCount> = self
            .rows(
                &format!(
                    "SELECT {key} AS key, uniqExact(id) AS failures, uniqExact(test_case_id) AS tests, \
                     uniqExact(launch_id) AS launches FROM {db}.results{fin} WHERE {w}{only_fp} \
                     AND status IN ('failed', 'broken') GROUP BY key ORDER BY failures DESC, key \
                     LIMIT {{limit:UInt32}} FORMAT JSONEachRow"
                ),
                &p2,
            )
            .await?;
        let keys: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
        let days: Vec<KeyDay> = if keys.is_empty() {
            Vec::new()
        } else if by == FailureGroupBy::DefectGroup {
            let mut p3 = Params::new();
            let mut w3 = String::from(
                "project = {p:String} AND day >= toDate({from:DateTime64(3, 'UTC')}) \
                 AND day <= toDate({to:DateTime64(3, 'UTC')} - INTERVAL 1 MILLISECOND)",
            );
            p3.push(("p", sc.project.to_string()));
            p3.push(("from", ts(sc.from)));
            p3.push(("to", ts(sc.to)));
            if let Some(b) = sc.branch {
                w3.push_str(" AND branch = {branch:String}");
                p3.push(("branch", b.to_string()));
            }
            if let Some(e) = sc.env {
                w3.push_str(" AND env_key = {env:String}");
                p3.push(("env", e.to_string()));
            }
            p3.push(("keys", str_array(&keys)));
            self.rows(
                &format!(
                    "SELECT grp AS key, day, toInt64(sum(results)) AS n FROM {db}.defect_daily FINAL \
                     WHERE {w3} AND grp IN {{keys:Array(String)}} GROUP BY key, day FORMAT JSONEachRow"
                ),
                &p3,
            )
            .await?
        } else {
            let mut p3 = params.clone();
            p3.push(("keys", str_array(&keys)));
            self.rows(
                &format!(
                    "SELECT {key} AS key, toDate(launch_created_at) AS day, uniqExact(id) AS n \
                     FROM {db}.results{fin} WHERE {w}{only_fp} AND status IN ('failed', 'broken') \
                     AND key IN {{keys:Array(String)}} GROUP BY key, day FORMAT JSONEachRow"
                ),
                &p3,
            )
            .await?
        };
        Ok((
            total.first().map_or(0, |t| t.n),
            groups,
            days.into_iter().map(|d| (d.key, d.day, d.n)).collect(),
        ))
    }

    /// Series whose median duration of the last 7 days of the window is at least 1.3 times
    /// the median before, with at least 10 runs in each part (passed, failed or broken runs
    /// with a duration).
    pub async fn durations(
        &self,
        sc: &Scope<'_>,
        recent_from: DateTime<Utc>,
        limit: usize,
    ) -> anyhow::Result<Vec<DurationRegression>> {
        let db = self.db();
        let mut params = Params::new();
        let w = sc.sql(&mut params, true);
        params.push(("mid", ts(recent_from)));
        params.push(("limit", limit.clamp(1, 1000).to_string()));
        let rows: Vec<DurationRow> = self
            .rows(
                &format!(
                    "SELECT series_id AS sid, any(test_case_id) AS tc, argMax(name, launch_created_at) AS name, \
                     toInt64(quantileExactLowIf(0.5)(assumeNotNull(duration_ms), \
                       launch_created_at >= {{mid:DateTime64(3, 'UTC')}})) AS p50r, \
                     toInt64(quantileExactLowIf(0.5)(assumeNotNull(duration_ms), \
                       launch_created_at < {{mid:DateTime64(3, 'UTC')}})) AS p50b, \
                     countIf(launch_created_at >= {{mid:DateTime64(3, 'UTC')}}) AS rr, \
                     countIf(launch_created_at < {{mid:DateTime64(3, 'UTC')}}) AS rb, \
                     toInt64(count()) AS runs \
                     FROM {db}.results FINAL WHERE {w} AND status IN ('passed', 'failed', 'broken') \
                     AND duration_ms IS NOT NULL GROUP BY series_id \
                     HAVING rr >= 10 AND rb >= 10 AND p50b > 0 AND p50r * 10 >= p50b * 13 \
                     ORDER BY p50r / p50b DESC, sid LIMIT {{limit:UInt32}} FORMAT JSONEachRow"
                ),
                &params,
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| DurationRegression {
                series_id: r.sid,
                test_case_id: r.tc,
                name: r.name,
                p50_recent_ms: r.p50r,
                p50_before_ms: r.p50b,
                ratio: ratio(r.p50r, r.p50b),
                runs: r.runs,
            })
            .collect())
    }

    /// Time spent on attempts that did not become final (all but the newest attempt of each
    /// result), per suite: the total and the top `limit` suites.
    pub async fn retries(
        &self,
        sc: &Scope<'_>,
        limit: usize,
    ) -> anyhow::Result<(i64, Vec<RetryCost>)> {
        let db = self.db();
        let mut params = Params::new();
        let wa = sc.sql(&mut params, false);
        let mut p2 = Params::new();
        let wr = sc.sql(&mut p2, true);
        for (k, v) in p2 {
            if !params.iter().any(|(n, _)| *n == k) {
                params.push((k, v));
            }
        }
        let inner = format!(
            "SELECT r.suite AS suite, a.wasted AS wasted, a.retries AS retries, r.test_case_id AS tc \
             FROM (SELECT result_id, toInt64(sum(ifNull(duration_ms, 0)) - argMax(ifNull(duration_ms, 0), seq)) AS wasted, \
               toInt64(count() - 1) AS retries FROM {db}.attempts FINAL WHERE {wa} GROUP BY result_id \
               HAVING count() > 1) AS a \
             INNER JOIN (SELECT id, suite, test_case_id FROM {db}.results FINAL WHERE {wr}) AS r ON r.id = a.result_id"
        );
        let total: Vec<Total> = self
            .rows(
                &format!("SELECT toInt64(sum(wasted)) AS n FROM ({inner}) FORMAT JSONEachRow"),
                &params,
            )
            .await?;
        params.push(("limit", limit.clamp(1, 1000).to_string()));
        let rows: Vec<RetryRow> = self
            .rows(
                &format!(
                    "SELECT suite, toInt64(sum(wasted)) AS wasted_ms, toInt64(sum(retries)) AS retries, \
                     toInt64(uniqExact(tc)) AS tests FROM ({inner}) GROUP BY suite \
                     ORDER BY wasted_ms DESC, suite LIMIT {{limit:UInt32}} FORMAT JSONEachRow"
                ),
                &params,
            )
            .await?;
        Ok((
            total.first().map_or(0, |t| t.n),
            rows.into_iter()
                .map(|r| RetryCost {
                    suite: r.suite,
                    wasted_ms: r.wasted_ms,
                    retries: r.retries,
                    tests: r.tests,
                })
                .collect(),
        ))
    }

    /// Flaky results and results that ran, per day of the window (days without runs are 0).
    pub async fn flaky_trend(&self, sc: &Scope<'_>) -> anyhow::Result<Vec<FlakyTrendPoint>> {
        let db = self.db();
        let mut params = Params::new();
        let w = sc.sql(&mut params, true);
        let rows: Vec<FlakyRow> = self
            .rows(
                &format!(
                    "SELECT toDate(launch_created_at) AS day, uniqExactIf(id, flaky_in_run) AS flaky, \
                     uniqExactIf(id, status NOT IN ('skipped', 'running')) AS runs FROM {db}.results \
                     WHERE {w} GROUP BY day ORDER BY day FORMAT JSONEachRow"
                ),
                &params,
            )
            .await?;
        let by_day: HashMap<NaiveDate, (i64, i64)> = rows
            .into_iter()
            .map(|r| (r.day, (r.flaky, r.runs)))
            .collect();
        Ok(days_of(sc.from, sc.to - chrono::Duration::milliseconds(1))
            .into_iter()
            .map(|day| {
                let (flaky, runs) = by_day.get(&day).copied().unwrap_or((0, 0));
                FlakyTrendPoint { day, flaky, runs }
            })
            .collect())
    }

    /// The slowest step names by their 95th percentile duration.
    pub async fn slow_steps(&self, sc: &Scope<'_>, limit: usize) -> anyhow::Result<Vec<SlowStep>> {
        let db = self.db();
        let mut params = Params::new();
        let w = sc.sql(&mut params, false);
        params.push(("limit", limit.clamp(1, 1000).to_string()));
        let rows: Vec<StepRow> = self
            .rows(
                &format!(
                    "SELECT name, toInt64(quantileExactLow(0.5)(d)) AS p50, toInt64(quantileExactLow(0.95)(d)) AS p95, \
                     toInt64(count()) AS n FROM (SELECT name, assumeNotNull(duration_ms) AS d FROM {db}.steps FINAL \
                     WHERE {w} AND duration_ms IS NOT NULL) GROUP BY name ORDER BY p95 DESC, name \
                     LIMIT {{limit:UInt32}} FORMAT JSONEachRow"
                ),
                &params,
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| SlowStep {
                name: r.name,
                p50_ms: r.p50,
                p95_ms: r.p95,
                count: r.n,
            })
            .collect())
    }

    /// Daily occurrences and launches of an error and its merged fingerprints, from
    /// `fingerprint_daily` (whole days; days without occurrences are 0).
    pub async fn error_timeline(
        &self,
        project: &str,
        fingerprints: &[String],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> anyhow::Result<Vec<ErrorTimelinePoint>> {
        let rows: Vec<TimelineRow> = if fingerprints.is_empty() {
            Vec::new()
        } else {
            self.rows(
                &format!(
                    "SELECT day, toInt64(uniqMerge(occurrences)) AS occurrences, \
                     toInt64(uniqMerge(launches)) AS launches FROM {}.fingerprint_daily \
                     WHERE project = {{p:String}} AND fingerprint IN {{fps:Array(String)}} \
                     AND day >= toDate({{from:DateTime64(3, 'UTC')}}) AND day <= toDate({{to:DateTime64(3, 'UTC')}}) \
                     GROUP BY day ORDER BY day FORMAT JSONEachRow",
                    self.db()
                ),
                &[
                    ("p", project.to_string()),
                    ("fps", str_array(fingerprints)),
                    ("from", ts(from)),
                    ("to", ts(to)),
                ],
            )
            .await?
        };
        let by_day: HashMap<NaiveDate, (i64, i64)> = rows
            .into_iter()
            .map(|r| (r.day, (r.occurrences, r.launches)))
            .collect();
        Ok(days_of(from, to)
            .into_iter()
            .map(|day| {
                let (occurrences, launches) = by_day.get(&day).copied().unwrap_or((0, 0));
                ErrorTimelinePoint {
                    day,
                    occurrences,
                    launches,
                }
            })
            .collect())
    }

    /// People's decisions kept in the `triage` history for these fingerprints, folded like
    /// the memory (one row per fingerprint and defect type: confirmations are batches); at
    /// most `per_fingerprint` rows per fingerprint, the most recent first. Rule, undo and
    /// `toInvestigate` decisions teach nothing; batches an undo reverted are left out.
    pub async fn history(
        &self,
        project: &str,
        fingerprints: &[String],
        per_fingerprint: usize,
    ) -> anyhow::Result<Vec<MemoryRow>> {
        if fingerprints.is_empty() {
            return Ok(Vec::new());
        }
        let db = self.db();
        let rows: Vec<HistoryRow> = self
            .rows(
                &format!(
                    "SELECT fingerprint, assumeNotNull(after_type_id) AS type_id, any(after_group) AS grp, \
                     argMax(message_norm, at) AS msg, groupUniqArray(20)(test_case_id) AS tcs, \
                     groupUniqArrayIf(10)(env_key, env_key != '') AS envs, \
                     groupUniqArrayIf(10)(branch, branch != '') AS brs, argMax(result_id, at) AS sample_result, \
                     argMax(launch_id, at) AS sample_launch, toInt64(uniqExact(batch_id)) AS confirmed, \
                     toUnixTimestamp64Milli(min(at)) AS first_ms, toUnixTimestamp64Milli(max(at)) AS last_ms, \
                     argMax(by, at) AS last_by FROM {db}.triage \
                     WHERE project = {{p:String}} AND fingerprint IN {{fps:Array(String)}} AND source = 'user' \
                     AND via NOT IN ('rule', 'undo') AND after_type_id IS NOT NULL \
                     AND after_group NOT IN ('', 'toInvestigate') \
                     AND batch_id NOT IN (SELECT assumeNotNull(reverts_batch) FROM {db}.triage \
                       WHERE project = {{p:String}} AND via = 'undo' AND reverts_batch IS NOT NULL) \
                     GROUP BY fingerprint, type_id ORDER BY fingerprint, last_ms DESC, type_id \
                     LIMIT {{per:UInt32}} BY fingerprint FORMAT JSONEachRow"
                ),
                &[
                    ("p", project.to_string()),
                    ("fps", str_array(fingerprints)),
                    ("per", per_fingerprint.clamp(1, 20).to_string()),
                ],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| MemoryRow {
                fingerprint: r.fingerprint,
                defect_type_id: r.type_id,
                group: r.grp,
                message_norm: r.msg,
                frames: Vec::new(),
                test_case_ids: r.tcs.into_iter().filter(|t| !t.is_nil()).collect(),
                env_keys: r.envs,
                branches: r.brs,
                comment: None,
                issue_url: None,
                issue_key: None,
                sample_result_id: Some(r.sample_result),
                sample_launch_id: Some(r.sample_launch),
                confirmed: r.confirmed.clamp(0, i64::from(i32::MAX)) as i32,
                overridden: 0,
                first_decided_at: ms_time(r.first_ms),
                last_decided_at: ms_time(r.last_ms),
                last_decided_by: r.last_by,
            })
            .collect())
    }

    /// Log lines of the listed launches of the project since `since` that a log rule's
    /// `match.log` matches (RE2 `match` for `regex`, a case-insensitive search for `contains`,
    /// with its levels and streams): the newest matching line of each attempt, newest launches
    /// first, at most `max`, each cut to [`LOG_MATCH_CHARS`] characters. Only the listed
    /// launches are read (no launches: no matches), so the scan and the answer follow the
    /// task, not the project's whole log history.
    pub async fn log_matches(
        &self,
        project: &str,
        m: &RuleLogMatch,
        since: DateTime<Utc>,
        launches: &[Uuid],
        max: usize,
    ) -> anyhow::Result<Vec<LogMatch>> {
        if launches.is_empty() {
            return Ok(Vec::new());
        }
        let db = self.db();
        let mut params: Params = vec![
            ("p", project.to_string()),
            ("since", ts(since)),
            (
                "ls",
                str_array(&launches.iter().map(Uuid::to_string).collect::<Vec<_>>()),
            ),
        ];
        let mut w = String::from(
            "project = {p:String} AND launch_id IN {ls:Array(UUID)} \
             AND launch_created_at >= {since:DateTime64(3, 'UTC')}",
        );
        match (&m.regex, &m.contains) {
            (Some(re), _) => {
                w.push_str(" AND match(message, {re:String})");
                params.push(("re", re.clone()));
            }
            (None, Some(c)) => {
                w.push_str(" AND positionCaseInsensitiveUTF8(message, {c:String}) > 0");
                params.push(("c", c.clone()));
            }
            (None, None) => return Ok(Vec::new()),
        }
        if let Some(levels) = m.levels.as_ref().filter(|l| !l.is_empty()) {
            w.push_str(" AND level IN {levels:Array(String)}");
            params.push((
                "levels",
                str_array(&levels.iter().map(|l| l.as_str()).collect::<Vec<_>>()),
            ));
        }
        if let Some(streams) = m.streams.as_ref().filter(|s| !s.is_empty()) {
            w.push_str(" AND stream IN {streams:Array(String)}");
            params.push((
                "streams",
                str_array(
                    &streams
                        .iter()
                        .map(|s| match s {
                            LogStreamName::Stdout => "stdout",
                            LogStreamName::Stderr => "stderr",
                            LogStreamName::Log => "log",
                            LogStreamName::Step => "step",
                        })
                        .collect::<Vec<_>>(),
                ),
            ));
        }
        params.push(("max", max.clamp(1, 100_000).to_string()));
        params.push(("chars", LOG_MATCH_CHARS.to_string()));
        let rows: Vec<MatchRow> = self
            .rows(
                &format!(
                    "SELECT launch_id AS lid, result_id, attempt, seq, \
                     substringUTF8(message, 1, {{chars:UInt32}}) AS message \
                     FROM {db}.logs WHERE {w} \
                     ORDER BY launch_created_at DESC, launch_id, result_id, attempt, seq DESC \
                     LIMIT 1 BY launch_id, result_id, attempt LIMIT {{max:UInt32}} FORMAT JSONEachRow"
                ),
                &params,
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| LogMatch {
                launch_id: r.lid,
                result_key: r.result_id,
                attempt: r.attempt,
                seq: r.seq,
                excerpt: r.message,
            })
            .collect())
    }

    /// Runs of a series since `since`, oldest first (at most the newest `max`), with the
    /// fingerprints of each run's attempts in attempt order. Labels (host, thread, CI runner)
    /// and signals are not kept in ClickHouse.
    pub async fn series_runs(
        &self,
        project: &str,
        series_id: Uuid,
        since: DateTime<Utc>,
        max: usize,
    ) -> anyhow::Result<Vec<SeriesRun>> {
        let db = self.db();
        let params: Params = vec![
            ("p", project.to_string()),
            ("s", series_id.to_string()),
            ("since", ts(since)),
            ("max", max.clamp(1, 100_000).to_string()),
        ];
        let runs: Vec<RunRow> = self
            .rows(
                &format!(
                    "SELECT id AS rid, launch_id AS lid, toUnixTimestamp64Milli(launch_created_at) AS at_ms, status, \
                     flaky_in_run, duration_ms, env_key, branch FROM {db}.results FINAL \
                     WHERE project = {{p:String}} AND series_id = {{s:UUID}} \
                     AND launch_created_at >= {{since:DateTime64(3, 'UTC')}} AND status != 'running' \
                     ORDER BY launch_created_at DESC, id LIMIT {{max:UInt32}} FORMAT JSONEachRow"
                ),
                &params,
            )
            .await?;
        let fps: Vec<FpsRow> = if runs.is_empty() {
            Vec::new()
        } else {
            self.rows(
                &format!(
                    "SELECT result_id AS rid, arrayMap(x -> x.2, arraySort(groupArray((seq, fingerprint)))) AS fps \
                     FROM {db}.attempts FINAL WHERE project = {{p:String}} AND series_id = {{s:UUID}} \
                     AND launch_created_at >= {{since:DateTime64(3, 'UTC')}} AND fingerprint != '' \
                     GROUP BY result_id FORMAT JSONEachRow"
                ),
                &params,
            )
            .await?
        };
        let mut fps: HashMap<Uuid, Vec<String>> = fps.into_iter().map(|f| (f.rid, f.fps)).collect();
        let mut out: Vec<SeriesRun> = runs
            .into_iter()
            .map(|r| SeriesRun {
                launch_id: r.lid,
                created_at: ms_time(r.at_ms),
                status: r.status,
                flaky_in_run: r.flaky_in_run,
                duration_ms: r.duration_ms,
                env_key: Some(r.env_key).filter(|e| !e.is_empty()),
                branch: Some(r.branch).filter(|b| !b.is_empty()),
                host: None,
                thread: None,
                ci_runner: None,
                attempt_fingerprints: fps.remove(&r.rid).unwrap_or_default(),
                signals: Vec::new(),
            })
            .collect();
        out.reverse();
        Ok(out)
    }

    /// The numbers of the portfolio for the listed projects in `[from, to)`.
    pub async fn portfolio(
        &self,
        projects: &[String],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> anyhow::Result<Vec<PortfolioCounts>> {
        if projects.is_empty() {
            return Ok(Vec::new());
        }
        self.rows(
            &format!(
                "SELECT project, toInt64(uniqExact(launch_id)) AS launches, \
                 toInt64(uniqExactIf(id, status = 'passed')) AS passed, \
                 toInt64(uniqExactIf(id, status NOT IN ('skipped', 'running'))) AS ran, \
                 toInt64(uniqExactIf(id, status IN ('failed', 'broken'))) AS failures, \
                 toInt64(uniqExactIf(series_id, flaky_in_run)) AS flaky FROM {}.results \
                 WHERE project IN {{ps:Array(String)}} AND launch_created_at >= {{from:DateTime64(3, 'UTC')}} \
                 AND launch_created_at < {{to:DateTime64(3, 'UTC')}} GROUP BY project FORMAT JSONEachRow",
                self.db()
            ),
            &[
                ("ps", str_array(projects)),
                ("from", ts(from)),
                ("to", ts(to)),
            ],
        )
        .await
    }

    /// Failures similar to `src` in the listed projects: the same fingerprint (score 1) or
    /// a close normalized message, most similar first.
    pub async fn similar_across(
        &self,
        projects: &[String],
        src: &SimilarSource,
        limit: usize,
    ) -> anyhow::Result<Vec<CrossProjectSimilarItem>> {
        let Some(message) = src.message.as_deref().filter(|m| !m.trim().is_empty()) else {
            return Ok(Vec::new());
        };
        if projects.is_empty() {
            return Ok(Vec::new());
        }
        let key = match_key(src.fingerprint.as_deref(), message);
        let norm = normalize_message(message);
        let rows: Vec<CrossRow> = self
            .rows(
                &format!(
                    "SELECT project, id AS rid, launch_id AS lid, launch_number, name, \
                     substringUTF8(message, 1, 500) AS message, defect_group AS grp, \
                     if(match_key = {{key:String}}, 1., 1 - ngramDistanceUTF8(message_norm, {{norm:String}})) AS score \
                     FROM {}.results FINAL WHERE project IN {{ps:Array(String)}} \
                     AND status IN ('failed', 'broken') AND message != '' \
                     AND (match_key = {{key:String}} OR ngramDistanceUTF8(message_norm, {{norm:String}}) < 0.7) \
                     ORDER BY score DESC, launch_created_at DESC, rid LIMIT {{limit:UInt32}} FORMAT JSONEachRow",
                    self.db()
                ),
                &[
                    ("key", key),
                    ("norm", norm),
                    ("ps", str_array(projects)),
                    ("limit", limit.clamp(1, 100).to_string()),
                ],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| CrossProjectSimilarItem {
                project: r.project,
                result_id: r.rid,
                launch_id: r.lid,
                launch_number: r.launch_number,
                name: r.name,
                message: Some(r.message).filter(|m| !m.is_empty()),
                score: (r.score * 1000.0).round() / 1000.0,
                group: Some(r.grp).filter(|g| !g.is_empty()),
            })
            .collect())
    }

    /// Projects with results or `defect_daily` rows on or after `from_day` (the hourly
    /// rollup goes through them one by one).
    pub async fn projects_since(&self, from_day: NaiveDate) -> anyhow::Result<Vec<String>> {
        #[derive(Deserialize)]
        struct P {
            project: String,
        }
        let db = self.db();
        let rows: Vec<P> = self
            .rows(
                &format!(
                    "SELECT DISTINCT project FROM (SELECT project FROM {db}.results \
                     WHERE launch_created_at >= toDateTime64({{from:Date}}, 3, 'UTC') \
                     UNION ALL SELECT project FROM {db}.defect_daily WHERE day >= {{from:Date}}) \
                     ORDER BY project FORMAT JSONEachRow"
                ),
                &[("from", from_day.format("%Y-%m-%d").to_string())],
            )
            .await?;
        Ok(rows.into_iter().map(|r| r.project).collect())
    }

    /// Recomputes `defect_daily` of one project for the given days from `results FINAL`: a
    /// failed or broken result counts in its current defect group (`toInvestigate` without
    /// one); a group that no longer has results on a day is written as 0, so a triage that
    /// moved results between groups is not counted twice. One statement per call, bounded by
    /// the project and the days (callers pass at most [`ROLLUP_DAYS`] days at once), so one
    /// big project cannot hold up the rollup of the others.
    pub async fn rollup_defects(&self, project: &str, days: &[NaiveDate]) -> anyhow::Result<()> {
        let (Some(lo), Some(hi)) = (days.iter().min(), days.iter().max()) else {
            return Ok(());
        };
        let db = self.db();
        let version = Utc::now().timestamp_micros().max(0) as u64;
        let list: Vec<String> = days
            .iter()
            .map(|d| d.format("%Y-%m-%d").to_string())
            .collect();
        let params: Params = vec![
            ("p", project.to_string()),
            ("lo", lo.format("%Y-%m-%d").to_string()),
            (
                "hi",
                (*hi + chrono::Duration::days(1))
                    .format("%Y-%m-%d")
                    .to_string(),
            ),
            ("days", str_array(&list)),
            ("v", version.to_string()),
        ];
        let sql = format!(
            "INSERT INTO {db}.defect_daily (project, day, branch, env_key, grp, results, version) \
             SELECT project, day, branch, env_key, grp, toUInt64(sum(n)) AS results, {{v:UInt64}} AS version FROM ( \
               SELECT project, toDate(launch_created_at) AS day, branch, env_key, \
                 if(defect_group = '', 'toInvestigate', defect_group) AS grp, count() AS n \
               FROM {db}.results FINAL WHERE project = {{p:String}} \
                 AND launch_created_at >= toDateTime64({{lo:Date}}, 3, 'UTC') \
                 AND launch_created_at < toDateTime64({{hi:Date}}, 3, 'UTC') \
                 AND toDate(launch_created_at) IN {{days:Array(Date)}} \
                 AND status IN ('failed', 'broken') GROUP BY project, day, branch, env_key, grp \
               UNION ALL \
               SELECT project, day, branch, env_key, grp, toUInt64(0) AS n FROM {db}.defect_daily FINAL \
               WHERE project = {{p:String}} AND day IN {{days:Array(Date)}} \
             ) GROUP BY project, day, branch, env_key, grp"
        );
        self.exec_with(&sql, &params, None, &ROLLUP_SETTINGS)
            .await
            .context("recomputing defect_daily")?;
        Ok(())
    }

    /// Rebuilds the daily aggregates of a project for the days of deleted launches (their
    /// rows are already gone from `results`): `fingerprint_daily` from the remaining results
    /// and `defect_daily` through [`ClickHouse::rollup_defects`], only for those days. Days
    /// older than the raw TTL have no results left and keep their aggregates.
    async fn rebuild_days(&self, project: &str, days: &[NaiveDate]) -> anyhow::Result<()> {
        let (Some(lo), Some(hi)) = (days.iter().min(), days.iter().max()) else {
            return Ok(());
        };
        let db = self.db();
        let list: Vec<String> = days
            .iter()
            .map(|d| d.format("%Y-%m-%d").to_string())
            .collect();
        let params: Params = vec![
            ("p", project.to_string()),
            ("days", str_array(&list)),
            ("lo", lo.format("%Y-%m-%d").to_string()),
            (
                "hi",
                (*hi + chrono::Duration::days(1))
                    .format("%Y-%m-%d")
                    .to_string(),
            ),
        ];
        self.exec_with(
            &format!(
                "DELETE FROM {db}.fingerprint_daily WHERE project = {{p:String}} AND day IN {{days:Array(Date)}}"
            ),
            &params,
            None,
            &ROLLUP_SETTINGS,
        )
        .await
        .context("deleting fingerprint_daily days")?;
        self.exec_with(
            &format!(
                "INSERT INTO {db}.fingerprint_daily SELECT project, fingerprint, toDate(launch_created_at) AS day, \
                 branch, env_key, uniqState(id) AS occurrences, uniqState(test_case_id) AS tests, \
                 uniqState(launch_id) AS launches FROM {db}.results WHERE project = {{p:String}} \
                 AND launch_created_at >= toDateTime64({{lo:Date}}, 3, 'UTC') \
                 AND launch_created_at < toDateTime64({{hi:Date}}, 3, 'UTC') \
                 AND toDate(launch_created_at) IN {{days:Array(Date)}} AND fingerprint != '' \
                 AND status IN ('failed', 'broken') GROUP BY project, fingerprint, day, branch, env_key"
            ),
            &params,
            None,
            &ROLLUP_SETTINGS,
        )
        .await
        .context("rebuilding fingerprint_daily days")?;
        for part in days.chunks(ROLLUP_DAYS as usize) {
            self.rollup_defects(project, part).await?;
        }
        Ok(())
    }

    /// Days of results of these launches (before they are deleted).
    async fn launch_days(&self, project: &str, ids: &str) -> anyhow::Result<Vec<NaiveDate>> {
        let rows: Vec<DayRow> = self
            .rows(
                &format!(
                    "SELECT DISTINCT toDate(launch_created_at) AS day FROM {}.results \
                     WHERE project = {{p:String}} AND launch_id IN {{ids:Array(UUID)}} FORMAT JSONEachRow",
                    self.db()
                ),
                &[("p", project.to_string()), ("ids", ids.to_string())],
            )
            .await?;
        Ok(rows.into_iter().map(|r| r.day).collect())
    }
}

/// `a / b` rounded to 3 decimals (0 when `b` is 0).
pub fn ratio(a: i64, b: i64) -> f64 {
    if b == 0 {
        return 0.0;
    }
    ((a as f64 / b as f64) * 1000.0).round() / 1000.0
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
            params.push(("toks", str_array(&toks)));
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
            params.push(("levels", str_array(&levels)));
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
