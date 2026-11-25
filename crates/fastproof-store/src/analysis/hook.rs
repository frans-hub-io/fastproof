//! Analysis of results as they are persisted (same transaction): fingerprints of new failed
//! attempts, transitions and flakiness against earlier runs, rules, default defects, mutes,
//! the fingerprint catalog and the per-series statistics.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fastproof_model::{Status, fingerprint, is_flaky, status_transition};
use sqlx::PgConnection;
use uuid::Uuid;

use super::defects::{self, TO_INVESTIGATE};
use super::rules::{self, Budget, CompiledRule, Subject};
use super::settings::ProjectSettings;
use super::stats::{self, SeriesStatsRow};
use crate::Defect;
use crate::ingest::LaunchRef;

/// A newly written attempt, as the hook needs it.
#[derive(Clone, Copy, Debug)]
pub struct PersistedAttempt<'a> {
    pub id: Uuid,
    pub status: Status,
    pub message: Option<&'a str>,
    pub trace: Option<&'a str>,
}

/// Who sets defects that no person chose.
pub const SYSTEM: &str = "fastproof";

/// A defect type reference (id, group, name).
#[derive(Clone, Debug)]
pub(crate) struct TypeRef {
    pub id: Uuid,
    pub group: String,
    pub name: String,
}

pub(crate) async fn type_refs(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> anyhow::Result<(HashMap<Uuid, TypeRef>, HashMap<String, TypeRef>)> {
    let builtins = defects::builtin_types(conn, project_id).await?;
    let rows: Vec<(Uuid, String, String)> =
        sqlx::query_as("select id, grp, name from defect_types where project_id = $1 limit 1000")
            .bind(project_id)
            .fetch_all(&mut *conn)
            .await?;
    let by_id = rows
        .into_iter()
        .map(|(id, group, name)| (id, TypeRef { id, group, name }))
        .collect();
    let by_group = builtins
        .into_iter()
        .map(|t| {
            (
                t.group.clone(),
                TypeRef {
                    id: t.id,
                    group: t.group,
                    name: t.name,
                },
            )
        })
        .collect();
    Ok((by_id, by_group))
}

/// The defect a rule gives a result.
pub(crate) fn rule_defect(
    rule: &CompiledRule,
    status: Status,
    existing: Option<&Defect>,
    by_id: &HashMap<Uuid, TypeRef>,
    by_group: &HashMap<String, TypeRef>,
    now: DateTime<Utc>,
) -> Option<Defect> {
    let a = &rule.rule.action;
    let t = a
        .defect_type_id
        .and_then(|i| by_id.get(&i))
        .or_else(|| {
            existing
                .filter(|d| d.source != "rule" || d.rule_id.is_some())
                .and_then(|d| by_id.get(&d.type_id))
        })
        .or_else(|| {
            by_group.get(if status.is_failure() {
                TO_INVESTIGATE
            } else {
                "noDefect"
            })
        })?;
    Some(Defect {
        type_id: t.id,
        group: t.group.clone(),
        name: t.name.clone(),
        comment: a.comment.clone(),
        issue_url: a.issue.as_ref().map(|i| i.url.clone()),
        issue_key: a.issue.as_ref().and_then(|i| i.key.clone()),
        source: "rule".into(),
        rule_id: Some(rule.rule.id),
        at: now,
        by: format!("rule:{}", rule.rule.name),
        accepted: a.accept.unwrap_or(false),
    })
}

fn default_defect(by_group: &HashMap<String, TypeRef>, now: DateTime<Utc>) -> Option<Defect> {
    let t = by_group.get(TO_INVESTIGATE)?;
    Some(Defect {
        type_id: t.id,
        group: t.group.clone(),
        name: t.name.clone(),
        comment: None,
        issue_url: None,
        issue_key: None,
        source: "rule".into(),
        rule_id: None,
        at: now,
        by: SYSTEM.into(),
        accepted: false,
    })
}

/// A defect a person (or an AI agent) chose; rules and defaults never replace it.
pub(crate) fn is_protected(d: Option<&Defect>) -> bool {
    d.is_some_and(|d| d.source == "user" || d.source == "ai")
}

fn is_default(d: &Defect) -> bool {
    d.source == "rule" && d.rule_id.is_none()
}

/// History rows: series, status, launch creation time.
type HistoryRow = (Uuid, String, DateTime<Utc>);

/// Past statuses of series on a branch (newest first), from raw results. Series whose raw
/// history is gone are returned apart: they fall back to the series statistics
/// ([`with_stats_fallback`]).
pub(crate) async fn past_statuses(
    conn: &mut PgConnection,
    project_id: Uuid,
    series: &[Uuid],
    before: DateTime<Utc>,
    exclude_launch: Uuid,
    branch: Option<&str>,
    depth: i64,
) -> anyhow::Result<(HashMap<Uuid, Vec<Status>>, Vec<Uuid>)> {
    // The latest results of each series first (an ordered index scan per series), then which
    // of their few launches count (branch, not debug). Joining every result to its launch
    // inside the scan let PostgreSQL probe every launch of the branch for every series when
    // the statistics were not fresh (a busy writer took 200 ms per batch). A series whose
    // fetched rows were cut by the limit and partly dropped by the launch filter may have
    // more history further back: those few series are read again exactly.
    let rows: Vec<(Uuid, String, DateTime<Utc>, Uuid)> = sqlx::query_as(
        "select s.sid, r.status, r.launch_created_at, r.launch_id from unnest($1::uuid[]) as s(sid) \
         cross join lateral (select r.status, r.launch_created_at, r.launch_id from results r \
           where r.series_id = s.sid and r.project_id = $2 and r.launch_created_at < $3 \
             and r.launch_id <> $4 \
           order by r.launch_created_at desc limit $5) r",
    )
    .bind(series)
    .bind(project_id)
    .bind(before)
    .bind(exclude_launch)
    .bind(depth)
    .fetch_all(&mut *conn)
    .await?;
    let mut launches: Vec<Uuid> = rows.iter().map(|r| r.3).collect();
    launches.sort_unstable();
    launches.dedup();
    let counted: HashSet<Uuid> = if launches.is_empty() {
        HashSet::new()
    } else {
        sqlx::query_scalar::<_, Uuid>(
            "select id from launches where id = any($1) and not debug and branch is not distinct from $2",
        )
        .bind(&launches)
        .bind(branch)
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .collect()
    };
    let mut fetched: HashMap<Uuid, i64> = HashMap::new();
    let mut by: HashMap<Uuid, Vec<(DateTime<Utc>, Status)>> = HashMap::new();
    for (s, st, at, launch) in rows {
        *fetched.entry(s).or_default() += 1;
        if counted.contains(&launch) {
            by.entry(s)
                .or_default()
                .push((at, Status::parse_lenient(&st)));
        }
    }
    let cut: Vec<Uuid> = fetched
        .iter()
        .filter(|(s, n)| **n >= depth && by.get(*s).map_or(0, |v| v.len() as i64) < depth)
        .map(|(s, _)| *s)
        .collect();
    if !cut.is_empty() {
        let rows: Vec<HistoryRow> = sqlx::query_as(
            "select s.sid, h.status, h.launch_created_at from unnest($1::uuid[]) as s(sid) \
             cross join lateral (select r.status, r.launch_created_at from results r \
               join launches l on l.id = r.launch_id \
               where r.series_id = s.sid and r.project_id = $2 and r.launch_created_at < $3 \
                 and r.launch_id <> $4 and not l.debug and l.branch is not distinct from $5 \
               order by r.launch_created_at desc limit $6) h",
        )
        .bind(&cut)
        .bind(project_id)
        .bind(before)
        .bind(exclude_launch)
        .bind(branch)
        .bind(depth)
        .fetch_all(&mut *conn)
        .await?;
        for s in &cut {
            by.remove(s);
        }
        for (s, st, at) in rows {
            by.entry(s)
                .or_default()
                .push((at, Status::parse_lenient(&st)));
        }
    }
    let missing: Vec<Uuid> = series
        .iter()
        .filter(|s| !by.contains_key(s))
        .copied()
        .collect();
    // Series with raw results elsewhere (other branches) have no history on this branch; only
    // series whose raw results are all gone fall back to the statistics.
    let elsewhere: std::collections::HashSet<Uuid> = if missing.is_empty() {
        Default::default()
    } else {
        sqlx::query_scalar::<_, Uuid>(
            "select s.sid from unnest($1::uuid[]) as s(sid) where exists (select 1 from results r \
             where r.series_id = s.sid and r.project_id = $2 and r.launch_id <> $3)",
        )
        .bind(&missing)
        .bind(project_id)
        .bind(exclude_launch)
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .collect()
    };
    let mut out = HashMap::new();
    let mut gone = Vec::new();
    for s in series {
        let h = match by.remove(s) {
            Some(mut v) => {
                v.sort_by_key(|a| std::cmp::Reverse(a.0));
                v.into_iter().map(|x| x.1).collect()
            }
            None if elsewhere.contains(s) => Vec::new(),
            None => {
                gone.push(*s);
                continue;
            }
        };
        out.insert(*s, h);
    }
    Ok((out, gone))
}

/// Completes [`past_statuses`] with the statistics of the series whose raw history is gone.
pub(crate) fn with_stats_fallback(
    (mut out, gone): (HashMap<Uuid, Vec<Status>>, Vec<Uuid>),
    stats: &HashMap<Uuid, SeriesStatsRow>,
    exclude_launch: Uuid,
    depth: i64,
) -> HashMap<Uuid, Vec<Status>> {
    for s in gone {
        let h = stats
            .get(&s)
            .map(|st| {
                let mut v = stats::decode(&st.last_statuses);
                if st.last_launch_id == Some(exclude_launch) && !v.is_empty() {
                    v.pop();
                }
                v.reverse();
                v.truncate(depth as usize);
                v
            })
            .unwrap_or_default();
        out.insert(s, h);
    }
    out
}

/// A result row the hook works on.
type ResultRow = (
    Uuid,
    Uuid,
    Uuid,
    String,
    Option<String>,
    Option<serde_json::Value>,
    Option<Uuid>,
    Option<String>,
    Option<String>,
    bool,
    bool,
    Option<DateTime<Utc>>,
);

/// Labels of a manual result: manual series are left out of flaky detection.
pub const MANUAL_LABEL: &str = r#"[{"name": "manual", "value": "true"}]"#;

/// Runs the analysis of newly persisted attempts and the results of their series.
pub async fn after_persist(
    conn: &mut PgConnection,
    launch: &LaunchRef,
    attempts: &[PersistedAttempt<'_>],
    series: &[Uuid],
) -> anyhow::Result<()> {
    let hits = after_persist_deferred(conn, launch, attempts, series).await?;
    rules::add_hits(conn, &hits).await
}

/// As [`after_persist`], but returns the rule hits instead of adding them, so a caller that
/// locks the launch row late (manual runs) can add them after it: rule rows are shared by
/// every launch of the project, and ingest locks the launch row before them.
pub async fn after_persist_deferred(
    conn: &mut PgConnection,
    launch: &LaunchRef,
    attempts: &[PersistedAttempt<'_>],
    series: &[Uuid],
) -> anyhow::Result<HashMap<Uuid, i64>> {
    if series.is_empty() {
        return Ok(HashMap::new());
    }
    let now = Utc::now();

    // Fingerprints of the new failed attempts.
    let mut fp_ids = Vec::new();
    let mut fp_hashes = Vec::new();
    let mut fp_messages: HashMap<String, String> = HashMap::new();
    for a in attempts.iter().filter(|a| a.status.is_failure()) {
        if let Some(fp) = fingerprint(a.message, a.trace) {
            fp_ids.push(a.id);
            fp_hashes.push(fp.hash.clone());
            fp_messages.entry(fp.hash).or_insert(fp.normalized_message);
        }
    }
    if !fp_ids.is_empty() {
        sqlx::query(
            "update attempts a set fingerprint = t.f from unnest($1::uuid[], $2::text[]) as t(i, f) \
             where a.id = t.i and a.launch_created_at = $3",
        )
        .bind(&fp_ids)
        .bind(&fp_hashes)
        .bind(launch.created_at)
        .execute(&mut *conn)
        .await?;
    }
    let new_attempts: std::collections::HashSet<Uuid> = attempts.iter().map(|a| a.id).collect();

    let (branch, env_key, debug, settings): (
        Option<String>,
        Option<String>,
        bool,
        serde_json::Value,
    ) = sqlx::query_as(
        "select l.branch, l.env_key, l.debug, p.settings from launches l join projects p on p.id = l.project_id \
         where l.id = $1",
    )
    .bind(launch.id)
    .fetch_one(&mut *conn)
    .await?;
    let settings = ProjectSettings::from_value(settings);
    let rules: Vec<Arc<CompiledRule>> =
        rules::enabled_rules(conn, launch.project_id, rules::RulePhase::Ingest).await?;
    let need_trace = rules.iter().any(|r| r.needs_trace());

    let rows: Vec<ResultRow> = sqlx::query_as(
        "select r.id, r.series_id, r.test_case_id, r.status, r.message, r.defect, la.id, la.fingerprint, la.trace, \
           r.labels @> $5::jsonb as manual, r.muted, r.mute_until \
         from results r left join lateral (select a.id, a.fingerprint, \
             case when $4 then left(a.trace, 16384) end as trace from attempts a \
           where a.result_id = r.id and a.launch_created_at = r.launch_created_at \
           order by (a.started_at is not null) desc, a.started_at desc nulls last, a.seq desc limit 1) la on true \
         where r.launch_id = $1 and r.launch_created_at = $2 and r.series_id = any($3)",
    )
    .bind(launch.id)
    .bind(launch.created_at)
    .bind(series)
    .bind(need_trace)
    .bind(MANUAL_LABEL)
    .fetch_all(&mut *conn)
    .await?;
    if rows.is_empty() {
        return Ok(HashMap::new());
    }
    let mut series_sorted: Vec<Uuid> = rows.iter().map(|r| r.1).collect();
    series_sorted.sort();
    series_sorted.dedup();

    // The statistics rows of the series are shared by every launch running the same tests,
    // and their locks are held until commit: they are taken last, right before the rows are
    // written, so such launches wait for each other only that long. The history is read
    // before, next to a snapshot of those rows; series whose row another launch changed in
    // between get their history read again under the lock (`recheck` below), so the verdicts
    // match the order in which the launches commit.
    let depth = settings.flaky.depth();
    let seen = stats::read_rows(conn, &series_sorted).await?;
    let history = past_statuses(
        conn,
        launch.project_id,
        &series_sorted,
        launch.created_at,
        launch.id,
        branch.as_deref(),
        depth,
    )
    .await?;
    let history = with_stats_fallback(history, &seen, launch.id, depth);
    let (by_id, by_group) = type_refs(conn, launch.project_id).await?;
    let mut mutes: HashMap<Uuid, Option<DateTime<Utc>>> = sqlx::query_as::<_, (Uuid, Option<DateTime<Utc>>)>(
        "select series_id, until from series_mutes where series_id = any($1) and (until is null or until > now())",
    )
    .bind(&series_sorted)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();

    let opts = settings.flaky.options();
    let mut budget = Budget::ingest();
    let mut hits: HashMap<Uuid, i64> = HashMap::new();
    let mut new_mutes: BTreeMap<Uuid, (Option<DateTime<Utc>>, String)> = BTreeMap::new();
    let mut upd = (
        Vec::<Uuid>::new(),
        Vec::<Option<String>>::new(),
        Vec::<Option<String>>::new(),
        Vec::<bool>::new(),
        Vec::<Option<serde_json::Value>>::new(),
        Vec::<bool>::new(),
        Vec::<Option<DateTime<Utc>>>::new(),
    );
    let mut catalog: BTreeMap<String, (String, i64)> = BTreeMap::new();
    let mut stat_updates: Vec<(Uuid, Status, bool)> = Vec::new();
    let mut journal: Vec<super::decisions::RuleChangeRow> = Vec::new();
    for (
        id,
        series_id,
        test_case_id,
        status_s,
        message,
        defect,
        latest,
        fp,
        trace,
        manual,
        was_muted,
        was_until,
    ) in &rows
    {
        let mut by_rule: Option<Uuid> = None;
        let status = Status::parse_lenient(status_s);
        let (transition, flaky) = verdict(status, *manual, history.get(series_id), &opts);
        let existing: Option<Defect> = defect.clone().and_then(|d| serde_json::from_value(d).ok());
        let mut new_defect = existing.clone();
        if !is_protected(existing.as_ref()) {
            let subject = Subject {
                status: status_s,
                message: message.as_deref(),
                trace: trace.as_deref(),
                test_case_id: *test_case_id,
                fingerprint: fp.as_deref(),
                environment: env_key.as_deref(),
            };
            let matched = if budget.exhausted {
                Ok(None)
            } else {
                rules::first_match(&rules, &subject, &mut budget)
            };
            if matched.is_err() {
                metrics::counter!("fastproof_rule_budget_exhausted_total").increment(1);
                tracing::warn!(launch = %launch.id, "rule evaluation budget of an ingest chunk exhausted; remaining results get default defects");
            }
            match matched.ok().flatten() {
                Some(rule) => {
                    new_defect =
                        rule_defect(rule, status, existing.as_ref(), &by_id, &by_group, now);
                    by_rule = Some(rule.rule.id);
                    *hits.entry(rule.rule.id).or_default() += 1;
                    if let Some(m) = &rule.rule.action.mute {
                        let until = m.days.map(|d| now + chrono::Duration::days(d as i64));
                        new_mutes.insert(*series_id, (until, format!("rule {}", rule.rule.name)));
                        let merged = match mutes.get(series_id) {
                            Some(None) => None,
                            Some(Some(u)) => until.map(|x| x.max(*u)),
                            None => until,
                        };
                        mutes.insert(*series_id, merged);
                    }
                }
                None => {
                    let keep_rule = existing
                        .as_ref()
                        .is_some_and(|d| !is_default(d) && budget.exhausted);
                    if !keep_rule {
                        new_defect = if status.is_failure() {
                            match existing.as_ref() {
                                Some(d) if is_default(d) => existing.clone(),
                                _ => default_defect(&by_group, now),
                            }
                        } else {
                            None
                        };
                    }
                }
            }
        }
        let mute = mutes.get(series_id).copied();
        let changed_by_rule = by_rule.filter(|_| {
            !super::decisions::same_decision(existing.as_ref(), new_defect.as_ref())
                || *was_muted != mute.is_some()
                || *was_until != mute.flatten()
        });
        let written = new_defect.map(serde_json::to_value).transpose()?;
        if let Some(rule_id) = changed_by_rule {
            journal.push(super::decisions::RuleChangeRow {
                result_id: *id,
                series_id: *series_id,
                fingerprint: fp.clone(),
                rule_id,
                before_defect: defect.clone(),
                before_muted: *was_muted,
                before_mute_until: *was_until,
                after_defect: written.clone(),
                after_muted: mute.is_some(),
                after_mute_until: mute.flatten(),
            });
        }
        upd.0.push(*id);
        upd.1.push(fp.clone());
        upd.2.push(transition);
        upd.3.push(flaky);
        upd.4.push(written);
        upd.5.push(mute.is_some());
        upd.6.push(mute.flatten());
        if status.is_failure()
            && let (Some(f), Some(l)) = (fp, latest)
            && new_attempts.contains(l)
        {
            let e = catalog
                .entry(f.clone())
                .or_insert_with(|| (fp_messages.get(f).cloned().unwrap_or_default(), 0));
            e.1 += 1;
        }
        if !debug {
            stat_updates.push((*series_id, status, flaky));
        }
    }

    sqlx::query(
        "update results r set fingerprint = t.fp, transition = t.tr, flaky = t.fl, defect = t.de, \
           muted = t.mu, mute_until = t.mt, updated_at = now() \
         from unnest($1::uuid[], $2::text[], $3::text[], $4::bool[], $5::jsonb[], $6::bool[], $7::timestamptz[]) \
           as t(i, fp, tr, fl, de, mu, mt) \
         where r.id = t.i and r.launch_created_at = $8",
    )
    .bind(&upd.0)
    .bind(&upd.1)
    .bind(&upd.2)
    .bind(&upd.3)
    .bind(&upd.4)
    .bind(&upd.5)
    .bind(&upd.6)
    .bind(launch.created_at)
    .execute(&mut *conn)
    .await?;

    if !new_mutes.is_empty() {
        upsert_mutes(conn, launch.project_id, &new_mutes, SYSTEM, false).await?;
    }
    super::decisions::record_rule_changes(conn, launch, &journal).await?;
    // Series whose statistics another launch changed since the snapshot are judged again
    // before the lock, from a fresh look at their rows: under the lock, only the few that
    // changed in the short time since need it.
    let fresh = stats::read_rows(conn, &series_sorted).await?;
    let moved = changed_since(&series_sorted, &seen, &fresh);
    if !moved.is_empty() {
        recheck(
            conn,
            launch,
            branch.as_deref(),
            depth,
            &opts,
            &rows,
            &moved,
            &fresh,
            &mut stat_updates,
            false,
        )
        .await?;
    }
    let create = (!debug).then_some(launch.project_id);
    let waited = std::time::Instant::now();
    let stats_rows = stats::lock_rows(conn, &series_sorted, create).await?;
    metrics::histogram!("fastproof_analysis_stats_lock_wait_seconds")
        .record(waited.elapsed().as_secs_f64());
    let locked = std::time::Instant::now();
    let changed = changed_since(&series_sorted, &fresh, &stats_rows);
    if !changed.is_empty() {
        recheck(
            conn,
            launch,
            branch.as_deref(),
            depth,
            &opts,
            &rows,
            &changed,
            &stats_rows,
            &mut stat_updates,
            true,
        )
        .await?;
    }
    if !catalog.is_empty() {
        let fps: Vec<String> = catalog.keys().cloned().collect();
        let msgs: Vec<String> = catalog.values().map(|v| v.0.clone()).collect();
        let counts: Vec<i64> = catalog.values().map(|v| v.1).collect();
        record_fingerprints(
            conn,
            launch.project_id,
            launch.id,
            launch.created_at,
            &fps,
            &msgs,
            &counts,
        )
        .await?;
    }
    if !stat_updates.is_empty() {
        stats::record(conn, launch, &stat_updates, &stats_rows, &settings, now).await?;
    }
    metrics::histogram!("fastproof_analysis_stats_locked_seconds")
        .record(locked.elapsed().as_secs_f64());
    Ok(hits)
}

/// Series whose statistics row differs between two reads.
fn changed_since(
    series: &[Uuid],
    a: &HashMap<Uuid, SeriesStatsRow>,
    b: &HashMap<Uuid, SeriesStatsRow>,
) -> Vec<Uuid> {
    series
        .iter()
        .filter(|id| {
            a.get(*id).cloned().unwrap_or_default() != b.get(*id).cloned().unwrap_or_default()
        })
        .copied()
        .collect()
}

/// Transition and cross-run flakiness of a result from the history of its series.
fn verdict(
    status: Status,
    manual: bool,
    past: Option<&Vec<Status>>,
    opts: &fastproof_model::FlakyOptions,
) -> (Option<String>, bool) {
    let past: &[Status] = past.map_or(&[], |v| v.as_slice());
    let transition = status_transition(status, past).map(|t| {
        serde_json::to_value(t)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    });
    let hist_opt: Vec<Option<Status>> = past.iter().copied().map(Some).collect();
    let flaky = !manual && is_flaky(status, &hist_opt, opts);
    (transition, flaky)
}

/// Series whose statistics another launch changed while this one was analysed: their
/// history is read again, now that their rows are locked (that launch has committed), and
/// the transition and flakiness of this launch's results and statistics follow it.
#[allow(clippy::too_many_arguments)]
async fn recheck(
    conn: &mut PgConnection,
    launch: &LaunchRef,
    branch: Option<&str>,
    depth: i64,
    opts: &fastproof_model::FlakyOptions,
    rows: &[ResultRow],
    changed: &[Uuid],
    stats_rows: &HashMap<Uuid, SeriesStatsRow>,
    stat_updates: &mut [(Uuid, Status, bool)],
    locked: bool,
) -> anyhow::Result<()> {
    let phase = if locked { "locked" } else { "before_lock" };
    metrics::counter!("fastproof_analysis_rechecked_series_total", "phase" => phase)
        .increment(changed.len() as u64);
    let history = past_statuses(
        conn,
        launch.project_id,
        changed,
        launch.created_at,
        launch.id,
        branch,
        depth,
    )
    .await?;
    let history = with_stats_fallback(history, stats_rows, launch.id, depth);
    let changed: std::collections::HashSet<Uuid> = changed.iter().copied().collect();
    let mut ids = Vec::new();
    let mut transitions = Vec::new();
    let mut flakies = Vec::new();
    let mut flaky_of: HashMap<Uuid, bool> = HashMap::new();
    for (id, series_id, _, status_s, _, _, _, _, _, manual, _, _) in rows {
        if !changed.contains(series_id) {
            continue;
        }
        let status = Status::parse_lenient(status_s);
        let (transition, flaky) = verdict(status, *manual, history.get(series_id), opts);
        ids.push(*id);
        transitions.push(transition);
        flakies.push(flaky);
        flaky_of.insert(*series_id, flaky);
    }
    sqlx::query(
        "update results r set transition = t.tr, flaky = t.fl \
         from unnest($1::uuid[], $2::text[], $3::bool[]) as t(i, tr, fl) \
         where r.id = t.i and r.launch_created_at = $4",
    )
    .bind(&ids)
    .bind(&transitions)
    .bind(&flakies)
    .bind(launch.created_at)
    .execute(&mut *conn)
    .await?;
    for (series, _, flaky) in stat_updates.iter_mut() {
        if let Some(f) = flaky_of.get(series) {
            *flaky = *f;
        }
    }
    Ok(())
}

/// Mutes series for a rule applied retroactively by `by` (a journal batch an undo can
/// revert): a series without a mute gets one created by `by` in this transaction, which the
/// undo of the batch finds by its author and time and removes; a series muted already keeps
/// its mute as it is (author, reason and end), so undoing the batch never removes or changes
/// a mute it did not create. Rows are written in series order.
pub(crate) async fn add_retro_mutes(
    conn: &mut PgConnection,
    project_id: Uuid,
    mutes: &BTreeMap<Uuid, (Option<DateTime<Utc>>, String)>,
    by: &str,
) -> anyhow::Result<()> {
    let ids: Vec<Uuid> = mutes.keys().copied().collect();
    let until: Vec<Option<DateTime<Utc>>> = mutes.values().map(|v| v.0).collect();
    let reasons: Vec<String> = mutes.values().map(|v| crate::no_nul(&v.1)).collect();
    sqlx::query(
        "insert into series_mutes (series_id, project_id, until, reason, created_by) \
         select s, $1, u, r, $5 from unnest($2::uuid[], $3::timestamptz[], $4::text[]) as t(s, u, r) \
         on conflict (series_id) do nothing",
    )
    .bind(project_id)
    .bind(&ids)
    .bind(&until)
    .bind(&reasons)
    .bind(crate::no_nul(by))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Adds or extends series mutes (`None` until = no end). Rows are written in series order.
pub(crate) async fn upsert_mutes(
    conn: &mut PgConnection,
    project_id: Uuid,
    mutes: &BTreeMap<Uuid, (Option<DateTime<Utc>>, String)>,
    by: &str,
    replace: bool,
) -> anyhow::Result<()> {
    let ids: Vec<Uuid> = mutes.keys().copied().collect();
    let until: Vec<Option<DateTime<Utc>>> = mutes.values().map(|v| v.0).collect();
    let reasons: Vec<String> = mutes.values().map(|v| crate::no_nul(&v.1)).collect();
    sqlx::query(
        "insert into series_mutes (series_id, project_id, until, reason, created_by) \
         select s, $1, u, r, $5 from unnest($2::uuid[], $3::timestamptz[], $4::text[]) as t(s, u, r) \
         on conflict (series_id) do update set \
           until = case when $6 then excluded.until \
             when series_mutes.until is null or excluded.until is null then null \
             else greatest(series_mutes.until, excluded.until) end, \
           reason = excluded.reason, created_by = excluded.created_by, created_at = now()",
    )
    .bind(project_id)
    .bind(&ids)
    .bind(&until)
    .bind(&reasons)
    .bind(crate::no_nul(by))
    .bind(replace)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Adds occurrences to the fingerprint catalog (sorted keys, one row each).
pub(crate) async fn record_fingerprints(
    conn: &mut PgConnection,
    project_id: Uuid,
    launch_id: Uuid,
    seen_at: DateTime<Utc>,
    fps: &[String],
    messages: &[String],
    counts: &[i64],
) -> anyhow::Result<()> {
    let messages: Vec<String> = messages.iter().map(|m| crate::no_nul(m)).collect();
    sqlx::query(
        "insert into failure_fingerprints (project_id, fingerprint, message, first_seen_at, first_launch_id, \
           last_seen_at, last_launch_id, occurrences) \
         select $1, f, m, $2, $3, $2, $3, n from unnest($4::text[], $5::text[], $6::bigint[]) as t(f, m, n) \
         on conflict (project_id, fingerprint) do update set \
           first_launch_id = case when excluded.first_seen_at < failure_fingerprints.first_seen_at \
             then excluded.first_launch_id else failure_fingerprints.first_launch_id end, \
           first_seen_at = least(failure_fingerprints.first_seen_at, excluded.first_seen_at), \
           last_launch_id = case when excluded.last_seen_at >= failure_fingerprints.last_seen_at \
             then excluded.last_launch_id else failure_fingerprints.last_launch_id end, \
           last_seen_at = greatest(failure_fingerprints.last_seen_at, excluded.last_seen_at), \
           message = case when failure_fingerprints.message = '' then excluded.message else failure_fingerprints.message end, \
           occurrences = failure_fingerprints.occurrences + excluded.occurrences",
    )
    .bind(project_id)
    .bind(seen_at)
    .bind(launch_id)
    .bind(fps)
    .bind(&messages)
    .bind(counts)
    .execute(&mut *conn)
    .await?;
    Ok(())
}
