//! Persisting parsed results: test cases, series, attempts and one result per series per
//! launch, in one transaction per chunk, idempotent by client key.
//!
//! A series seen again in the same launch (a retry inside one upload, or the same test in a
//! later upload) becomes another attempt, and its result is recomputed from all attempts: the
//! latest by start time, then by ingest order (Allure 3 semantics, [`fastproof_model::group_attempts`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use fastproof_model::{
    AttachmentRef, AttemptKey, BoundedText, Container, Description, Label, Link, Parameter,
    ParameterMode, ParsedResult, SourceFormat, Stage, Status, Step, group_attempts,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Store, codec, json_no_nul, no_nul, opt_no_nul, ts_ms};

/// The launch results are written into.
#[derive(Clone, Debug)]
pub struct LaunchRef {
    pub id: Uuid,
    pub project_id: Uuid,
    pub project_key: String,
    pub created_at: DateTime<Utc>,
}

/// One parsed attempt with its idempotency key (unique per launch).
#[derive(Clone, Debug)]
pub struct NewAttempt {
    pub client_key: String,
    pub result: ParsedResult,
}

/// What a persist call did.
#[derive(Clone, Debug, Default)]
pub struct PersistOutcome {
    /// Attempts written.
    pub accepted: u64,
    /// Attempts skipped because their client key was already stored.
    pub duplicates: u64,
    /// `running` lines replaced by a later line with the same client key in the same batch
    /// (applied, then superseded; not duplicates).
    pub superseded: u64,
    /// Client keys of the attempts written.
    pub accepted_keys: HashSet<String>,
    /// Results created or recomputed.
    pub results: u64,
}

/// Attempts of a transaction that hit a deadlock or a serialization failure, at most.
pub const TX_ATTEMPTS: u32 = 5;

/// `Some("deadlock")` or `Some("serialization")` when `e` is a PostgreSQL error after which
/// the whole transaction can simply run again (SQLSTATE 40P01 or 40001).
pub fn retryable(e: &anyhow::Error) -> Option<&'static str> {
    e.chain()
        .find_map(|c| match c.downcast_ref::<sqlx::Error>() {
            Some(sqlx::Error::Database(d)) => match d.code().as_deref() {
                Some("40P01") => Some("deadlock"),
                Some("40001") => Some("serialization"),
                _ => None,
            },
            _ => None,
        })
}

/// True when `e` may mean that a launch row is gone: a foreign key violation (SQLSTATE 23503)
/// or a row that was not found. Callers check that the launch is really gone before acting.
pub fn launch_maybe_gone(e: &anyhow::Error) -> bool {
    e.chain().any(|c| match c.downcast_ref::<sqlx::Error>() {
        Some(sqlx::Error::RowNotFound) => true,
        Some(sqlx::Error::Database(d)) => d.code().as_deref() == Some("23503"),
        _ => false,
    })
}

/// Counts a retried transaction (`fastproof_db_tx_retries_total{path, reason}`) and sleeps a
/// short jittered backoff that grows with `attempt` (about 20 ms, 40 ms, 80 ms, ...).
pub async fn retry_pause(path: &'static str, e: &anyhow::Error, attempt: u32) {
    let reason = retryable(e).unwrap_or("other");
    metrics::counter!("fastproof_db_tx_retries_total", "path" => path, "reason" => reason)
        .increment(1);
    tracing::debug!(path, reason, attempt, "retrying a transaction");
    let base = 20u64 << attempt.min(6).saturating_sub(1);
    let jitter = rand::random::<u64>() % base.max(1);
    tokio::time::sleep(Duration::from_millis(base + jitter)).await;
}

/// Per-attempt data kept as JSON next to the indexed columns.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AttemptDetail {
    #[serde(skip_serializing_if = "Description::is_empty")]
    pub description: Description,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<Parameter>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentRef>,
    pub stage: Stage,
    pub interrupted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    pub format: SourceFormat,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
}

/// Captured output of an attempt.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AttemptLogs {
    pub stdout: Option<BoundedText>,
    pub stderr: Option<BoundedText>,
}

/// Fixtures of a container.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ContainerFixtures {
    pub befores: Vec<Step>,
    pub afters: Vec<Step>,
}

/// Suite path of a result: Allure suite labels, else the class/package part of the full name.
pub fn suite_path(r: &ParsedResult) -> Vec<String> {
    let mut path: Vec<String> = ["parentSuite", "suite", "subSuite"]
        .iter()
        .filter_map(|n| r.label(n))
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect();
    if path.is_empty() {
        let full = r.identity.full_name.as_str();
        let cut = full
            .rfind('#')
            .or_else(|| full.rfind(" > "))
            .or_else(|| full.rfind("::"))
            .or_else(|| full.rfind('.'));
        if let Some(i) = cut
            && i > 0
        {
            path.push(full[..i].to_string());
        } else if let Some(f) = r
            .identity
            .source
            .file
            .as_deref()
            .or(r.source_file.as_deref())
        {
            path.push(f.to_string());
        }
    }
    path
}

/// The stored status: `running` while the result is open (stage running), else the status.
pub fn status_text(r: &ParsedResult) -> &'static str {
    if r.stage == Stage::Running {
        "running"
    } else {
        r.status.as_str()
    }
}

fn duration(r: &ParsedResult) -> Option<i64> {
    r.duration_ms.or(match (r.start, r.stop) {
        (Some(a), Some(b)) if b >= a => Some(b - a),
        _ => None,
    })
}

/// series id, attempt id, status, start, seq
type AttemptKeyRow = (Uuid, Uuid, String, Option<DateTime<Utc>>, i64);

struct Prepared {
    client_key: String,
    tc_hash: String,
    series_key: String,
    seq: i64,
    id: Uuid,
    r: ParsedResult,
}

fn display_parameters(ps: &[Parameter]) -> serde_json::Value {
    serde_json::Value::Array(
        ps.iter()
            .filter(|p| p.mode == ParameterMode::Default)
            .map(|p| serde_json::json!({"name": p.name, "value": p.value}))
            .collect(),
    )
}

impl Store {
    /// Writes attempts into a launch (one transaction) and recomputes the results of the series
    /// they belong to. Attachment references must already carry attachment row ids.
    pub async fn persist_attempts(
        &self,
        launch: &LaunchRef,
        upload_id: Option<Uuid>,
        attempts: Vec<NewAttempt>,
    ) -> anyhow::Result<PersistOutcome> {
        if attempts.is_empty() {
            return Ok(PersistOutcome::default());
        }
        let mut attempt = 0;
        loop {
            attempt += 1;
            let r = async {
                let mut tx = self.pool.begin().await?;
                let out = persist_attempts_in(&mut tx, launch, upload_id, attempts.clone()).await?;
                if out.accepted == 0 {
                    tx.rollback().await?;
                } else {
                    tx.commit().await?;
                }
                Ok::<_, anyhow::Error>(out)
            }
            .await;
            match r {
                Err(e) if attempt < TX_ATTEMPTS && retryable(&e).is_some() => {
                    retry_pause("persist", &e, attempt).await
                }
                r => return r,
            }
        }
    }

    /// Stores containers of a launch (fixtures attach to results at read time, whichever
    /// arrives first). A container uploaded again replaces its earlier copy.
    pub async fn upsert_containers(
        &self,
        launch: &LaunchRef,
        containers: &[Container],
    ) -> anyhow::Result<u64> {
        let mut uuids = Vec::new();
        let mut names: Vec<Option<String>> = Vec::new();
        let mut children = Vec::new();
        let mut fixtures = Vec::new();
        let mut starts: Vec<Option<i64>> = Vec::new();
        let mut seen = HashSet::new();
        for c in containers.iter().rev() {
            if c.befores.is_empty() && c.afters.is_empty() {
                continue;
            }
            if c.uuid.is_empty() || !seen.insert(c.uuid.as_str()) {
                continue;
            }
            uuids.push(no_nul(&c.uuid));
            names.push(opt_no_nul(c.name.as_deref()));
            let mut kids: Vec<String> = c.children.iter().map(|s| no_nul(s)).collect();
            kids.sort();
            kids.dedup();
            children.push(json_no_nul(serde_json::to_value(kids)?));
            fixtures.push(codec::encode(&ContainerFixtures {
                befores: c.befores.clone(),
                afters: c.afters.clone(),
            })?);
            starts.push(c.start);
        }
        if uuids.is_empty() {
            return Ok(0);
        }
        let r = sqlx::query(
            "insert into launch_containers (launch_id, uuid, name, children, fixtures_z, start_ms) \
             select $1, u, n, array(select jsonb_array_elements_text(c)), f, s \
             from unnest($2::text[], $3::text[], $4::jsonb[], $5::bytea[], $6::bigint[]) as t(u, n, c, f, s) \
             on conflict (launch_id, uuid) do update set name = excluded.name, children = excluded.children, \
               fixtures_z = excluded.fixtures_z, start_ms = excluded.start_ms",
        )
        .bind(launch.id)
        .bind(&uuids)
        .bind(&names)
        .bind(&children)
        .bind(&fixtures)
        .bind(&starts)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }
}

/// Removes ANSI escape sequences (colours, cursor moves, OSC titles and links) from text.
/// Error messages and traces are shown, searched, fingerprinted and compared without them;
/// captured output (stdout, stderr, logs) keeps them for the console.
pub fn strip_ansi(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('\u{1b}') && !s.contains('\u{9b}') {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    skip_csi(&mut chars);
                }
                Some(']') => {
                    chars.next();
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' {
                            break;
                        }
                        if n == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                Some(n) if ('@'..='_').contains(n) || ('('..=')').contains(n) => {
                    let n = *n;
                    chars.next();
                    if n == '(' || n == ')' {
                        chars.next();
                    }
                }
                _ => {}
            },
            '\u{9b}' => skip_csi(&mut chars),
            c => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

fn skip_csi(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for n in chars.by_ref() {
        if ('@'..='~').contains(&n) {
            break;
        }
    }
}

fn strip_opt(v: &mut Option<String>) {
    if let Some(t) = v
        && let std::borrow::Cow::Owned(clean) = strip_ansi(t)
    {
        *t = clean;
    }
}

fn clean_steps(steps: &mut [Step]) {
    for s in steps {
        strip_opt(&mut s.status_details.message);
        strip_opt(&mut s.status_details.trace);
        clean_steps(&mut s.steps);
    }
}

/// Strips ANSI escapes from the error texts of a result and its steps.
fn clean_error_text(r: &mut ParsedResult) {
    let d = &mut r.status_details;
    strip_opt(&mut d.message);
    strip_opt(&mut d.trace);
    strip_opt(&mut d.expected);
    strip_opt(&mut d.actual);
    clean_steps(&mut r.steps);
}

/// After running attempts moved to another series: deletes the results of `vacated` series
/// (series id to result id) that have no attempts left in the launch, with their series and
/// test cases when nothing else uses them, and refreshes the outcome of the others from their
/// latest remaining attempt. Returns `(series, result, still has attempts)`.
async fn refresh_vacated(
    conn: &mut sqlx::PgConnection,
    launch: &LaunchRef,
    vacated: &HashMap<Uuid, Uuid>,
) -> anyhow::Result<Vec<(Uuid, Uuid, bool)>> {
    if vacated.is_empty() {
        return Ok(Vec::new());
    }
    let mut series: Vec<Uuid> = vacated.keys().copied().collect();
    series.sort();
    let remaining: HashSet<Uuid> = sqlx::query_scalar::<_, Uuid>(
        "select distinct series_id from attempts \
         where launch_id = $1 and launch_created_at = $2 and series_id = any($3)",
    )
    .bind(launch.id)
    .bind(launch.created_at)
    .bind(&series)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    let mut out = Vec::with_capacity(series.len());
    let mut gone_results = Vec::new();
    let mut gone_series = Vec::new();
    let mut kept_results = Vec::new();
    for s in &series {
        let r = vacated[s];
        let remains = remaining.contains(s);
        out.push((*s, r, remains));
        if remains {
            kept_results.push(r);
        } else {
            gone_results.push(r);
            gone_series.push(*s);
        }
    }
    if !kept_results.is_empty() {
        sqlx::query(
            "update results r set status = la.status, started_at = la.started_at, stopped_at = la.stopped_at, \
               duration_ms = la.duration_ms, message = la.message, updated_at = now() \
             from (select distinct on (a.result_id) a.result_id, a.status, a.started_at, a.stopped_at, \
                 a.duration_ms, a.message from attempts a \
               where a.result_id = any($1) and a.launch_created_at = $2 \
               order by a.result_id, (a.started_at is not null) desc, a.started_at desc nulls last, a.seq desc) la \
             where r.id = la.result_id and r.launch_created_at = $2",
        )
        .bind(&kept_results)
        .bind(launch.created_at)
        .execute(&mut *conn)
        .await?;
    }
    if gone_results.is_empty() {
        return Ok(out);
    }
    sqlx::query("delete from results where id = any($1) and launch_created_at = $2")
        .bind(&gone_results)
        .bind(launch.created_at)
        .execute(&mut *conn)
        .await?;
    // Lock before checking, so a result another launch adds meanwhile is seen (or that launch
    // waits and creates the series again).
    let locked: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "select id, test_case_id from series where id = any($1) order by id for update",
    )
    .bind(&gone_series)
    .fetch_all(&mut *conn)
    .await?;
    if locked.is_empty() {
        return Ok(out);
    }
    let dropped: Vec<Uuid> = sqlx::query_scalar(
        "delete from series s where s.id = any($1) \
           and not exists (select 1 from results r where r.series_id = s.id) \
           and not exists (select 1 from attempts a where a.series_id = s.id and a.launch_id = $2) \
         returning s.id",
    )
    .bind(&gone_series)
    .bind(launch.id)
    .fetch_all(&mut *conn)
    .await?;
    if dropped.is_empty() {
        return Ok(out);
    }
    for table in ["series_stats", "series_daily", "series_mutes"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "delete from {table} where series_id = any($1)"
        )))
        .bind(&dropped)
        .execute(&mut *conn)
        .await?;
    }
    let mut cases: Vec<Uuid> = locked
        .iter()
        .filter(|(s, _)| dropped.contains(s))
        .map(|(_, t)| *t)
        .collect();
    cases.sort();
    cases.dedup();
    sqlx::query("select id from test_cases where id = any($1) order by id for update")
        .bind(&cases)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "delete from test_cases t where t.id = any($1) \
           and not exists (select 1 from series s where s.test_case_id = t.id)",
    )
    .bind(&cases)
    .execute(&mut *conn)
    .await?;
    Ok(out)
}

/// As [`Store::persist_attempts`], inside the caller's transaction (the Kafka writer commits its
/// consumer offset in the same transaction). Nothing is written when no attempt is new; the
/// caller decides whether to commit.
/// Recomputes the counters of a launch: the automated ones from its results (the first and
/// last result times from automated results only, so manual work does not stretch the launch
/// duration), the manual ones from its run items; sets `attempt_seq` when given. The caller
/// holds the launch row lock (taken before this statement), so no manual write adds a
/// counter change that this count misses (`tm::projection::apply_launch_changes`).
pub(crate) async fn recount(
    conn: &mut sqlx::PgConnection,
    launch_id: Uuid,
    launch_created_at: chrono::DateTime<chrono::Utc>,
    attempt_seq: Option<i64>,
) -> anyhow::Result<()> {
    sqlx::query(
        "update launches l set attempt_seq = coalesce($2, l.attempt_seq), updated_at = now(), count_total = c.total, \
           count_passed = c.passed, count_failed = c.failed, count_broken = c.broken, \
           count_skipped = c.skipped, count_unknown = c.unknown, count_flaky = c.flaky, \
           count_muted = c.muted, first_result_at = c.mn, last_result_at = c.mx, \
           count_untested = m.untested, count_in_progress = m.in_progress \
         from (select count(*)::int as total, count(*) filter (where status = 'passed')::int as passed, \
           count(*) filter (where status = 'failed')::int as failed, \
           count(*) filter (where status = 'broken')::int as broken, \
           count(*) filter (where status = 'skipped')::int as skipped, \
           count(*) filter (where status = 'unknown')::int as unknown, \
           count(*) filter (where flaky_in_run)::int as flaky, count(*) filter (where muted)::int as muted, \
           min(started_at) filter (where run_item_id is null) as mn, \
           max(coalesce(stopped_at, started_at)) filter (where run_item_id is null) as mx \
           from results where launch_id = $1 and launch_created_at = $3) c, \
         (select count(*) filter (where status = 'untested')::int as untested, \
           count(*) filter (where status = 'in_progress')::int as in_progress \
           from run_items where launch_id = $1) m \
         where l.id = $1",
    )
    .bind(launch_id)
    .bind(attempt_seq)
    .bind(launch_created_at)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

impl Store {
    /// Recomputes the counters of a launch (automated from `results`, manual `count_untested`
    /// and `count_in_progress` from `run_items`). Run it in the transaction that changed them,
    /// after locking the launch row: manual writes change the counters incrementally, and a
    /// count taken before waiting for their lock would overwrite their change.
    pub async fn recount_launch(
        conn: &mut sqlx::PgConnection,
        launch_id: Uuid,
    ) -> anyhow::Result<()> {
        let created_at: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("select created_at from launches where id = $1")
                .bind(launch_id)
                .fetch_optional(&mut *conn)
                .await?;
        match created_at {
            Some(at) => recount(conn, launch_id, at, None).await,
            None => Ok(()),
        }
    }
}

/// A stored test case: id, hash, display name, source file and line, last seen.
type KnownTestCase = (
    Uuid,
    String,
    String,
    Option<String>,
    Option<i32>,
    DateTime<Utc>,
);

/// The items of `v` at `at`.
fn pick<T: Clone>(v: &[T], at: &[usize]) -> Vec<T> {
    at.iter().map(|&i| v[i].clone()).collect()
}

pub async fn persist_attempts_in(
    conn: &mut sqlx::PgConnection,
    launch: &LaunchRef,
    upload_id: Option<Uuid>,
    attempts: Vec<NewAttempt>,
) -> anyhow::Result<PersistOutcome> {
    let mut out = PersistOutcome::default();
    if attempts.is_empty() {
        return Ok(out);
    }
    // `no key update` serializes the writers of this launch without blocking the foreign key
    // checks of rows that reference it (log chunks of the launch written meanwhile).
    let seq0: i64 =
        sqlx::query_scalar("select attempt_seq from launches where id = $1 for no key update")
            .bind(launch.id)
            .fetch_one(&mut *conn)
            .await?;

    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut prepared: Vec<Prepared> = Vec::with_capacity(attempts.len());
    for mut a in attempts {
        clean_error_text(&mut a.result);
        if let Some(&i) = seen.get(&a.client_key) {
            // A running result completed (or updated) within the same batch: the client id wins,
            // so the newer copy replaces it whatever identity it carries.
            if prepared[i].r.stage == Stage::Running {
                prepared[i].tc_hash = a.result.identity.test_case_hash.clone();
                prepared[i].series_key = a.result.identity.series_key.clone();
                prepared[i].r = a.result;
                out.superseded += 1;
            } else {
                out.duplicates += 1;
            }
            continue;
        }
        seen.insert(a.client_key.clone(), prepared.len());
        let seq = seq0 + prepared.len() as i64 + 1;
        prepared.push(Prepared {
            client_key: no_nul(&a.client_key),
            tc_hash: a.result.identity.test_case_hash.clone(),
            series_key: a.result.identity.series_key.clone(),
            seq,
            id: Uuid::now_v7(),
            r: a.result,
        });
    }

    // Test cases, in key order: concurrent launches of a project upsert the same rows, and
    // taking their locks in one order keeps them from deadlocking.
    let mut tc_index: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, p) in prepared.iter().enumerate() {
        tc_index.insert(p.tc_hash.as_str(), i);
    }
    let now = Utc::now();
    let mut tc_ids = Vec::new();
    let mut tc_hashes = Vec::new();
    let mut tc_full = Vec::new();
    let mut tc_display = Vec::new();
    let mut tc_file: Vec<Option<String>> = Vec::new();
    let mut tc_line: Vec<Option<i32>> = Vec::new();
    let mut tc_code: Vec<Option<String>> = Vec::new();
    let mut tc_explicit: Vec<Option<String>> = Vec::new();
    let mut tc_explicit_src: Vec<Option<String>> = Vec::new();
    for &i in tc_index.values() {
        let id = &prepared[i].r.identity;
        tc_ids.push(Uuid::now_v7());
        tc_hashes.push(no_nul(&id.test_case_hash));
        tc_full.push(no_nul(&id.full_name));
        tc_display.push(no_nul(&id.display_name));
        tc_file.push(opt_no_nul(id.source.file.as_deref()));
        tc_line.push(id.source.line.map(|l| l.min(i32::MAX as u32) as i32));
        tc_code.push(opt_no_nul(id.source.code_ref.as_deref()));
        tc_explicit.push(id.explicit_id.as_ref().map(|e| no_nul(&e.value)));
        tc_explicit_src.push(id.explicit_id.as_ref().map(|e| {
            serde_json::to_value(e.source)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
        }));
    }
    // Test cases that exist and need no change are only share-locked: `for key share` keeps
    // them from being deleted (`refresh_vacated`) without waiting for, or blocking, other
    // launches that use them. An upsert would lock them until commit even when it changes
    // nothing, which made launches sharing tests wait for each other's whole transaction.
    let known: Vec<KnownTestCase> = sqlx::query_as(
            "select id, test_case_hash, display_name, source_file, source_line, last_seen_at from test_cases \
             where project_id = $1 and test_case_hash = any($2) order by id for key share",
        )
        .bind(launch.project_id)
        .bind(&tc_hashes)
        .fetch_all(&mut *conn)
        .await?;
    let stale = now - chrono::Duration::hours(1);
    let mut tc_by_hash: HashMap<String, Uuid> = HashMap::with_capacity(tc_hashes.len());
    let mut current: HashSet<String> = HashSet::with_capacity(known.len());
    {
        let at: HashMap<&str, usize> = tc_hashes
            .iter()
            .enumerate()
            .map(|(i, h)| (h.as_str(), i))
            .collect();
        for (id, hash, display, file, line, seen) in known {
            let Some(&i) = at.get(hash.as_str()) else {
                continue;
            };
            let changed = display != tc_display[i]
                || (tc_file[i].is_some() && file != tc_file[i])
                || (tc_line[i].is_some() && line != tc_line[i])
                || seen < stale;
            if !changed {
                current.insert(hash.clone());
            }
            tc_by_hash.insert(hash, id);
        }
    }
    let upsert: Vec<usize> = (0..tc_hashes.len())
        .filter(|&i| !current.contains(&tc_hashes[i]))
        .collect();
    if !upsert.is_empty() {
        let u_ids: Vec<Uuid> = upsert.iter().map(|&i| tc_ids[i]).collect();
        let u_hashes: Vec<String> = pick(&tc_hashes, &upsert);
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "insert into test_cases (id, project_id, test_case_hash, full_name, display_name, source_file, \
               source_line, code_ref, explicit_id, explicit_id_source, first_seen_at, last_seen_at) \
             select i, $1, h, f, d, sf, sl, cr, e, es, $11, $11 from unnest($2::uuid[], $3::text[], $4::text[], \
               $5::text[], $6::text[], $7::int[], $8::text[], $9::text[], $10::text[]) as t(i, h, f, d, sf, sl, cr, e, es) \
             on conflict (project_id, test_case_hash) do update set last_seen_at = excluded.last_seen_at, \
               display_name = excluded.display_name, \
               source_file = coalesce(excluded.source_file, test_cases.source_file), \
               source_line = coalesce(excluded.source_line, test_cases.source_line) \
             where test_cases.display_name is distinct from excluded.display_name \
               or (excluded.source_file is not null and test_cases.source_file is distinct from excluded.source_file) \
               or (excluded.source_line is not null and test_cases.source_line is distinct from excluded.source_line) \
               or test_cases.last_seen_at < excluded.last_seen_at - interval '1 hour' \
             returning id, test_case_hash",
        )
        .bind(launch.project_id)
        .bind(&u_ids)
        .bind(&u_hashes)
        .bind(pick(&tc_full, &upsert))
        .bind(pick(&tc_display, &upsert))
        .bind(pick(&tc_file, &upsert))
        .bind(pick(&tc_line, &upsert))
        .bind(pick(&tc_code, &upsert))
        .bind(pick(&tc_explicit, &upsert))
        .bind(pick(&tc_explicit_src, &upsert))
        .bind(now)
        .fetch_all(&mut *conn)
        .await?;
        tc_by_hash.extend(rows.into_iter().map(|(i, h)| (h, i)));
    }
    // Rows left unchanged by the upsert (another launch inserted them meanwhile): read their ids.
    let unchanged: Vec<&String> = tc_hashes
        .iter()
        .filter(|h| !tc_by_hash.contains_key(*h))
        .collect();
    if !unchanged.is_empty() {
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "select id, test_case_hash from test_cases where project_id = $1 and test_case_hash = any($2)",
        )
        .bind(launch.project_id)
        .bind(&unchanged)
        .fetch_all(&mut *conn)
        .await?;
        tc_by_hash.extend(rows.into_iter().map(|(i, h)| (h, i)));
    }

    // Series, in key order (as the test cases).
    let mut se_index: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, p) in prepared.iter().enumerate() {
        se_index.insert(p.series_key.as_str(), i);
    }
    let mut se_ids = Vec::new();
    let mut se_tc = Vec::new();
    let mut se_keys = Vec::new();
    let mut se_phash = Vec::new();
    let mut se_env: Vec<Option<String>> = Vec::new();
    let mut se_params = Vec::new();
    for &i in se_index.values() {
        let p = &prepared[i];
        let tc = *tc_by_hash
            .get(&no_nul(&p.tc_hash))
            .ok_or_else(|| anyhow::anyhow!("test case row missing"))?;
        se_ids.push(Uuid::now_v7());
        se_tc.push(tc);
        se_keys.push(no_nul(&p.series_key));
        se_phash.push(no_nul(&p.r.identity.parameters_hash));
        se_env.push(opt_no_nul(p.r.identity.env_key.as_deref()));
        se_params.push(json_no_nul(display_parameters(&p.r.parameters)));
    }
    // Series as the test cases: existing ones are share-locked, new or stale ones upserted.
    let known: Vec<(Uuid, String, Uuid, DateTime<Utc>)> = sqlx::query_as(
        "select id, series_key, test_case_id, last_seen_at from series \
         where project_id = $1 and series_key = any($2) order by id for key share",
    )
    .bind(launch.project_id)
    .bind(&se_keys)
    .fetch_all(&mut *conn)
    .await?;
    let mut series_by_key: HashMap<String, (Uuid, Uuid)> = HashMap::with_capacity(se_keys.len());
    let mut current: HashSet<String> = HashSet::with_capacity(known.len());
    for (id, key, tc, seen) in known {
        if seen >= stale {
            current.insert(key.clone());
        }
        series_by_key.insert(key, (id, tc));
    }
    let upsert: Vec<usize> = (0..se_keys.len())
        .filter(|&i| !current.contains(&se_keys[i]))
        .collect();
    if !upsert.is_empty() {
        let u_ids: Vec<Uuid> = upsert.iter().map(|&i| se_ids[i]).collect();
        let u_tc: Vec<Uuid> = upsert.iter().map(|&i| se_tc[i]).collect();
        let rows: Vec<(Uuid, String, Uuid)> = sqlx::query_as(
            "insert into series (id, project_id, test_case_id, series_key, parameters_hash, env_key, parameters, \
               first_seen_at, last_seen_at) \
             select i, $1, tc, k, ph, e, p, $8, $8 from unnest($2::uuid[], $3::uuid[], $4::text[], $5::text[], \
               $6::text[], $7::jsonb[]) as t(i, tc, k, ph, e, p) \
             on conflict (project_id, series_key) do update set last_seen_at = excluded.last_seen_at \
             where series.last_seen_at < excluded.last_seen_at - interval '1 hour' \
             returning id, series_key, test_case_id",
        )
        .bind(launch.project_id)
        .bind(&u_ids)
        .bind(&u_tc)
        .bind(pick(&se_keys, &upsert))
        .bind(pick(&se_phash, &upsert))
        .bind(pick(&se_env, &upsert))
        .bind(pick(&se_params, &upsert))
        .bind(now)
        .fetch_all(&mut *conn)
        .await?;
        series_by_key.extend(rows.into_iter().map(|(i, k, tc)| (k, (i, tc))));
    }
    let unchanged: Vec<&String> = se_keys
        .iter()
        .filter(|k| !series_by_key.contains_key(*k))
        .collect();
    if !unchanged.is_empty() {
        let rows: Vec<(Uuid, String, Uuid)> = sqlx::query_as(
            "select id, series_key, test_case_id from series where project_id = $1 and series_key = any($2)",
        )
        .bind(launch.project_id)
        .bind(&unchanged)
        .fetch_all(&mut *conn)
        .await?;
        series_by_key.extend(rows.into_iter().map(|(i, k, tc)| (k, (i, tc))));
    }
    let series_ids: Vec<Uuid> = series_by_key.values().map(|v| v.0).collect();
    let mut tc_of_series: HashMap<Uuid, Uuid> = series_by_key.values().copied().collect();

    // Results that exist for these series.
    let existing: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "select id, series_id from results where launch_id = $1 and launch_created_at = $2 and series_id = any($3)",
        )
        .bind(launch.id)
        .bind(launch.created_at)
        .bind(&series_ids)
        .fetch_all(&mut *conn)
        .await?;
    let mut result_of: HashMap<Uuid, Uuid> = existing.into_iter().map(|(r, s)| (s, r)).collect();
    for s in &series_ids {
        result_of.entry(*s).or_insert_with(Uuid::now_v7);
    }

    // Attempts stored as `running` under another identity: the client id wins, so they move to
    // the series of the line that completes them and their old results are recomputed.
    let keys: Vec<String> = prepared.iter().map(|p| p.client_key.clone()).collect();
    let open_before: HashMap<String, (Uuid, Uuid)> = sqlx::query_as::<_, (String, Uuid, Uuid)>(
        "select client_key, series_id, result_id from attempts \
         where launch_id = $1 and launch_created_at = $2 and client_key = any($3) and status = 'running'",
    )
    .bind(launch.id)
    .bind(launch.created_at)
    .bind(&keys)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|(k, s, r)| (k, (s, r)))
    .collect();

    // Attempts.
    let n = prepared.len();
    let mut a_id = Vec::with_capacity(n);
    let mut a_result = Vec::with_capacity(n);
    let mut a_series = Vec::with_capacity(n);
    let mut a_key = Vec::with_capacity(n);
    let mut a_seq = Vec::with_capacity(n);
    let mut a_status = Vec::with_capacity(n);
    let mut a_raw: Vec<Option<String>> = Vec::with_capacity(n);
    let mut a_start: Vec<Option<DateTime<Utc>>> = Vec::with_capacity(n);
    let mut a_stop: Vec<Option<DateTime<Utc>>> = Vec::with_capacity(n);
    let mut a_dur: Vec<Option<i64>> = Vec::with_capacity(n);
    let mut a_msg: Vec<Option<String>> = Vec::with_capacity(n);
    let mut a_trace: Vec<Option<String>> = Vec::with_capacity(n);
    let mut a_uuid: Vec<Option<String>> = Vec::with_capacity(n);
    let mut a_trace_id: Vec<Option<String>> = Vec::with_capacity(n);
    let mut a_detail = Vec::with_capacity(n);
    let mut a_steps: Vec<Option<Vec<u8>>> = Vec::with_capacity(n);
    let mut a_logs: Vec<Option<Vec<u8>>> = Vec::with_capacity(n);
    for p in &prepared {
        let r = &p.r;
        let (series_id, _) = series_by_key
            .get(&no_nul(&p.series_key))
            .copied()
            .ok_or_else(|| anyhow::anyhow!("series row missing"))?;
        a_id.push(p.id);
        a_result.push(result_of[&series_id]);
        a_series.push(series_id);
        a_key.push(p.client_key.clone());
        a_seq.push(p.seq);
        a_status.push(status_text(r).to_string());
        a_raw.push(opt_no_nul(r.raw_status.as_deref()));
        a_start.push(ts_ms(r.start));
        a_stop.push(ts_ms(r.stop));
        a_dur.push(duration(r));
        a_msg.push(opt_no_nul(r.status_details.message.as_deref()));
        a_trace.push(opt_no_nul(r.status_details.trace.as_deref()));
        a_uuid.push(opt_no_nul(r.source_uuid.as_deref()));
        a_trace_id.push(opt_no_nul(r.trace_id.as_deref()));
        let detail = AttemptDetail {
            description: r.description.clone(),
            parameters: r.parameters.clone(),
            links: r.links.clone(),
            actual: r.status_details.actual.clone(),
            expected: r.status_details.expected.clone(),
            attachments: r.attachments.clone(),
            stage: r.stage,
            interrupted: r.interrupted,
            attempt: r.attempt,
            format: r.format,
            source_file: r.source_file.clone(),
        };
        a_detail.push(json_no_nul(serde_json::to_value(&detail)?));
        a_steps.push(if r.steps.is_empty() {
            None
        } else {
            Some(codec::encode(&r.steps)?)
        });
        a_logs.push(if r.stdout.is_none() && r.stderr.is_none() {
            None
        } else {
            Some(codec::encode(&AttemptLogs {
                stdout: r.stdout.clone(),
                stderr: r.stderr.clone(),
            })?)
        });
    }
    let inserted: Vec<(Uuid, String)> = sqlx::query_as(
            "insert into attempts (id, launch_created_at, project_id, launch_id, result_id, series_id, upload_id, \
               client_key, seq, status, raw_status, started_at, stopped_at, duration_ms, message, trace, source_uuid, \
               trace_id, detail, steps_z, logs_z) \
             select i, $1, $2, $3, r, s, $4, k, q, st, rs, sa, so, d, m, tr, su, ti, de, sz, lz \
             from unnest($5::uuid[], $6::uuid[], $7::uuid[], $8::text[], $9::bigint[], $10::text[], $11::text[], \
               $12::timestamptz[], $13::timestamptz[], $14::bigint[], $15::text[], $16::text[], $17::text[], \
               $18::text[], $19::jsonb[], $20::bytea[], $21::bytea[]) \
               as t(i, r, s, k, q, st, rs, sa, so, d, m, tr, su, ti, de, sz, lz) \
             on conflict (launch_id, client_key, launch_created_at) do update set status = excluded.status, \
               result_id = excluded.result_id, series_id = excluded.series_id, \
               raw_status = excluded.raw_status, started_at = excluded.started_at, stopped_at = excluded.stopped_at, \
               duration_ms = excluded.duration_ms, message = excluded.message, trace = excluded.trace, \
               source_uuid = excluded.source_uuid, trace_id = excluded.trace_id, detail = excluded.detail, \
               steps_z = excluded.steps_z, logs_z = excluded.logs_z, upload_id = excluded.upload_id \
               where attempts.status = 'running' \
             returning id, client_key",
        )
        .bind(launch.created_at)
        .bind(launch.project_id)
        .bind(launch.id)
        .bind(upload_id)
        .bind(&a_id)
        .bind(&a_result)
        .bind(&a_series)
        .bind(&a_key)
        .bind(&a_seq)
        .bind(&a_status)
        .bind(&a_raw)
        .bind(&a_start)
        .bind(&a_stop)
        .bind(&a_dur)
        .bind(&a_msg)
        .bind(&a_trace)
        .bind(&a_uuid)
        .bind(&a_trace_id)
        .bind(&a_detail)
        .bind(&a_steps)
        .bind(&a_logs)
        .fetch_all(&mut *conn)
        .await?;
    // A result reported as running and sent again keeps its attempt row (id and order).
    let stored_id: HashMap<String, Uuid> = inserted.into_iter().map(|(i, k)| (k, i)).collect();
    for p in prepared.iter_mut() {
        if let Some(id) = stored_id.get(&p.client_key) {
            p.id = *id;
        }
    }
    let accepted: HashSet<String> = stored_id.into_keys().collect();
    out.accepted = accepted.len() as u64;
    out.duplicates += (prepared.len() - accepted.len()) as u64;
    if accepted.is_empty() {
        return Ok(out);
    }

    // Recompute the results of the affected series from all their attempts.
    let mut affected: HashSet<Uuid> = prepared
        .iter()
        .zip(a_series.iter())
        .filter(|(p, _)| accepted.contains(&p.client_key))
        .map(|(_, s)| *s)
        .collect();
    // Series that running attempts moved out of.
    let mut vacated: HashMap<Uuid, Uuid> = HashMap::new();
    for (p, s) in prepared.iter().zip(a_series.iter()) {
        if let Some((old_series, old_result)) = open_before.get(&p.client_key)
            && accepted.contains(&p.client_key)
            && old_series != s
        {
            vacated.insert(*old_series, *old_result);
        }
    }
    let vacated_rows = refresh_vacated(&mut *conn, launch, &vacated).await?;
    let mut removed_events = Vec::new();
    for (series_id, result_id, remains) in &vacated_rows {
        if *remains {
            affected.insert(*series_id);
            result_of.insert(*series_id, *result_id);
        } else {
            removed_events.push(serde_json::json!({"id": result_id, "launchId": launch.id,
                "seriesId": series_id, "removed": true}));
        }
    }
    let kept: Vec<Uuid> = vacated_rows
        .iter()
        .filter(|v| v.2 && !tc_of_series.contains_key(&v.0))
        .map(|v| v.0)
        .collect();
    if !kept.is_empty() {
        let rows: Vec<(Uuid, Uuid)> =
            sqlx::query_as("select id, test_case_id from series where id = any($1)")
                .bind(&kept)
                .fetch_all(&mut *conn)
                .await?;
        tc_of_series.extend(rows);
    }
    let affected_v: Vec<Uuid> = affected.iter().copied().collect();
    let mut all: Vec<AttemptKeyRow> = sqlx::query_as(
        "select series_id, id, status, started_at, seq from attempts \
             where launch_id = $1 and launch_created_at = $2 and series_id = any($3)",
    )
    .bind(launch.id)
    .bind(launch.created_at)
    .bind(&affected_v)
    .fetch_all(&mut *conn)
    .await?;
    all.sort_by_key(|a| a.4);
    let series_keys: Vec<String> = all.iter().map(|a| a.0.to_string()).collect();
    let keys: Vec<AttemptKey<'_>> = all
        .iter()
        .zip(series_keys.iter())
        .map(|(a, k)| AttemptKey {
            series_key: k.as_str(),
            start: a.3.map(|t| t.timestamp_millis()),
            status: Status::parse_lenient(&a.2),
        })
        .collect();
    let groups = group_attempts(&keys);
    let by_attempt: HashMap<Uuid, usize> = prepared
        .iter()
        .enumerate()
        .filter(|(_, p)| accepted.contains(&p.client_key))
        .map(|(i, p)| (p.id, i))
        .collect();

    let mut full = (
        Vec::<Uuid>::new(),
        Vec::<Uuid>::new(),
        Vec::<Uuid>::new(),
        Vec::<String>::new(),
        Vec::<String>::new(),
        Vec::<serde_json::Value>::new(),
        Vec::<String>::new(),
        Vec::<bool>::new(),
        Vec::<i32>::new(),
    );
    let mut times = (
        Vec::<Option<DateTime<Utc>>>::new(),
        Vec::<Option<DateTime<Utc>>>::new(),
        Vec::<Option<i64>>::new(),
        Vec::<Option<String>>::new(),
        Vec::<serde_json::Value>::new(),
    );
    let mut partial = (Vec::<Uuid>::new(), Vec::<i32>::new(), Vec::<bool>::new());
    let mut events = Vec::new();
    for g in &groups {
        let latest = &all[g.latest];
        let series_id = latest.0;
        let result_id = result_of[&series_id];
        let count = g.attempts.len() as i32;
        let new_attempts: Vec<serde_json::Value> = g
            .attempts
            .iter()
            .filter_map(|&i| by_attempt.get(&all[i].1).map(|&p| (all[i].1, p)))
            .map(|(id, p)| {
                let r = &prepared[p].r;
                serde_json::json!({"id": id, "status": status_text(r), "startedAt": ts_ms(r.start),
                        "durationMs": duration(r)})
            })
            .collect();
        let test_case_id = tc_of_series.get(&series_id).copied().unwrap_or_default();
        if let Some(&pi) = by_attempt.get(&latest.1) {
            let r = &prepared[pi].r;
            full.0.push(result_id);
            full.1.push(test_case_id);
            full.2.push(series_id);
            full.3.push(no_nul(&r.name));
            full.4.push(no_nul(&r.full_name));
            full.5
                .push(json_no_nul(serde_json::to_value(suite_path(r))?));
            full.6.push(status_text(r).to_string());
            full.7.push(g.flaky_in_run);
            full.8.push(count);
            times.0.push(ts_ms(r.start));
            times.1.push(ts_ms(r.stop));
            times.2.push(duration(r));
            times
                .3
                .push(opt_no_nul(r.status_details.message.as_deref()));
            times.4.push(json_no_nul(serde_json::to_value(
                r.labels
                    .iter()
                    .map(|l: &Label| serde_json::json!({"name": l.name, "value": l.value}))
                    .collect::<Vec<_>>(),
            )?));
            events.push(serde_json::json!({"id": result_id, "launchId": launch.id, "seriesId": series_id,
                    "testCaseId": test_case_id, "status": status_text(r), "flakyInRun": g.flaky_in_run,
                    "attemptCount": count, "startedAt": ts_ms(r.start), "durationMs": duration(r),
                    "uploadId": upload_id, "attempts": new_attempts}));
        } else {
            partial.0.push(result_id);
            partial.1.push(count);
            partial.2.push(g.flaky_in_run);
            events.push(
                serde_json::json!({"id": result_id, "launchId": launch.id, "seriesId": series_id,
                    "testCaseId": test_case_id, "status": latest.2, "flakyInRun": g.flaky_in_run,
                    "attemptCount": count, "uploadId": upload_id, "attempts": new_attempts}),
            );
        }
    }
    if !full.0.is_empty() {
        sqlx::query(
                "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, \
                   full_name, suite_path, status, flaky_in_run, attempt_count, started_at, stopped_at, duration_ms, \
                   message, labels) \
                 select i, $1, $2, $3, tc, se, n, f, array(select jsonb_array_elements_text(sp)), st, fl, ac, sa, so, \
                   d, m, lb \
                 from unnest($4::uuid[], $5::uuid[], $6::uuid[], $7::text[], $8::text[], $9::jsonb[], $10::text[], \
                   $11::bool[], $12::int[], $13::timestamptz[], $14::timestamptz[], $15::bigint[], $16::text[], \
                   $17::jsonb[]) as t(i, tc, se, n, f, sp, st, fl, ac, sa, so, d, m, lb) \
                 on conflict (launch_id, series_id, launch_created_at) do update set name = excluded.name, \
                   full_name = excluded.full_name, suite_path = excluded.suite_path, status = excluded.status, \
                   flaky_in_run = excluded.flaky_in_run, attempt_count = excluded.attempt_count, \
                   started_at = excluded.started_at, stopped_at = excluded.stopped_at, \
                   duration_ms = excluded.duration_ms, message = excluded.message, labels = excluded.labels, \
                   updated_at = now()",
            )
            .bind(launch.created_at)
            .bind(launch.project_id)
            .bind(launch.id)
            .bind(&full.0)
            .bind(&full.1)
            .bind(&full.2)
            .bind(&full.3)
            .bind(&full.4)
            .bind(&full.5)
            .bind(&full.6)
            .bind(&full.7)
            .bind(&full.8)
            .bind(&times.0)
            .bind(&times.1)
            .bind(&times.2)
            .bind(&times.3)
            .bind(&times.4)
            .execute(&mut *conn)
            .await?;
    }
    if !partial.0.is_empty() {
        sqlx::query(
            "update results r set attempt_count = t.ac, flaky_in_run = t.fl, updated_at = now() \
                 from unnest($1::uuid[], $2::int[], $3::bool[]) as t(i, ac, fl) \
                 where r.id = t.i and r.launch_created_at = $4",
        )
        .bind(&partial.0)
        .bind(&partial.1)
        .bind(&partial.2)
        .bind(launch.created_at)
        .execute(&mut *conn)
        .await?;
    }
    out.results = groups.len() as u64;
    let stored: Vec<serde_json::Value> = prepared
        .iter()
        .zip(a_series.iter())
        .filter(|(p, _)| accepted.contains(&p.client_key))
        .map(|(p, series)| {
            serde_json::json!({"id": p.id, "resultId": result_of[series], "launchId": launch.id,
                    "seriesId": series, "status": status_text(&p.r)})
        })
        .collect();
    let analyzed: Vec<crate::analysis::PersistedAttempt<'_>> = prepared
        .iter()
        .filter(|p| accepted.contains(&p.client_key))
        .map(|p| crate::analysis::PersistedAttempt {
            id: p.id,
            status: p.r.status,
            message: p.r.status_details.message.as_deref(),
            trace: p.r.status_details.trace.as_deref(),
        })
        .collect();
    crate::analysis::after_persist(&mut *conn, launch, &analyzed, &affected_v).await?;
    drop(analyzed);
    out.accepted_keys = accepted;

    recount(
        &mut *conn,
        launch.id,
        launch.created_at,
        Some(seq0 + prepared.len() as i64),
    )
    .await?;
    // The outbox rows last: their ids are taken at insert, and the outbox reader waits at the
    // first id whose transaction is still open, so they go in right before the commit.
    crate::events::emit_many(
        &mut *conn,
        "result.updated",
        Some(&launch.project_key),
        events,
    )
    .await?;
    if !removed_events.is_empty() {
        crate::events::emit_many(
            &mut *conn,
            "result.removed",
            Some(&launch.project_key),
            removed_events,
        )
        .await?;
    }
    crate::events::emit_many(
        &mut *conn,
        "attempt.stored",
        Some(&launch.project_key),
        stored,
    )
    .await?;
    crate::events::notify_launch(&mut *conn, &launch.project_key, launch.id).await?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::strip_ansi;

    #[test]
    fn strips_escape_sequences() {
        assert_eq!(
            strip_ansi("Error: \u{1b}[2mexpect(\u{1b}[22m\u{1b}[31mlocator\u{1b}[39m)"),
            "Error: expect(locator)"
        );
        assert_eq!(
            strip_ansi("\u{1b}]8;;https://example.com\u{7}link\u{1b}]8;;\u{1b}\\ end"),
            "link end"
        );
        assert_eq!(strip_ansi("a\u{9b}1mb\u{1b}(Bc\u{1b}Md"), "abcd");
        assert_eq!(strip_ansi("trailing \u{1b}["), "trailing ");
        assert!(matches!(
            strip_ansi("plain é text"),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}
