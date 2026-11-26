//! Signals about test attempts (contract §7.7): pushed grid, CI and Kubernetes events, the
//! FastMock signals the analysis role writes, their matching to attempts (at once, by the
//! launch's analysis step, or later by the resolution job) and the readers suggestions,
//! flaky hints and the result page use.
//!
//! A signal never changes a defect: it is stored next to an attempt and only read.
//!
//! Matching: a `traceId` finds the attempt through `attempts_trace_id` among launches created
//! from a day before the signal to an hour after it (or in its `launchId`); `launchId` +
//! `resultClientId` find the attempt by its client id. The attempt number is the 1-based
//! position of the attempt in its result (as the FastMock correlation and the result page
//! count). Signals that match nothing stay pending for two days, then the resolution job
//! deletes them.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use fastproof_analysis::model::{
    MAX_SIGNAL_DETAIL_BYTES, MAX_SIGNALS_PER_ATTEMPT, MAX_SIGNALS_PER_REQUEST,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use uuid::Uuid;

use super::types::{
    AnalysisResult, AttemptSignal, AttemptSignals, LaunchCtx, SignalConfidence, SignalItem,
    SignalRejection, SignalSource, SignalsAccepted,
};
use crate::tm::TmError;
use crate::{Launch, Store};

/// Kinds a grid may push.
pub const GRID_KINDS: [&str; 5] = [
    "session_crashed",
    "browser_oom",
    "startup_timeout",
    "node_lost",
    "evicted",
];
/// Kinds a CI system may push.
pub const CI_KINDS: [&str; 3] = ["runner_oom", "pod_evicted", "job_cancelled"];
/// Kinds a Kubernetes event exporter may push.
pub const K8S_KINDS: [&str; 3] = ["pod_evicted", "oom_killed", "node_not_ready"];
/// Kinds the analysis role writes from the FastMock journal (`injected_fault` once FastMock
/// records injected faults in its journal).
pub const MOCK_KINDS: [&str; 5] = [
    "unmatched",
    "stub_error",
    "slow",
    "unexpected_calls",
    "injected_fault",
];

/// A pushed signal older than this is refused (`too_old`).
pub const MAX_AGE_DAYS: i64 = 7;
/// A pushed signal this far ahead of the server clock is refused (`too_old`: outside the window).
pub const MAX_AHEAD_SECONDS: i64 = 300;
/// Unmatched signals are kept this long, then deleted.
pub const UNMATCHED_DAYS: i64 = 2;
/// A trace id is looked for in launches created up to this long before the signal.
pub const TRACE_LOOKBACK_HOURS: i64 = 24;
/// ... and up to this long after it (clock skew between the grid and the server).
pub const TRACE_LOOKAHEAD_HOURS: i64 = 1;
/// Bytes of a `resultClientId`, at most (as the ingest takes client ids).
pub const MAX_CLIENT_ID_BYTES: usize = 256;
/// Bytes of a `detail` key, at most.
pub const MAX_DETAIL_KEY_BYTES: usize = 64;
/// Signals a reader returns at most.
pub const MAX_READ: usize = 10_000;
/// Signals of one result the result page lists at most.
pub const MAX_RESULT_SIGNALS: i64 = 500;
/// Pending signals one resolution batch takes.
const RESOLVE_BATCH: i64 = 500;
/// Pending signals one run of the resolution job looks at, at most.
const RESOLVE_MAX_ROWS: u64 = 20_000;
/// Pending signals the analysis of one launch resolves at most.
const LAUNCH_RESOLVE_ROWS: i64 = 5000;
/// Ids per reader query.
const READ_CHUNK: usize = 5000;
/// Namespace of the per-result advisory locks that keep the per-attempt cap exact.
const LOCK_SEED: i64 = 1006;

/// Where the resolution job continues (process-wide), so pending signals that never match do
/// not hide newer ones behind them.
static RESOLVE_CURSOR: AtomicI64 = AtomicI64::new(0);

/// A signal the analysis role writes for an attempt it read from FastMock.
#[derive(Clone, Debug, PartialEq)]
pub struct NewSignal {
    pub result_id: Uuid,
    /// 1-based attempt number in its result.
    pub attempt: i32,
    /// One of [`MOCK_KINDS`].
    pub kind: String,
    pub at: DateTime<Utc>,
    pub confidence: SignalConfidence,
    /// Flat string, number or boolean values.
    pub detail: Map<String, Value>,
}

/// Whether `kind` is one a source may report.
pub fn known_kind(source: SignalSource, kind: &str) -> bool {
    match source {
        SignalSource::Grid => GRID_KINDS.contains(&kind),
        SignalSource::Ci => CI_KINDS.contains(&kind),
        SignalSource::K8s => K8S_KINDS.contains(&kind),
        SignalSource::Mock => MOCK_KINDS.contains(&kind),
    }
}

fn source_of(s: &str) -> SignalSource {
    match s {
        "grid" => SignalSource::Grid,
        "ci" => SignalSource::Ci,
        "k8s" => SignalSource::K8s,
        _ => SignalSource::Mock,
    }
}

fn confidence_of(s: &str) -> SignalConfidence {
    if s == "window" {
        SignalConfidence::Window
    } else {
        SignalConfidence::Exact
    }
}

fn confidence_str(c: SignalConfidence) -> &'static str {
    match c {
        SignalConfidence::Exact => "exact",
        SignalConfidence::Window => "window",
    }
}

/// A 32-hex trace id, lower-cased; all zeros is not one.
pub fn normalize_trace_id(v: &str) -> Option<String> {
    let t = v.trim().to_ascii_lowercase();
    (t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit()) && t.bytes().any(|b| b != b'0'))
        .then_some(t)
}

/// How a pushed signal names its attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Address {
    Trace { trace: String, launch: Option<Uuid> },
    Client { launch: Uuid, client: String },
}

impl Address {
    fn key(&self) -> String {
        match self {
            Self::Trace { trace, launch } => match launch {
                Some(l) => format!("t:{trace}:{l}"),
                None => format!("t:{trace}"),
            },
            Self::Client { launch, client } => format!("c:{launch}:{client}"),
        }
    }

    fn launch(&self) -> Option<Uuid> {
        match self {
            Self::Trace { launch, .. } => *launch,
            Self::Client { launch, .. } => Some(*launch),
        }
    }
}

/// The dedup key of a signal: sha256 of source, kind, time and addressing.
pub fn dedup_key(source: SignalSource, kind: &str, at: DateTime<Utc>, addressing: &str) -> String {
    let mut h = Sha256::new();
    for part in [
        source.as_str(),
        kind,
        &at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        addressing,
    ] {
        h.update(part.as_bytes());
        h.update([0u8]);
    }
    hex::encode(h.finalize())
}

/// A `detail` text as it is stored and shown: NUL (which PostgreSQL cannot hold) and invisible
/// characters (bidirectional controls, zero-width characters) dropped, other control
/// characters (line breaks, tabs, escapes) as spaces, so a value can neither forge another
/// `key: value` line nor reorder or hide what a reader sees.
fn clean_text(s: &str) -> String {
    s.chars()
        .filter_map(|c| {
            if c == '\0' || super::log_rules::invisible(c) {
                None
            } else if c.is_control() || c == '\u{2028}' || c == '\u{2029}' {
                Some(' ')
            } else {
                Some(c)
            }
        })
        .collect()
}

/// Checks a `detail` object: flat string, number or boolean values and keys of 1 to 64 bytes.
/// Returns the cleaned object (see [`clean_text`]) or the reason it is not one.
pub fn clean_detail(d: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    for (k, v) in d {
        let key = clean_text(k);
        if key.is_empty() || key.len() > MAX_DETAIL_KEY_BYTES {
            return Err(format!(
                "detail keys must have 1 to {MAX_DETAIL_KEY_BYTES} bytes"
            ));
        }
        let v = match v {
            Value::String(s) => Value::String(clean_text(s)),
            Value::Number(_) | Value::Bool(_) => v.clone(),
            _ => {
                return Err(format!(
                    "detail.{key} must be a string, a number or a boolean (detail is flat)"
                ));
            }
        };
        out.insert(key, v);
    }
    Ok(out)
}

/// Why an item of a push is refused, before any lookup.
fn check_item(item: &SignalItem, now: DateTime<Utc>) -> Result<Address, &'static str> {
    if item.source == SignalSource::Mock || !known_kind(item.source, &item.kind) {
        return Err("unknown_kind");
    }
    if item.at < now - ChronoDuration::days(MAX_AGE_DAYS)
        || item.at > now + ChronoDuration::seconds(MAX_AHEAD_SECONDS)
    {
        return Err("too_old");
    }
    let addr = match (&item.trace_id, &item.result_client_id, item.launch_id) {
        (Some(t), None, launch) => Address::Trace {
            trace: normalize_trace_id(t).ok_or("bad_address")?,
            launch,
        },
        (None, Some(c), Some(launch)) => {
            if c.is_empty()
                || c.len() > MAX_CLIENT_ID_BYTES
                || c.chars().any(char::is_control)
                || c.trim() != c
            {
                return Err("bad_address");
            }
            Address::Client {
                launch,
                client: c.clone(),
            }
        }
        _ => return Err("bad_address"),
    };
    Ok(addr)
}

/// An attempt a signal belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hit {
    launch_id: Uuid,
    launch_created_at: DateTime<Utc>,
    result_id: Uuid,
    attempt: i32,
}

/// A signal to match: its key in the caller's list, project and addressing.
#[derive(Clone, Debug)]
struct Lookup {
    key: i64,
    project_id: Uuid,
    at: DateTime<Utc>,
    trace: Option<String>,
    client: Option<String>,
    launch: Option<(Uuid, DateTime<Utc>)>,
}

/// The attempt number of `a` in its result: 1 + the attempts of the result before it.
const ATTEMPT_NO_SQL: &str = "(select count(*) from attempts b where b.result_id = a.result_id \
     and b.launch_created_at = a.launch_created_at and b.seq <= a.seq)::int";

/// Finds the attempts of signals; signals that match nothing are left out.
async fn find_attempts(
    conn: &mut PgConnection,
    rows: &[Lookup],
) -> anyhow::Result<HashMap<i64, Hit>> {
    let mut out = HashMap::new();
    let traced: Vec<&Lookup> = rows.iter().filter(|r| r.trace.is_some()).collect();
    if !traced.is_empty() {
        let mut keys = Vec::with_capacity(traced.len());
        let mut projects = Vec::with_capacity(traced.len());
        let mut traces = Vec::with_capacity(traced.len());
        let mut launches: Vec<Option<Uuid>> = Vec::with_capacity(traced.len());
        let mut los = Vec::with_capacity(traced.len());
        let mut his = Vec::with_capacity(traced.len());
        for r in &traced {
            keys.push(r.key);
            projects.push(r.project_id);
            traces.push(r.trace.clone().unwrap_or_default());
            match r.launch {
                Some((l, created)) => {
                    launches.push(Some(l));
                    los.push(created);
                    his.push(created);
                }
                None => {
                    launches.push(None);
                    los.push(r.at - ChronoDuration::hours(TRACE_LOOKBACK_HOURS));
                    his.push(r.at + ChronoDuration::hours(TRACE_LOOKAHEAD_HOURS));
                }
            }
        }
        let lo = los.iter().min().copied().unwrap_or_else(Utc::now);
        let hi = his.iter().max().copied().unwrap_or_else(Utc::now);
        let found: Vec<(i64, Uuid, DateTime<Utc>, Uuid, i32)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select k.key, a.launch_id, a.launch_created_at, a.result_id, {ATTEMPT_NO_SQL} \
             from unnest($1::bigint[], $2::uuid[], $3::text[], $4::uuid[], $5::timestamptz[], $6::timestamptz[]) \
               as k(key, project_id, trace, launch_id, lo, hi) \
             cross join lateral (select x.launch_id, x.launch_created_at, x.result_id, x.seq from attempts x \
               where x.trace_id = k.trace and x.project_id = k.project_id \
                 and (k.launch_id is null or x.launch_id = k.launch_id) \
                 and x.launch_created_at >= k.lo and x.launch_created_at <= k.hi \
                 and x.launch_created_at >= $7 and x.launch_created_at <= $8 \
               order by x.launch_created_at desc, x.seq desc limit 1) a"
        )))
        .bind(&keys)
        .bind(&projects)
        .bind(&traces)
        .bind(&launches)
        .bind(&los)
        .bind(&his)
        .bind(lo)
        .bind(hi)
        .fetch_all(&mut *conn)
        .await?;
        for (key, launch_id, launch_created_at, result_id, attempt) in found {
            out.insert(
                key,
                Hit {
                    launch_id,
                    launch_created_at,
                    result_id,
                    attempt,
                },
            );
        }
    }
    let by_client: Vec<&Lookup> = rows
        .iter()
        .filter(|r| r.trace.is_none() && r.client.is_some() && r.launch.is_some())
        .collect();
    if !by_client.is_empty() {
        let mut keys = Vec::with_capacity(by_client.len());
        let mut projects = Vec::with_capacity(by_client.len());
        let mut launches = Vec::with_capacity(by_client.len());
        let mut created = Vec::with_capacity(by_client.len());
        let mut clients = Vec::with_capacity(by_client.len());
        for r in &by_client {
            let (l, c) = r.launch.unwrap_or((Uuid::nil(), Utc::now()));
            keys.push(r.key);
            projects.push(r.project_id);
            launches.push(l);
            created.push(c);
            clients.push(r.client.clone().unwrap_or_default());
        }
        let found: Vec<(i64, Uuid, DateTime<Utc>, Uuid, i32)> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "select k.key, a.launch_id, a.launch_created_at, a.result_id, {ATTEMPT_NO_SQL} \
             from unnest($1::bigint[], $2::uuid[], $3::uuid[], $4::timestamptz[], $5::text[]) \
               as k(key, project_id, launch_id, created, client) \
             cross join lateral (select x.launch_id, x.launch_created_at, x.result_id, x.seq from attempts x \
               where x.launch_id = k.launch_id and x.launch_created_at = k.created \
                 and x.client_key = any(array['r:' || k.client, k.client]) and x.project_id = k.project_id \
               order by (x.client_key = 'r:' || k.client) desc limit 1) a"
            )))
            .bind(&keys)
            .bind(&projects)
            .bind(&launches)
            .bind(&created)
            .bind(&clients)
            .fetch_all(&mut *conn)
            .await?;
        for (key, launch_id, launch_created_at, result_id, attempt) in found {
            out.insert(
                key,
                Hit {
                    launch_id,
                    launch_created_at,
                    result_id,
                    attempt,
                },
            );
        }
    }
    Ok(out)
}

/// Locks the results (in a stable order) and reads how many signals each of their attempts
/// already has, so the per-attempt cap holds under concurrent writers.
async fn attempt_counts(
    conn: &mut PgConnection,
    results: &BTreeSet<Uuid>,
) -> anyhow::Result<HashMap<(Uuid, i32), usize>> {
    if results.is_empty() {
        return Ok(HashMap::new());
    }
    let ids: Vec<Uuid> = results.iter().copied().collect();
    sqlx::query(
        "select pg_advisory_xact_lock(hashtextextended(r::text, $2)) from unnest($1::uuid[]) as r order by r",
    )
    .bind(&ids)
    .bind(LOCK_SEED)
    .execute(&mut *conn)
    .await?;
    let rows: Vec<(Uuid, i32, i64)> = sqlx::query_as(
        "select result_id, attempt, count(*) from attempt_signals \
         where result_id = any($1) and resolved and attempt is not null group by 1, 2",
    )
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(r, a, n)| ((r, a), n.max(0) as usize))
        .collect())
}

/// Keeps a hit when its attempt has room; counts it.
fn take_slot(counts: &mut HashMap<(Uuid, i32), usize>, hit: &Hit) -> bool {
    let n = counts.entry((hit.result_id, hit.attempt)).or_insert(0);
    if *n >= MAX_SIGNALS_PER_ATTEMPT {
        false
    } else {
        *n += 1;
        true
    }
}

fn detail_of(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

type SignalRow = (
    i64,
    Option<Uuid>,
    Option<i32>,
    String,
    String,
    DateTime<Utc>,
    String,
    Value,
);

fn signal_of(r: SignalRow) -> AttemptSignal {
    AttemptSignal {
        id: r.0,
        result_id: r.1,
        attempt: r.2,
        source: source_of(&r.3),
        kind: r.4,
        at: r.5,
        confidence: confidence_of(&r.6),
        detail: detail_of(r.7),
    }
}

const SIGNAL_COLUMNS: &str =
    "s.id, s.result_id, s.attempt, s.source, s.kind, s.at, s.confidence, s.detail";

/// What one resolution pass did, per source.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Resolved {
    matched: u64,
    per_source: BTreeMap<(String, &'static str), u64>,
}

impl Resolved {
    fn add(&mut self, other: Resolved) {
        self.matched += other.matched;
        for (k, n) in other.per_source {
            *self.per_source.entry(k).or_default() += n;
        }
    }

    fn count(&self) {
        for ((source, outcome), n) in &self.per_source {
            count(source, outcome, *n);
        }
    }
}

/// Pending rows: id, project, launch, launch time, trace, client id, time, source.
type PendingRow = (
    i64,
    Uuid,
    Option<Uuid>,
    Option<DateTime<Utc>>,
    Option<String>,
    Option<String>,
    DateTime<Utc>,
    String,
);

const PENDING_COLUMNS: &str = "s.id, s.project_id, s.launch_id, s.launch_created_at, s.trace_id, s.result_client_key, s.at, s.source";

/// Matches locked pending rows to attempts: matched rows are attached, rows whose attempt is
/// full are deleted, the rest stay pending.
async fn attach_pending(conn: &mut PgConnection, rows: &[PendingRow]) -> anyhow::Result<Resolved> {
    let lookups: Vec<Lookup> = rows
        .iter()
        .map(|r| Lookup {
            key: r.0,
            project_id: r.1,
            at: r.6,
            trace: r.4.clone(),
            client: r.5.clone(),
            launch: r.2.zip(r.3),
        })
        .collect();
    let hits = find_attempts(conn, &lookups).await?;
    if hits.is_empty() {
        return Ok(Resolved::default());
    }
    let results: BTreeSet<Uuid> = hits.values().map(|h| h.result_id).collect();
    let mut counts = attempt_counts(conn, &results).await?;
    let mut ids = Vec::new();
    let mut launches = Vec::new();
    let mut created = Vec::new();
    let mut result_ids = Vec::new();
    let mut attempts = Vec::new();
    let mut full = Vec::new();
    let mut keys: Vec<i64> = hits.keys().copied().collect();
    keys.sort_unstable();
    for key in keys {
        let h = hits[&key];
        if take_slot(&mut counts, &h) {
            ids.push(key);
            launches.push(h.launch_id);
            created.push(h.launch_created_at);
            result_ids.push(h.result_id);
            attempts.push(h.attempt);
        } else {
            full.push(key);
        }
    }
    if !ids.is_empty() {
        sqlx::query(
            "update attempt_signals s set launch_id = u.launch_id, launch_created_at = u.created, \
               result_id = u.result_id, attempt = u.attempt, resolved = true \
             from unnest($1::bigint[], $2::uuid[], $3::timestamptz[], $4::uuid[], $5::int[]) \
               as u(id, launch_id, created, result_id, attempt) \
             where s.id = u.id and not s.resolved",
        )
        .bind(&ids)
        .bind(&launches)
        .bind(&created)
        .bind(&result_ids)
        .bind(&attempts)
        .execute(&mut *conn)
        .await?;
    }
    if !full.is_empty() {
        sqlx::query("delete from attempt_signals where id = any($1) and not resolved")
            .bind(&full)
            .execute(&mut *conn)
            .await?;
    }
    let source: HashMap<i64, &str> = rows.iter().map(|r| (r.0, r.7.as_str())).collect();
    let mut out = Resolved {
        matched: ids.len() as u64,
        ..Resolved::default()
    };
    for (list, outcome) in [(&ids, "resolved"), (&full, "attempt_full")] {
        for id in list {
            let s = source.get(id).copied().unwrap_or("grid").to_string();
            *out.per_source.entry((s, outcome)).or_default() += 1;
        }
    }
    Ok(out)
}

fn count(source: &str, outcome: &'static str, n: u64) {
    if n > 0 {
        metrics::counter!("fastproof_analysis_signals_total", "source" => source.to_string(), "outcome" => outcome)
            .increment(n);
    }
}

impl Store {
    /// Stores pushed signals of a project (contract §7.7): each item is checked (kind of its
    /// source, time window, addressing, `detail`), duplicates (same source, kind, time and
    /// addressing) are refused, and every stored signal is matched to its attempt at once when
    /// the attempt exists (at most [`MAX_SIGNALS_PER_ATTEMPT`] per attempt); the others stay
    /// pending for the resolution job. A `detail` that is not a flat object of strings,
    /// numbers and booleans fails the whole request (400); everything else is refused per item.
    pub async fn insert_signals(
        &self,
        project_id: Uuid,
        items: &[SignalItem],
        now: DateTime<Utc>,
    ) -> AnalysisResult<SignalsAccepted> {
        if items.is_empty() || items.len() > MAX_SIGNALS_PER_REQUEST {
            return Err(TmError::bad_request(format!(
                "items must hold 1 to {MAX_SIGNALS_PER_REQUEST} signals"
            )));
        }
        let mut details = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let d = match &item.detail {
                Some(d) => {
                    clean_detail(d).map_err(|e| TmError::bad_request(format!("items[{i}]: {e}")))?
                }
                None => Map::new(),
            };
            details.push(d);
        }
        let mut rejected: Vec<SignalRejection> = Vec::new();
        let reject = |rejected: &mut Vec<SignalRejection>, i: usize, code: &str| {
            rejected.push(SignalRejection {
                index: i as u32,
                code: code.to_string(),
            });
        };
        let mut checked: Vec<(usize, Address)> = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            match check_item(item, now) {
                Err(code) => reject(&mut rejected, i, code),
                Ok(_)
                    if serde_json::to_vec(&details[i]).map_or(usize::MAX, |b| b.len())
                        > MAX_SIGNAL_DETAIL_BYTES =>
                {
                    reject(&mut rejected, i, "detail_too_large")
                }
                Ok(a) => checked.push((i, a)),
            }
        }
        let mut tx = self.pool.begin().await?;
        let launch_ids: Vec<Uuid> = checked
            .iter()
            .filter_map(|(_, a)| a.launch())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let launches: HashMap<Uuid, DateTime<Utc>> = if launch_ids.is_empty() {
            HashMap::new()
        } else {
            sqlx::query_as::<_, (Uuid, DateTime<Utc>)>(
                "select id, created_at from launches where project_id = $1 and id = any($2)",
            )
            .bind(project_id)
            .bind(&launch_ids)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect()
        };
        let mut valid: Vec<(usize, Address, String)> = Vec::with_capacity(checked.len());
        let mut seen: HashSet<String> = HashSet::new();
        for (i, a) in checked {
            if a.launch().is_some_and(|l| !launches.contains_key(&l)) {
                reject(&mut rejected, i, "bad_address");
                continue;
            }
            let item = &items[i];
            let key = dedup_key(item.source, &item.kind, item.at, &a.key());
            if !seen.insert(key.clone()) {
                reject(&mut rejected, i, "duplicate");
                continue;
            }
            valid.push((i, a, key));
        }
        let keys: Vec<String> = valid.iter().map(|v| v.2.clone()).collect();
        let stored: HashSet<String> = if keys.is_empty() {
            HashSet::new()
        } else {
            sqlx::query_scalar::<_, String>(
                "select dedup_key from attempt_signals where project_id = $1 and dedup_key = any($2)",
            )
            .bind(project_id)
            .bind(&keys)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect()
        };
        valid.retain(|(i, _, key)| {
            if stored.contains(key) {
                reject(&mut rejected, *i, "duplicate");
                false
            } else {
                true
            }
        });
        let lookups: Vec<Lookup> = valid
            .iter()
            .map(|(i, a, _)| {
                let launch = a.launch().and_then(|l| launches.get(&l).map(|c| (l, *c)));
                let (trace, client) = match a {
                    Address::Trace { trace, .. } => (Some(trace.clone()), None),
                    Address::Client { client, .. } => (None, Some(client.clone())),
                };
                Lookup {
                    key: *i as i64,
                    project_id,
                    at: items[*i].at,
                    trace,
                    client,
                    launch,
                }
            })
            .collect();
        let hits = find_attempts(&mut tx, &lookups).await?;
        let results: BTreeSet<Uuid> = hits.values().map(|h| h.result_id).collect();
        let mut counts = attempt_counts(&mut tx, &results).await?;
        let n = valid.len();
        let mut c_launch: Vec<Option<Uuid>> = Vec::with_capacity(n);
        let mut c_created: Vec<Option<DateTime<Utc>>> = Vec::with_capacity(n);
        let mut c_result: Vec<Option<Uuid>> = Vec::with_capacity(n);
        let mut c_attempt: Vec<Option<i32>> = Vec::with_capacity(n);
        let mut c_trace: Vec<Option<String>> = Vec::with_capacity(n);
        let mut c_client: Vec<Option<String>> = Vec::with_capacity(n);
        let mut c_source: Vec<&'static str> = Vec::with_capacity(n);
        let mut c_kind: Vec<String> = Vec::with_capacity(n);
        let mut c_at: Vec<DateTime<Utc>> = Vec::with_capacity(n);
        let mut c_detail: Vec<Value> = Vec::with_capacity(n);
        let mut c_key: Vec<String> = Vec::with_capacity(n);
        let mut c_resolved: Vec<bool> = Vec::with_capacity(n);
        let mut c_index: Vec<usize> = Vec::with_capacity(n);
        for (i, a, key) in valid {
            let item = &items[i];
            let hit = hits.get(&(i as i64));
            if let Some(h) = hit
                && !take_slot(&mut counts, h)
            {
                reject(&mut rejected, i, "attempt_full");
                continue;
            }
            let launch = a.launch().and_then(|l| launches.get(&l).map(|c| (l, *c)));
            match hit {
                Some(h) => {
                    c_launch.push(Some(h.launch_id));
                    c_created.push(Some(h.launch_created_at));
                    c_result.push(Some(h.result_id));
                    c_attempt.push(Some(h.attempt));
                }
                None => {
                    c_launch.push(launch.map(|l| l.0));
                    c_created.push(launch.map(|l| l.1));
                    c_result.push(None);
                    c_attempt.push(None);
                }
            }
            match &a {
                Address::Trace { trace, .. } => {
                    c_trace.push(Some(trace.clone()));
                    c_client.push(None);
                }
                Address::Client { client, .. } => {
                    c_trace.push(None);
                    c_client.push(Some(client.clone()));
                }
            }
            c_source.push(item.source.as_str());
            c_kind.push(item.kind.clone());
            c_at.push(item.at);
            c_detail.push(Value::Object(details[i].clone()));
            c_key.push(key);
            c_resolved.push(hit.is_some());
            c_index.push(i);
        }
        let inserted: HashSet<String> = if c_key.is_empty() {
            HashSet::new()
        } else {
            sqlx::query_scalar::<_, String>(
                "insert into attempt_signals (project_id, launch_id, launch_created_at, result_id, attempt, \
                   trace_id, result_client_key, source, kind, at, confidence, detail, dedup_key, resolved) \
                 select $1, u.launch_id, u.created, u.result_id, u.attempt, u.trace, u.client, u.source, u.kind, \
                   u.at, 'exact', u.detail, u.key, u.resolved \
                 from unnest($2::uuid[], $3::timestamptz[], $4::uuid[], $5::int[], $6::text[], $7::text[], \
                   $8::text[], $9::text[], $10::timestamptz[], $11::jsonb[], $12::text[], $13::bool[]) \
                   as u(launch_id, created, result_id, attempt, trace, client, source, kind, at, detail, key, resolved) \
                 on conflict (project_id, dedup_key) do nothing returning dedup_key",
            )
            .bind(project_id)
            .bind(&c_launch)
            .bind(&c_created)
            .bind(&c_result)
            .bind(&c_attempt)
            .bind(&c_trace)
            .bind(&c_client)
            .bind(&c_source)
            .bind(&c_kind)
            .bind(&c_at)
            .bind(&c_detail)
            .bind(&c_key)
            .bind(&c_resolved)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect()
        };
        tx.commit().await?;
        let mut out = SignalsAccepted {
            accepted: 0,
            matched: 0,
            pending: 0,
            rejected: Vec::new(),
        };
        let mut by_source: BTreeMap<(&'static str, &'static str), u64> = BTreeMap::new();
        for (j, key) in c_key.iter().enumerate() {
            if !inserted.contains(key) {
                reject(&mut rejected, c_index[j], "duplicate");
                continue;
            }
            out.accepted += 1;
            let outcome = if c_resolved[j] {
                out.matched += 1;
                "matched"
            } else {
                out.pending += 1;
                "pending"
            };
            *by_source.entry((c_source[j], outcome)).or_default() += 1;
        }
        for r in &rejected {
            let source = items[r.index as usize].source.as_str();
            *by_source.entry((source, "rejected")).or_default() += 1;
        }
        for ((source, outcome), n) in by_source {
            count(source, outcome, n);
        }
        rejected.sort_by_key(|r| r.index);
        out.rejected = rejected;
        Ok(out)
    }

    /// The resolution job's work (contract §7.7): pending signals of the last two days are
    /// matched to attempts that arrived after them, in batches of [`RESOLVE_BATCH`] that other
    /// replicas skip (`for update skip locked`), round-robin over the pending rows, within
    /// `budget`; unmatched signals older than two days are deleted. Returns the signals matched.
    pub async fn resolve_signals(&self, budget: Duration) -> anyhow::Result<u64> {
        let started = Instant::now();
        let mut done = Resolved::default();
        let mut seen = 0u64;
        let mut after = RESOLVE_CURSOR.load(Ordering::Relaxed);
        let mut wrapped = false;
        while started.elapsed() < budget && seen < RESOLVE_MAX_ROWS {
            let mut tx = self.pool.begin().await?;
            let rows: Vec<PendingRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "select {PENDING_COLUMNS} from attempt_signals s where not s.resolved and s.id > $1 \
                   and s.created_at > now() - make_interval(days => $2) \
                 order by s.id limit $3 for update of s skip locked"
            )))
            .bind(after)
            .bind(UNMATCHED_DAYS as i32)
            .bind(RESOLVE_BATCH)
            .fetch_all(&mut *tx)
            .await?;
            if rows.is_empty() {
                tx.commit().await?;
                if after == 0 || wrapped {
                    after = 0;
                    break;
                }
                after = 0;
                wrapped = true;
                continue;
            }
            seen += rows.len() as u64;
            after = rows.last().map_or(after, |r| r.0);
            let r = attach_pending(&mut tx, &rows).await?;
            tx.commit().await?;
            done.add(r);
        }
        RESOLVE_CURSOR.store(after, Ordering::Relaxed);
        while started.elapsed() < budget {
            let gone: Vec<String> = sqlx::query_scalar(
                "delete from attempt_signals where ctid = any(array(select ctid from attempt_signals \
                   where not resolved and created_at < now() - make_interval(days => $1) limit $2)) \
                 returning source",
            )
            .bind(UNMATCHED_DAYS as i32)
            .bind(RESOLVE_BATCH)
            .fetch_all(&self.pool)
            .await?;
            for source in &gone {
                *done
                    .per_source
                    .entry((source.clone(), "expired"))
                    .or_default() += 1;
            }
            if (gone.len() as i64) < RESOLVE_BATCH {
                break;
            }
        }
        if started.elapsed() >= budget || seen >= RESOLVE_MAX_ROWS {
            metrics::counter!("fastproof_analysis_budget_exhausted_total", "kind" => "signals")
                .increment(1);
        }
        done.count();
        Ok(done.matched)
    }

    /// Matches the pending signals of one launch (its `launchId`, or a trace id of one of its
    /// attempts) right before the launch is analysed. Returns the signals matched.
    pub async fn resolve_launch_signals(
        &self,
        project_id: Uuid,
        launch: &Launch,
    ) -> anyhow::Result<u64> {
        let mut tx = self.pool.begin().await?;
        let rows: Vec<PendingRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {PENDING_COLUMNS} from attempt_signals s where s.project_id = $1 and not s.resolved \
               and s.created_at > now() - make_interval(days => $4) \
               and (s.launch_id = $2 or (s.launch_id is null and s.trace_id in \
                 (select a.trace_id from attempts a where a.launch_id = $2 and a.launch_created_at = $3 \
                    and a.trace_id is not null))) \
             order by s.id limit $5 for update of s skip locked"
        )))
        .bind(project_id)
        .bind(launch.id)
        .bind(launch.created_at)
        .bind(UNMATCHED_DAYS as i32)
        .bind(LAUNCH_RESOLVE_ROWS)
        .fetch_all(&mut *tx)
        .await?;
        if rows.is_empty() {
            tx.commit().await?;
            return Ok(0);
        }
        let r = attach_pending(&mut tx, &rows).await?;
        tx.commit().await?;
        r.count();
        Ok(r.matched)
    }

    /// Failed and broken attempts of a launch, newest first, at most `limit`: result, 1-based
    /// attempt number. The FastMock step reads the journal for these.
    pub async fn failed_attempts(
        &self,
        launch: &Launch,
        limit: u32,
    ) -> anyhow::Result<Vec<(Uuid, i32)>> {
        let rows: Vec<(Uuid, i32)> = sqlx::query_as(
            "select result_id, no from (select a.result_id, a.status, a.started_at, a.seq, \
               (row_number() over (partition by a.result_id order by a.seq))::int as no \
               from attempts a where a.launch_id = $1 and a.launch_created_at = $2 and a.project_id = $3) x \
             where status in ('failed', 'broken') order by started_at desc nulls last, seq desc limit $4",
        )
        .bind(launch.id)
        .bind(launch.created_at)
        .bind(launch.project_id)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Writes the FastMock signals of a launch's attempts (source `mock`). One row per attempt
    /// and kind: analysing the launch again refreshes it instead of adding another. New rows
    /// respect the per-attempt cap. Returns the rows written.
    pub async fn write_mock_signals(
        &self,
        ctx: &LaunchCtx<'_>,
        signals: &[NewSignal],
    ) -> anyhow::Result<u64> {
        let signals: Vec<&NewSignal> = signals
            .iter()
            .filter(|s| MOCK_KINDS.contains(&s.kind.as_str()) && s.attempt >= 1)
            .collect();
        if signals.is_empty() {
            return Ok(0);
        }
        let launch = ctx.launch;
        let mut tx = self.pool.begin().await?;
        let ids: Vec<Uuid> = signals
            .iter()
            .map(|s| s.result_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let own: HashSet<Uuid> = sqlx::query_scalar::<_, Uuid>(
            "select id from results where launch_id = $1 and launch_created_at = $2 and project_id = $3 \
               and id = any($4)",
        )
        .bind(launch.id)
        .bind(launch.created_at)
        .bind(ctx.project_id)
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
        let signals: Vec<&NewSignal> = signals
            .into_iter()
            .filter(|s| own.contains(&s.result_id))
            .collect();
        let keys: Vec<String> = signals
            .iter()
            .map(|s| {
                dedup_key(
                    SignalSource::Mock,
                    &s.kind,
                    DateTime::<Utc>::UNIX_EPOCH,
                    &format!("r:{}:{}", s.result_id, s.attempt),
                )
            })
            .collect();
        let existing: HashSet<String> = sqlx::query_scalar::<_, String>(
            "select dedup_key from attempt_signals where project_id = $1 and dedup_key = any($2)",
        )
        .bind(ctx.project_id)
        .bind(&keys)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
        let results: BTreeSet<Uuid> = signals.iter().map(|s| s.result_id).collect();
        let mut counts = attempt_counts(&mut tx, &results).await?;
        let mut c_result = Vec::new();
        let mut c_attempt = Vec::new();
        let mut c_kind = Vec::new();
        let mut c_at = Vec::new();
        let mut c_conf = Vec::new();
        let mut c_detail = Vec::new();
        let mut c_key = Vec::new();
        for (s, key) in signals.iter().zip(keys) {
            let hit = Hit {
                launch_id: launch.id,
                launch_created_at: launch.created_at,
                result_id: s.result_id,
                attempt: s.attempt,
            };
            if !existing.contains(&key) && !take_slot(&mut counts, &hit) {
                continue;
            }
            let detail = clean_detail(&s.detail).unwrap_or_default();
            c_result.push(s.result_id);
            c_attempt.push(s.attempt);
            c_kind.push(s.kind.clone());
            c_at.push(s.at);
            c_conf.push(confidence_str(s.confidence));
            c_detail.push(Value::Object(detail));
            c_key.push(key);
        }
        let n = if c_key.is_empty() {
            0
        } else {
            sqlx::query(
                "insert into attempt_signals (project_id, launch_id, launch_created_at, result_id, attempt, \
                   source, kind, at, confidence, detail, dedup_key, resolved) \
                 select $1, $2, $3, u.result_id, u.attempt, 'mock', u.kind, u.at, u.confidence, u.detail, u.key, true \
                 from unnest($4::uuid[], $5::int[], $6::text[], $7::timestamptz[], $8::text[], $9::jsonb[], $10::text[]) \
                   as u(result_id, attempt, kind, at, confidence, detail, key) \
                 on conflict (project_id, dedup_key) do update set at = excluded.at, \
                   confidence = excluded.confidence, detail = excluded.detail",
            )
            .bind(ctx.project_id)
            .bind(launch.id)
            .bind(launch.created_at)
            .bind(&c_result)
            .bind(&c_attempt)
            .bind(&c_kind)
            .bind(&c_at)
            .bind(&c_conf)
            .bind(&c_detail)
            .bind(&c_key)
            .execute(&mut *tx)
            .await?
            .rows_affected()
        };
        tx.commit().await?;
        count("mock", "written", n);
        Ok(n)
    }

    /// Stored signals of results (matched ones), oldest first, at most [`MAX_READ`].
    pub async fn signals_for_results(
        &self,
        project_id: Uuid,
        result_ids: &[Uuid],
    ) -> anyhow::Result<Vec<AttemptSignal>> {
        let mut out = Vec::new();
        for chunk in result_ids.chunks(READ_CHUNK) {
            if out.len() >= MAX_READ {
                break;
            }
            let rows: Vec<SignalRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "select {SIGNAL_COLUMNS} from attempt_signals s \
                 where s.project_id = $1 and s.result_id = any($2) order by s.id limit $3"
            )))
            .bind(project_id)
            .bind(chunk)
            .bind((MAX_READ - out.len()) as i64)
            .fetch_all(&self.pool)
            .await?;
            out.extend(rows.into_iter().map(signal_of));
        }
        Ok(out)
    }

    /// Stored signals of the results of a series whose launch was created at `since` or later,
    /// with their result, newest launch first, at most [`MAX_READ`].
    pub async fn signals_for_series(
        &self,
        project_id: Uuid,
        series_id: Uuid,
        since: DateTime<Utc>,
    ) -> anyhow::Result<Vec<(Uuid, AttemptSignal)>> {
        let rows: Vec<SignalRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {SIGNAL_COLUMNS} from results r \
             join attempt_signals s on s.result_id = r.id and s.project_id = r.project_id \
             where r.project_id = $1 and r.series_id = $2 and r.launch_created_at >= $3 \
             order by r.launch_created_at desc, s.id limit $4"
        )))
        .bind(project_id)
        .bind(series_id)
        .bind(since)
        .bind(MAX_READ as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let s = signal_of(r);
                s.result_id.map(|id| (id, s))
            })
            .collect())
    }

    /// `GET .../results/{id}/signals`: the result's signals by attempt and time, at most
    /// [`MAX_RESULT_SIGNALS`]; 404 for a result of another project.
    pub async fn result_signals(
        &self,
        project_id: Uuid,
        result_id: Uuid,
    ) -> AnalysisResult<AttemptSignals> {
        let exists: bool = sqlx::query_scalar(
            "select exists(select 1 from results where project_id = $1 and id = $2)",
        )
        .bind(project_id)
        .bind(result_id)
        .fetch_one(&self.pool)
        .await?;
        if !exists {
            return Err(TmError::NotFound("result"));
        }
        let rows: Vec<SignalRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {SIGNAL_COLUMNS} from attempt_signals s where s.project_id = $1 and s.result_id = $2 \
             order by s.attempt, s.at, s.id limit $3"
        )))
        .bind(project_id)
        .bind(result_id)
        .bind(MAX_RESULT_SIGNALS)
        .fetch_all(&self.pool)
        .await?;
        Ok(AttemptSignals {
            items: rows.into_iter().map(signal_of).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(v: Value) -> SignalItem {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn kinds_are_per_source() {
        assert!(known_kind(SignalSource::Grid, "node_lost"));
        assert!(!known_kind(SignalSource::Grid, "runner_oom"));
        assert!(known_kind(SignalSource::Ci, "job_cancelled"));
        assert!(known_kind(SignalSource::K8s, "pod_evicted"));
        assert!(known_kind(SignalSource::Ci, "pod_evicted"));
        assert!(!known_kind(SignalSource::K8s, "node-lost"));
        assert!(known_kind(SignalSource::Mock, "unexpected_calls"));
    }

    #[test]
    fn items_are_checked_before_any_lookup() {
        let now = Utc::now();
        let at = now.to_rfc3339();
        let trace = "4BF92F3577B34DA6A3CE929D0E0E4736";
        let launch = Uuid::now_v7();
        let ok = check_item(
            &item(json!({"traceId": trace, "source": "grid", "kind": "node_lost", "at": at})),
            now,
        )
        .unwrap();
        assert_eq!(
            ok,
            Address::Trace {
                trace: trace.to_ascii_lowercase(),
                launch: None
            }
        );
        let client = check_item(
            &item(
                json!({"launchId": launch, "resultClientId": "py-1-a-0", "source": "ci", "kind": "runner_oom", "at": at}),
            ),
            now,
        )
        .unwrap();
        assert_eq!(client.key(), format!("c:{launch}:py-1-a-0"));
        let code = |v: Value| check_item(&item(v), now).unwrap_err();
        assert_eq!(
            code(json!({"traceId": trace, "source": "grid", "kind": "runner_oom", "at": at})),
            "unknown_kind"
        );
        assert_eq!(
            code(json!({"traceId": trace, "source": "mock", "kind": "unmatched", "at": at})),
            "unknown_kind",
            "mock signals are written by the server only"
        );
        assert_eq!(
            code(json!({"source": "grid", "kind": "node_lost", "at": at})),
            "bad_address"
        );
        assert_eq!(
            code(
                json!({"traceId": "00000000000000000000000000000000", "source": "grid", "kind": "node_lost", "at": at})
            ),
            "bad_address"
        );
        assert_eq!(
            code(json!({"traceId": "xyz", "source": "grid", "kind": "node_lost", "at": at})),
            "bad_address"
        );
        assert_eq!(
            code(
                json!({"resultClientId": "py-1", "source": "grid", "kind": "node_lost", "at": at})
            ),
            "bad_address",
            "a client id needs its launch"
        );
        assert_eq!(
            code(
                json!({"traceId": trace, "launchId": launch, "resultClientId": "py-1", "source": "grid", "kind": "node_lost", "at": at})
            ),
            "bad_address",
            "one addressing only"
        );
        assert_eq!(
            code(
                json!({"launchId": launch, "resultClientId": "a\u{7}b", "source": "grid", "kind": "node_lost", "at": at})
            ),
            "bad_address"
        );
        let long = "x".repeat(MAX_CLIENT_ID_BYTES + 1);
        assert_eq!(
            code(
                json!({"launchId": launch, "resultClientId": long, "source": "grid", "kind": "node_lost", "at": at})
            ),
            "bad_address"
        );
        let old =
            (now - ChronoDuration::days(MAX_AGE_DAYS) - ChronoDuration::seconds(1)).to_rfc3339();
        assert_eq!(
            code(json!({"traceId": trace, "source": "grid", "kind": "node_lost", "at": old})),
            "too_old"
        );
        let ahead = (now + ChronoDuration::seconds(MAX_AHEAD_SECONDS + 1)).to_rfc3339();
        assert_eq!(
            code(json!({"traceId": trace, "source": "grid", "kind": "node_lost", "at": ahead})),
            "too_old"
        );
    }

    #[test]
    fn details_are_flat_and_clean() {
        let d = |v: Value| clean_detail(v.as_object().unwrap());
        let ok = d(json!({"node": "n\u{0}1", "mb": 2048, "retried": false})).unwrap();
        assert_eq!(ok["node"], "n1");
        assert!(d(json!({"nested": {"a": 1}})).is_err());
        assert!(d(json!({"list": [1]})).is_err());
        assert!(d(json!({"none": null})).is_err());
        assert!(d(json!({"": 1})).is_err());
        assert!(d(json!({"k".repeat(MAX_DETAIL_KEY_BYTES + 1): 1})).is_err());
    }

    #[test]
    fn detail_texts_cannot_forge_lines_or_hide_text() {
        let d = |v: Value| clean_detail(v.as_object().unwrap());
        let ok = d(json!({
            "node": "n1\nkind: browser_oom\r\tsource: grid",
            "who\u{202e}": "a\u{202e}b\u{200b}c\u{2066}d\u{1b}[31me\u{2028}f",
        }))
        .unwrap();
        assert_eq!(ok["node"], "n1 kind: browser_oom  source: grid");
        assert_eq!(ok["who"], "abcd [31me f");
        assert!(
            d(json!({"\u{200b}\u{202e}": 1})).is_err(),
            "a key of invisible characters only"
        );
    }

    #[test]
    fn dedup_keys_name_source_kind_time_and_address() {
        let at = Utc::now();
        let a = dedup_key(SignalSource::Grid, "node_lost", at, "t:abc");
        assert_eq!(a, dedup_key(SignalSource::Grid, "node_lost", at, "t:abc"));
        assert_ne!(a, dedup_key(SignalSource::K8s, "node_lost", at, "t:abc"));
        assert_ne!(a, dedup_key(SignalSource::Grid, "evicted", at, "t:abc"));
        assert_ne!(
            a,
            dedup_key(
                SignalSource::Grid,
                "node_lost",
                at + ChronoDuration::milliseconds(1),
                "t:abc"
            )
        );
        assert_ne!(a, dedup_key(SignalSource::Grid, "node_lost", at, "t:abd"));
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn the_attempt_cap_counts_per_attempt() {
        let mut counts = HashMap::new();
        let h = Hit {
            launch_id: Uuid::nil(),
            launch_created_at: Utc::now(),
            result_id: Uuid::nil(),
            attempt: 1,
        };
        for _ in 0..MAX_SIGNALS_PER_ATTEMPT {
            assert!(take_slot(&mut counts, &h));
        }
        assert!(!take_slot(&mut counts, &h));
        assert!(take_slot(&mut counts, &Hit { attempt: 2, ..h }));
    }
}
