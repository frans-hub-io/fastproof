//! The decision journal (contract `76` §7.1): one `triage_decisions` row per change of the
//! defect or mute of a result, from every path (REST, MCP, RP, rules at ingest, the retro task,
//! suggestions, auto-apply, undo); one request is one batch. The journal is core: it is
//! written with Analysis on or off. History and undo read it.
//!
//! States are kept as PostgreSQL builds them (`jsonb_build_object('defect', defect, 'muted',
//! muted, 'muteUntil', mute_until)`), so the comparison of a result's current state with the
//! `after` of a batch is exact, in SQL and here.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use super::types::{
    AnalysisResult, DecisionSource, DecisionVia, MAX_UNDO_ROWS, TriageDecision, TriageHistory,
    TriageHistoryQuery, TriageState, TriageUndoResult, TriageUndoSkip, TriageUndoSkipCode,
    UndoActor,
};
use crate::ingest::LaunchRef;
use crate::tm::TmError;
use crate::{Defect, Store, no_nul};

/// The SQL of a result's triage state (`r` is the result row).
pub const STATE_SQL: &str =
    "jsonb_build_object('defect', r.defect, 'muted', r.muted, 'muteUntil', r.mute_until)";
/// Launches named in one `triage.decided` event and notified after a batch, at most.
pub const MAX_BATCH_LAUNCHES: usize = 100;
/// The reason of a mute an undo recreates.
pub const RESTORED_MUTE_REASON: &str = "restored by undo";

/// A result and its triage state, as read inside a writer's transaction.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultState {
    pub result_id: Uuid,
    pub launch_id: Uuid,
    pub launch_created_at: DateTime<Utc>,
    pub series_id: Uuid,
    pub fingerprint: Option<String>,
    /// `{defect, muted, muteUntil}` as PostgreSQL builds it.
    pub state: Value,
}

impl ResultState {
    /// The stored defect, when it parses.
    pub fn defect(&self) -> Option<Defect> {
        self.state
            .get("defect")
            .filter(|d| !d.is_null())
            .and_then(|d| serde_json::from_value(d.clone()).ok())
    }

    /// The `source` of the stored defect (`user`, `ai`, `rule`, `auto`).
    pub fn defect_source(&self) -> Option<&str> {
        self.state
            .get("defect")
            .and_then(|d| d.get("source"))
            .and_then(|s| s.as_str())
    }
}

/// A stored state as the API shows it (a defect that does not parse shows as none).
pub fn triage_state(v: &Value) -> TriageState {
    TriageState {
        defect: v
            .get("defect")
            .filter(|d| !d.is_null())
            .and_then(|d| serde_json::from_value(d.clone()).ok()),
        muted: v.get("muted").and_then(|m| m.as_bool()).unwrap_or(false),
        mute_until: v
            .get("muteUntil")
            .and_then(|m| m.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
    }
}

type StateRow = (Uuid, Uuid, DateTime<Utc>, Uuid, Option<String>, Value);

/// The current states of results (by id and launch time), in id order. Read them after the
/// rows are locked.
pub async fn read_states(
    conn: &mut PgConnection,
    keys: &[(Uuid, DateTime<Utc>)],
) -> anyhow::Result<Vec<ResultState>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = keys.iter().map(|k| k.0).collect();
    let created: Vec<DateTime<Utc>> = keys.iter().map(|k| k.1).collect();
    let rows: Vec<StateRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select r.id, r.launch_id, r.launch_created_at, r.series_id, r.fingerprint, {STATE_SQL} \
         from results r join unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
           on r.id = t.i and r.launch_created_at = t.c order by r.id"
    )))
    .bind(&ids)
    .bind(&created)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ResultState {
            result_id: r.0,
            launch_id: r.1,
            launch_created_at: r.2,
            series_id: r.3,
            fingerprint: r.4,
            state: r.5,
        })
        .collect())
}

/// Who and what a batch is.
#[derive(Clone, Debug)]
pub struct Batch<'a> {
    pub project_id: Uuid,
    pub project_key: &'a str,
    pub via: DecisionVia,
    pub source: DecisionSource,
    pub by: &'a str,
    pub suggestion_id: Option<Uuid>,
    pub reverts: Option<Uuid>,
}

/// Journals the results whose state differs between `before` and `after` (matched by result
/// id) as one new batch and emits `triage.decided` (not for rule batches). Returns the batch
/// id, `None` when nothing changed. Runs in the writer's transaction.
pub async fn record(
    conn: &mut PgConnection,
    batch: &Batch<'_>,
    before: &[ResultState],
    after: &[ResultState],
) -> anyhow::Result<Option<Uuid>> {
    let after_of: HashMap<Uuid, &ResultState> = after.iter().map(|a| (a.result_id, a)).collect();
    let changed: Vec<(&ResultState, &ResultState)> = before
        .iter()
        .filter_map(|b| after_of.get(&b.result_id).map(|a| (b, *a)))
        .filter(|(b, a)| b.state != a.state)
        .collect();
    if changed.is_empty() {
        return Ok(None);
    }
    let batch_id = Uuid::now_v7();
    sqlx::query(
        "insert into triage_decisions (project_id, launch_id, launch_created_at, result_id, series_id, \
           fingerprint, batch_id, via, source, by, before, after, suggestion_id, reverts_batch) \
         select $1, l, c, r, s, f, $7, $8, $9, $10, b, a, $13, $14 \
         from unnest($2::uuid[], $3::timestamptz[], $4::uuid[], $5::uuid[], $6::text[], $11::jsonb[], $12::jsonb[]) \
           as t(l, c, r, s, f, b, a)",
    )
    .bind(batch.project_id)
    .bind(changed.iter().map(|c| c.0.launch_id).collect::<Vec<_>>())
    .bind(
        changed
            .iter()
            .map(|c| c.0.launch_created_at)
            .collect::<Vec<_>>(),
    )
    .bind(changed.iter().map(|c| c.0.result_id).collect::<Vec<_>>())
    .bind(changed.iter().map(|c| c.0.series_id).collect::<Vec<_>>())
    .bind(
        changed
            .iter()
            .map(|c| c.1.fingerprint.clone().or_else(|| c.0.fingerprint.clone()))
            .collect::<Vec<_>>(),
    )
    .bind(batch_id)
    .bind(batch.via.as_str())
    .bind(batch.source.as_str())
    .bind(no_nul(batch.by))
    .bind(changed.iter().map(|c| c.0.state.clone()).collect::<Vec<_>>())
    .bind(changed.iter().map(|c| c.1.state.clone()).collect::<Vec<_>>())
    .bind(batch.suggestion_id)
    .bind(batch.reverts)
    .execute(&mut *conn)
    .await?;
    metrics::counter!("fastproof_analysis_decisions_total",
        "via" => batch.via.as_str(), "source" => batch.source.as_str())
    .increment(changed.len() as u64);
    if batch.via != DecisionVia::Rule {
        let launches: Vec<Uuid> = changed
            .iter()
            .map(|c| c.0.launch_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(MAX_BATCH_LAUNCHES)
            .collect();
        let mut payload = serde_json::json!({"batchId": batch_id, "via": batch.via.as_str(),
            "source": batch.source.as_str(), "launchIds": launches, "count": changed.len()});
        if let Some(s) = batch.suggestion_id {
            payload["suggestionId"] = serde_json::json!(s);
        }
        crate::events::emit(conn, "triage.decided", Some(batch.project_key), payload).await?;
    }
    Ok(Some(batch_id))
}

/// A result whose defect a matched rule changed at ingest.
#[derive(Clone, Debug)]
pub struct RuleChangeRow {
    pub result_id: Uuid,
    pub series_id: Uuid,
    pub fingerprint: Option<String>,
    pub rule_id: Uuid,
    pub before_defect: Option<Value>,
    pub before_muted: bool,
    pub before_mute_until: Option<DateTime<Utc>>,
    pub after_defect: Option<Value>,
    pub after_muted: bool,
    pub after_mute_until: Option<DateTime<Utc>>,
}

/// Journals the rule changes of one ingest hook call in one statement (`via = rule`, `source =
/// rule`, `by = rule:<id>`, one batch); no outbox event (the ingest emits its own).
pub async fn record_rule_changes(
    conn: &mut PgConnection,
    launch: &LaunchRef,
    rows: &[RuleChangeRow],
) -> anyhow::Result<Option<Uuid>> {
    if rows.is_empty() {
        return Ok(None);
    }
    let batch_id = Uuid::now_v7();
    sqlx::query(
        "insert into triage_decisions (project_id, launch_id, launch_created_at, result_id, series_id, \
           fingerprint, batch_id, via, source, by, before, after) \
         select $1, $2, $3, r, s, f, $4, 'rule', 'rule', b, \
           jsonb_build_object('defect', bd, 'muted', bm, 'muteUntil', bu), \
           jsonb_build_object('defect', ad, 'muted', am, 'muteUntil', au) \
         from unnest($5::uuid[], $6::uuid[], $7::text[], $8::text[], $9::jsonb[], $10::bool[], \
           $11::timestamptz[], $12::jsonb[], $13::bool[], $14::timestamptz[]) \
           as t(r, s, f, b, bd, bm, bu, ad, am, au)",
    )
    .bind(launch.project_id)
    .bind(launch.id)
    .bind(launch.created_at)
    .bind(batch_id)
    .bind(rows.iter().map(|r| r.result_id).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.series_id).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.fingerprint.clone()).collect::<Vec<_>>())
    .bind(
        rows.iter()
            .map(|r| format!("rule:{}", r.rule_id))
            .collect::<Vec<_>>(),
    )
    .bind(
        rows.iter()
            .map(|r| r.before_defect.clone())
            .collect::<Vec<_>>(),
    )
    .bind(rows.iter().map(|r| r.before_muted).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.before_mute_until).collect::<Vec<_>>())
    .bind(
        rows.iter()
            .map(|r| r.after_defect.clone())
            .collect::<Vec<_>>(),
    )
    .bind(rows.iter().map(|r| r.after_muted).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.after_mute_until).collect::<Vec<_>>())
    .execute(&mut *conn)
    .await?;
    metrics::counter!("fastproof_analysis_decisions_total", "via" => "rule", "source" => "rule")
        .increment(rows.len() as u64);
    Ok(Some(batch_id))
}

/// Whether two defects are the same decision (time and author aside).
pub fn same_decision(a: Option<&Defect>, b: Option<&Defect>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.type_id == b.type_id
                && a.comment == b.comment
                && a.issue_url == b.issue_url
                && a.issue_key == b.issue_key
                && a.source == b.source
                && a.rule_id == b.rule_id
                && a.accepted == b.accepted
        }
        _ => false,
    }
}

type DecisionRow = (
    i64,
    Uuid,
    Uuid,
    Uuid,
    DateTime<Utc>,
    Uuid,
    Option<String>,
    String,
    String,
    String,
    DateTime<Utc>,
    Value,
    Value,
    Option<Uuid>,
    Option<Uuid>,
);

const DECISION_COLUMNS: &str = "id, batch_id, result_id, launch_id, launch_created_at, series_id, fingerprint, \
     via, source, by, at, before, after, suggestion_id, reverts_batch";

fn mute_part(v: &Value) -> (bool, Option<&Value>) {
    (
        v.get("muted").and_then(|m| m.as_bool()).unwrap_or(false),
        v.get("muteUntil").filter(|u| !u.is_null()),
    )
}

/// Whether a journal row changed the mute of its result.
fn changes_mute(r: &DecisionRow) -> bool {
    mute_part(&r.11) != mute_part(&r.12)
}

fn decision(r: DecisionRow, undoable: bool) -> TriageDecision {
    TriageDecision {
        id: r.0,
        batch_id: r.1,
        result_id: r.2,
        launch_id: r.3,
        fingerprint: r.6,
        via: DecisionVia::parse(&r.7).unwrap_or(DecisionVia::Api),
        source: DecisionSource::parse(&r.8).unwrap_or(DecisionSource::User),
        by: r.9,
        at: r.10,
        before: triage_state(&r.11),
        after: triage_state(&r.12),
        suggestion_id: r.13,
        reverts_batch: r.14,
        undoable,
    }
}

fn parse_before(s: Option<&str>) -> AnalysisResult<Option<i64>> {
    s.map(|s| {
        s.parse::<i64>()
            .map_err(|_| TmError::bad_request("before must be the next value of a previous page"))
    })
    .transpose()
}

impl Store {
    /// `GET .../triage/history`: the rows of a result (newest first) or of a batch.
    pub async fn triage_history(
        &self,
        project_id: Uuid,
        q: &TriageHistoryQuery,
    ) -> AnalysisResult<TriageHistory> {
        let (column, id) = match (q.result_id, q.batch_id) {
            (Some(r), None) => ("result_id", r),
            (None, Some(b)) => ("batch_id", b),
            _ => return Err(TmError::bad_request("give resultId or batchId")),
        };
        let before = parse_before(q.before.as_deref())?;
        let limit = q.limit.clamp(1, 500);
        let mut rows: Vec<DecisionRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {DECISION_COLUMNS} from triage_decisions \
             where project_id = $1 and {column} = $2 and ($3::bigint is null or id < $3) \
             order by id desc limit $4"
        )))
        .bind(project_id)
        .bind(id)
        .bind(before)
        .bind(limit + 1)
        .fetch_all(&self.pool)
        .await
        .map_err(anyhow::Error::from)?;
        let next = if rows.len() as i64 > limit {
            rows.truncate(limit as usize);
            rows.last().map(|r| r.0.to_string())
        } else {
            None
        };
        let batches: Vec<Uuid> = rows
            .iter()
            .filter(|r| r.7 != "rule")
            .map(|r| r.1)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let undoable: HashMap<Uuid, bool> = if batches.is_empty() {
            HashMap::new()
        } else {
            sqlx::query_as::<_, (Uuid, bool)>(sqlx::AssertSqlSafe(format!(
                "select b, exists(select 1 from triage_decisions d join results r \
                   on r.id = d.result_id and r.launch_created_at = d.launch_created_at \
                   where d.project_id = $1 and d.batch_id = b and {STATE_SQL} = d.after) \
                 from unnest($2::uuid[]) as b"
            )))
            .bind(project_id)
            .bind(&batches)
            .fetch_all(&self.pool)
            .await
            .map_err(anyhow::Error::from)?
            .into_iter()
            .collect()
        };
        Ok(TriageHistory {
            items: rows
                .into_iter()
                .map(|r| {
                    let u = r.7 != "rule" && undoable.get(&r.1).copied().unwrap_or(false);
                    decision(r, u)
                })
                .collect(),
            next,
        })
    }

    /// `POST .../triage/undo`: writes back the `before` of every row of a batch whose result is
    /// still at its `after`, as a new batch (`via = undo`). The transaction runs again when
    /// PostgreSQL aborts it to break a deadlock.
    pub async fn undo_triage(
        &self,
        project_id: Uuid,
        project_key: &str,
        batch_id: Uuid,
        by: &str,
        actor: &UndoActor,
    ) -> AnalysisResult<TriageUndoResult> {
        let mut attempt = 1;
        loop {
            match self
                .undo_once(project_id, project_key, batch_id, by, actor)
                .await
            {
                Err(TmError::Other(e))
                    if attempt < crate::ingest::TX_ATTEMPTS
                        && crate::ingest::retryable(&e).is_some() =>
                {
                    crate::ingest::retry_pause("triage undo", &e, attempt).await;
                    attempt += 1;
                }
                r => return r,
            }
        }
    }

    async fn undo_once(
        &self,
        project_id: Uuid,
        project_key: &str,
        batch_id: Uuid,
        by: &str,
        actor: &UndoActor,
    ) -> AnalysisResult<TriageUndoResult> {
        let mut tx = self.pool.begin().await.map_err(anyhow::Error::from)?;
        let rows: Vec<DecisionRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {DECISION_COLUMNS} from triage_decisions where project_id = $1 and batch_id = $2 \
             order by id limit $3"
        )))
        .bind(project_id)
        .bind(batch_id)
        .bind(MAX_UNDO_ROWS + 1)
        .fetch_all(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
        let Some(first) = rows.first() else {
            return Err(TmError::NotFound("batch"));
        };
        if first.7 == "rule" {
            return Err(TmError::conflict(
                "batch_not_undoable",
                "decisions of a rule at ingest cannot be undone (the next ingest applies the rule again); edit the rule",
            ));
        }
        if rows.len() as i64 > MAX_UNDO_ROWS {
            return Err(TmError::conflict(
                "batch_too_large",
                format!("a batch of more than {MAX_UNDO_ROWS} results cannot be undone"),
            ));
        }
        if actor.agent && first.9 != actor.name {
            return Err(TmError::Forbidden {
                code: "not_your_batch",
                message: "an agent may undo only its own batches".into(),
            });
        }
        if !actor.can_mute && rows.iter().any(changes_mute) {
            return Err(TmError::Forbidden {
                code: "forbidden",
                message: "undoing a batch that changed mutes needs results:mute".into(),
            });
        }
        let batch_by = first.9.clone();
        let batch_at = first.10;

        // The order every writer of results takes (`triage::lock_launches`): launch rows
        // share-locked, result rows in id order, the launch rows for the counter at the end.
        let launch_ids: Vec<Uuid> = rows
            .iter()
            .map(|r| r.3)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let launches: HashMap<Uuid, bool> = sqlx::query_as::<_, (Uuid, bool)>(
            "select id, merged_into is not null from launches where id = any($1) order by id for key share",
        )
        .bind(&launch_ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?
        .into_iter()
        .collect();
        let mut keys: Vec<(Uuid, DateTime<Utc>)> = rows.iter().map(|r| (r.2, r.4)).collect();
        keys.sort();
        keys.dedup();
        sqlx::query(
            "select 1 from results r join unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
               on r.id = t.i and r.launch_created_at = t.c order by r.id for no key update of r",
        )
        .bind(keys.iter().map(|k| k.0).collect::<Vec<_>>())
        .bind(keys.iter().map(|k| k.1).collect::<Vec<_>>())
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
        let current: HashMap<Uuid, ResultState> = read_states(&mut tx, &keys)
            .await?
            .into_iter()
            .map(|s| (s.result_id, s))
            .collect();

        let mut skipped = Vec::new();
        let mut restore: Vec<(&DecisionRow, &ResultState)> = Vec::new();
        let mut seen = BTreeSet::new();
        for r in &rows {
            if !seen.insert(r.2) {
                continue;
            }
            let code = match current.get(&r.2) {
                None => Some(TriageUndoSkipCode::ResultGone),
                Some(_) if launches.get(&r.3).copied().unwrap_or(true) => {
                    Some(TriageUndoSkipCode::LaunchMerged)
                }
                Some(c) if c.state != r.12 => Some(TriageUndoSkipCode::ChangedSince),
                Some(c) => {
                    restore.push((r, c));
                    None
                }
            };
            if let Some(code) = code {
                skipped.push(TriageUndoSkip {
                    result_id: r.2,
                    code,
                });
            }
        }
        if restore.is_empty() {
            tx.rollback().await.map_err(anyhow::Error::from)?;
            return Ok(TriageUndoResult {
                batch_id: None,
                reverts: batch_id,
                restored: 0,
                skipped,
            });
        }
        let before: Vec<ResultState> = restore.iter().map(|(_, c)| (*c).clone()).collect();
        let defects: Vec<Option<Value>> = restore
            .iter()
            .map(|(r, _)| r.11.get("defect").filter(|d| !d.is_null()).cloned())
            .collect();
        let muted: Vec<bool> = restore.iter().map(|(r, _)| mute_part(&r.11).0).collect();
        let until: Vec<Option<DateTime<Utc>>> = restore
            .iter()
            .map(|(r, _)| triage_state(&r.11).mute_until)
            .collect();
        sqlx::query(
            "update results r set defect = t.d, muted = t.m, mute_until = t.u, updated_at = now() \
             from unnest($1::uuid[], $2::timestamptz[], $3::jsonb[], $4::bool[], $5::timestamptz[]) \
               as t(i, c, d, m, u) \
             where r.id = t.i and r.launch_created_at = t.c",
        )
        .bind(restore.iter().map(|(r, _)| r.2).collect::<Vec<_>>())
        .bind(restore.iter().map(|(r, _)| r.4).collect::<Vec<_>>())
        .bind(&defects)
        .bind(&muted)
        .bind(&until)
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;

        // Series mutes, once per series in series order.
        let mut mute_launches = BTreeSet::new();
        let mut per_series: BTreeMap<Uuid, &DecisionRow> = BTreeMap::new();
        for (r, _) in &restore {
            if changes_mute(r) {
                per_series.entry(r.5).or_insert(r);
                mute_launches.insert(r.3);
            }
        }
        for (series, r) in per_series {
            let (b, a) = (triage_state(&r.11), triage_state(&r.12));
            if !b.muted && a.muted {
                sqlx::query(
                    "delete from series_mutes where project_id = $1 and series_id = $2 \
                     and created_by is not distinct from $3 and created_at = $4",
                )
                .bind(project_id)
                .bind(series)
                .bind(&batch_by)
                .bind(batch_at)
                .execute(&mut *tx)
                .await
                .map_err(anyhow::Error::from)?;
            } else if b.muted && !a.muted {
                sqlx::query(
                    "insert into series_mutes (series_id, project_id, until, reason, created_by) \
                     values ($1, $2, $3, $4, $5) on conflict (series_id) do nothing",
                )
                .bind(series)
                .bind(project_id)
                .bind(b.mute_until)
                .bind(RESTORED_MUTE_REASON)
                .bind(no_nul(by))
                .execute(&mut *tx)
                .await
                .map_err(anyhow::Error::from)?;
            } else if b.muted && a.muted {
                sqlx::query(
                    "update series_mutes set until = $3 where project_id = $1 and series_id = $2 \
                     and created_by is not distinct from $4 and created_at = $5",
                )
                .bind(project_id)
                .bind(series)
                .bind(b.mute_until)
                .bind(&batch_by)
                .bind(batch_at)
                .execute(&mut *tx)
                .await
                .map_err(anyhow::Error::from)?;
            }
        }
        if !mute_launches.is_empty() {
            let l: Vec<Uuid> = mute_launches.into_iter().collect();
            super::triage::recount_muted(&mut tx, &l).await?;
        }
        let restored_keys: Vec<(Uuid, DateTime<Utc>)> =
            restore.iter().map(|(r, _)| (r.2, r.4)).collect();
        let after = read_states(&mut tx, &restored_keys).await?;
        let new_batch = record(
            &mut tx,
            &Batch {
                project_id,
                project_key,
                via: DecisionVia::Undo,
                source: if actor.agent {
                    DecisionSource::Ai
                } else {
                    DecisionSource::User
                },
                by,
                suggestion_id: None,
                reverts: Some(batch_id),
            },
            &before,
            &after,
        )
        .await?;
        let notified: BTreeSet<Uuid> = restore.iter().map(|(r, _)| r.3).collect();
        for l in notified.into_iter().take(MAX_BATCH_LAUNCHES) {
            crate::events::notify_launch(&mut tx, project_key, l).await?;
        }
        tx.commit().await.map_err(anyhow::Error::from)?;
        Ok(TriageUndoResult {
            batch_id: new_batch,
            reverts: batch_id,
            restored: restore.len() as u64,
            skipped,
        })
    }
}
