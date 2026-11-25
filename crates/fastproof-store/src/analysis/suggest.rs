//! Triage suggestions (contract §7.2): the candidates of each cluster of a launch (launch ×
//! canonical fingerprint) from the memory, rules and signals, scored by
//! [`fastproof_analysis::score`] (the same code the backtest runs), persisted per launch with
//! the previous ones superseded; the launch view, accept and reject, and auto-apply.
//!
//! Retrieval only fetches: memory rows of the same fingerprint, then rows with a similar
//! message (`pg_trgm` when installed, the most recent rows compared in Rust otherwise). The
//! score is always computed in Rust, so both paths give the same answer.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use fastproof_analysis::backtest::{message_keys, similar_keys};
use fastproof_analysis::score::{RankOptions, SignalFact, rank, rule_candidate, score};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::decisions::{Batch, read_states, record};
use super::hook::type_refs;
use super::memory::{MEMORY_COLUMNS, MemoryDb, frames_of_results, memory_row, memory_rows};
use super::rules::RulePhase;
use super::triage::{TriageRequest, recount_muted};
use super::types::{
    AcceptActor, AnalysisResult, AttemptSignalBrief, Candidate, ClusterDecided, ClusterFacts,
    ClusterSuggestions, CommitRange, DecisionCtx, DecisionSample, DecisionSource, DecisionVia,
    ErrorState, LaunchCtx, LaunchSuggestions, LaunchSuggestionsState, MAX_CANDIDATES, MemoryRow,
    NoSuggestion, RULE_OFFER_CONFIRMATIONS, SignalConfidence, StepOutcome, Suggestion,
    SuggestionAccept, SuggestionAccepted, SuggestionApplyTo, SuggestionBand, SuggestionReject,
    SuggestionSignal, SuggestionState, WhyItem, WhySignal,
};
use crate::tm::TmError;
use crate::{Defect, Launch, Store, json_no_nul, no_nul};

/// Clusters listed by the launch view, at most.
pub const MAX_VIEW_CLUSTERS: i64 = 2000;
/// Memory rows fetched per cluster by message similarity with `pg_trgm`.
pub const TRIGRAM_ROWS: i64 = 20;
/// Disabled rules dry-run per launch, at most (the newest).
pub const MAX_RULE_CANDIDATES: usize = 20;
/// Results of one cluster read for signals and listed accepts, at most.
pub const CLUSTER_RESULTS: usize = 1000;
/// Bytes of a reject reason, at most.
pub const MAX_REJECT_REASON: usize = 500;
/// Who auto-apply decides as.
pub const AUTO_PRINCIPAL: &str = "fastproof-analysis";

/// A cluster of a launch: failed and broken results under one canonical fingerprint.
#[derive(Clone, Debug)]
pub struct LaunchCluster {
    pub fingerprint: String,
    /// Raw fingerprints of its results (the canonical one and aliases).
    pub fingerprints: Vec<String>,
    pub count: i64,
    pub sample_result_id: Uuid,
    pub result_ids: Vec<Uuid>,
    pub test_case_ids: Vec<Uuid>,
    pub defect: Option<Defect>,
    pub decided: Option<ClusterDecided>,
}

type ClusterDbRow = (
    String,
    i64,
    Uuid,
    Vec<String>,
    Vec<Uuid>,
    Option<Value>,
    bool,
    bool,
    Vec<Uuid>,
);

/// The clusters of a launch, largest first (at most `limit`).
pub async fn launch_clusters(
    conn: &mut PgConnection,
    launch: &Launch,
    limit: i64,
) -> anyhow::Result<Vec<LaunchCluster>> {
    let rows: Vec<ClusterDbRow> = sqlx::query_as(
        "with r as (select r.id, r.test_case_id, r.fingerprint, r.full_name, r.defect, \
             coalesce(f.alias_of, r.fingerprint) as c \
           from results r left join failure_fingerprints f \
             on f.project_id = r.project_id and f.fingerprint = r.fingerprint \
           where r.launch_id = $1 and r.launch_created_at = $2 and r.status in ('failed', 'broken') \
             and r.fingerprint is not null) \
         select c, count(*)::bigint, (array_agg(id order by full_name, id))[1], \
           array_agg(distinct fingerprint), (array_agg(distinct test_case_id))[1:20], \
           (array_agg(defect order by (defect->>'source' in ('user', 'ai')) desc, \
             (defect->>'ruleId') is not null desc, full_name, id) filter (where defect is not null))[1], \
           coalesce(bool_or(defect->>'source' in ('user', 'ai')), false), \
           coalesce(bool_or(defect->>'source' = 'rule' and defect ? 'ruleId'), false), \
           (array_agg(id order by id))[1:1000] \
         from r group by c order by 2 desc, 1 limit $3",
    )
    .bind(launch.id)
    .bind(launch.created_at)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| LaunchCluster {
            fingerprint: r.0,
            count: r.1,
            sample_result_id: r.2,
            fingerprints: r.3,
            test_case_ids: r.4,
            defect: r.5.and_then(|d| serde_json::from_value(d).ok()),
            decided: if r.6 {
                Some(ClusterDecided::Person)
            } else if r.7 {
                Some(ClusterDecided::Rule)
            } else {
                None
            },
            result_ids: r.8,
        })
        .collect())
}

/// Fingerprints aliased to the canonical ones (one level), by canonical fingerprint.
async fn aliases_of(
    conn: &mut PgConnection,
    project_id: Uuid,
    canonical: &[String],
) -> anyhow::Result<HashMap<String, Vec<String>>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select alias_of, fingerprint from failure_fingerprints \
         where project_id = $1 and alias_of = any($2) order by alias_of, fingerprint",
    )
    .bind(project_id)
    .bind(canonical)
    .fetch_all(&mut *conn)
    .await?;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for (c, f) in rows {
        out.entry(c).or_default().push(f);
    }
    Ok(out)
}

type CatalogRow = (
    String,
    String,
    Option<Uuid>,
    String,
    Option<String>,
    Option<String>,
);

/// Catalog facts of fingerprints: normalized message, first launch, state and commits.
async fn catalog(
    conn: &mut PgConnection,
    project_id: Uuid,
    fps: &[String],
) -> anyhow::Result<HashMap<String, CatalogRow>> {
    let rows: Vec<CatalogRow> = sqlx::query_as(
        "select fingerprint, message, first_launch_id, state, first_commit, last_good_commit \
         from failure_fingerprints where project_id = $1 and fingerprint = any($2)",
    )
    .bind(project_id)
    .bind(fps)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(|r| (r.0.clone(), r)).collect())
}

fn signal_facts(signals: &[super::types::AttemptSignal]) -> Vec<SignalFact> {
    signals
        .iter()
        .map(|s| SignalFact {
            source: s.source,
            kind: s.kind.clone(),
            exact: s.confidence == SignalConfidence::Exact,
        })
        .collect()
}

/// Memory rows similar by message to each of `messages` (`pg_trgm`, at most
/// [`TRIGRAM_ROWS`] each), by message index.
async fn trigram_rows(
    conn: &mut PgConnection,
    project_id: Uuid,
    messages: &[String],
) -> anyhow::Result<HashMap<usize, Vec<MemoryRow>>> {
    if messages.is_empty() {
        return Ok(HashMap::new());
    }
    sqlx::query("set local pg_trgm.similarity_threshold = 0.4")
        .execute(&mut *conn)
        .await?;
    let cols: String = MEMORY_COLUMNS
        .split(',')
        .map(|c| format!("m.{}", c.trim()))
        .collect::<Vec<_>>()
        .join(", ");
    let rows: Vec<TrigramRow> = sqlx::query_as::<_, TrigramRow>(sqlx::AssertSqlSafe(
        format!(
            "select u.i, {cols} from unnest($2::text[]) with ordinality as u(msg, i) \
             cross join lateral (select {MEMORY_COLUMNS} from triage_memory \
               where project_id = $1 and message_norm % u.msg \
               order by similarity(message_norm, u.msg) desc, last_decided_at desc, fingerprint, defect_type_id \
               limit $3) m"
        ),
    ))
    .bind(project_id)
    .bind(messages)
    .bind(TRIGRAM_ROWS)
    .fetch_all(&mut *conn)
    .await?;
    let mut out: HashMap<usize, Vec<MemoryRow>> = HashMap::new();
    for r in rows {
        out.entry((r.i - 1) as usize)
            .or_default()
            .push(memory_row(r.m));
    }
    Ok(out)
}

#[derive(sqlx::FromRow)]
struct TrigramRow {
    i: i64,
    #[sqlx(flatten)]
    m: MemoryDb,
}

/// The newest `n` memory rows of a project (the candidates scored in Rust without `pg_trgm`).
async fn recent_rows(
    conn: &mut PgConnection,
    project_id: Uuid,
    n: u32,
) -> anyhow::Result<Vec<MemoryRow>> {
    let rows: Vec<MemoryDb> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {MEMORY_COLUMNS} from triage_memory where project_id = $1 \
         order by last_decided_at desc, fingerprint, defect_type_id limit $2"
    )))
    .bind(project_id)
    .bind(n as i64)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(memory_row).collect())
}

fn source_ref(c: &Candidate, m: Option<&MemoryRow>) -> Value {
    json!({
        "fingerprint": c.fingerprint,
        "sampleResultId": m.and_then(|m| m.sample_result_id),
        "sampleLaunchId": m.and_then(|m| m.sample_launch_id),
        "decidedBy": m.map(|m| m.last_decided_by.clone()),
        "decidedAt": m.map(|m| m.last_decided_at),
        "confirmed": c.confirmed,
        "overridden": c.overridden,
        "ruleId": c.rule_id,
        "sameFingerprint": c.same_fingerprint,
    })
}

/// What one cluster got, before persistence.
struct Computed {
    fingerprint: String,
    candidates: Vec<(Candidate, Option<MemoryRow>)>,
}

/// Whether a candidate came from ClickHouse history (night B): its family is `history`.
fn as_history(mut c: Candidate) -> Candidate {
    c.why.push(WhyItem {
        signal: WhySignal::History,
        weight: 0.0,
        detail: "Decided this way in history kept in ClickHouse".into(),
    });
    c.signal = SuggestionSignal::History;
    c
}

/// Auto-apply's outcome for a launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoApplied {
    pub batch_id: Option<Uuid>,
    pub clusters: u64,
    pub results: u64,
}

type SuggestionDbRow = (
    Uuid,
    String,
    i16,
    String,
    f32,
    Uuid,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Value,
    Value,
    String,
    String,
    DateTime<Utc>,
    Uuid,
);

const SUGGESTION_COLUMNS: &str = "id, fingerprint, rank, band, score, defect_type_id, grp, comment, issue_url, issue_key, \
     why, source_ref, signal, state, created_at, launch_id";

fn state_of(s: &str) -> SuggestionState {
    match s {
        "accepted" => SuggestionState::Accepted,
        "rejected" => SuggestionState::Rejected,
        "superseded" => SuggestionState::Superseded,
        "expired" => SuggestionState::Expired,
        _ => SuggestionState::Open,
    }
}

fn signal_name(s: &str) -> SuggestionSignal {
    match s {
        "similar" => SuggestionSignal::Similar,
        "rule" => SuggestionSignal::Rule,
        "history" => SuggestionSignal::History,
        "signal" => SuggestionSignal::Signal,
        _ => SuggestionSignal::Memory,
    }
}

/// The context the launch view and the single-suggestion answers share.
struct ViewCtx {
    names: HashMap<Uuid, String>,
    numbers: HashMap<Uuid, i64>,
    precision: HashMap<SuggestionBand, f64>,
    ruled: BTreeSet<String>,
}

fn suggestion_of(r: SuggestionDbRow, v: &ViewCtx) -> Suggestion {
    let band = SuggestionBand::parse(&r.3).unwrap_or(SuggestionBand::Low);
    let sr = &r.11;
    let confirmed = sr["confirmed"].as_i64().unwrap_or(0) as i32;
    let overridden = sr["overridden"].as_i64().unwrap_or(0) as i32;
    let same = sr["sameFingerprint"].as_bool().unwrap_or(false);
    let sample_launch = sr["sampleLaunchId"]
        .as_str()
        .and_then(|s| s.parse::<Uuid>().ok());
    let sample = sr["decidedAt"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|at| DecisionSample {
            result_id: sr["sampleResultId"]
                .as_str()
                .and_then(|s| s.parse::<Uuid>().ok()),
            launch_id: sample_launch,
            launch_number: sample_launch.and_then(|l| v.numbers.get(&l).copied()),
            decided_by: sr["decidedBy"].as_str().unwrap_or_default().to_string(),
            decided_at: at.with_timezone(&Utc),
            confirmed,
            overridden,
        });
    Suggestion {
        id: r.0,
        rank: r.2,
        band,
        score: ((r.4 as f64) * 1000.0).round() / 1000.0,
        precision: v.precision.get(&band).copied(),
        defect_type_id: r.5,
        group: r.6,
        defect_name: v.names.get(&r.5).cloned().unwrap_or_default(),
        comment: r.7,
        issue_url: r.8,
        issue_key: r.9,
        signal: signal_name(&r.12),
        why: serde_json::from_value(r.10).unwrap_or_default(),
        sample,
        rule_id: sr["ruleId"].as_str().and_then(|s| s.parse::<Uuid>().ok()),
        state: state_of(&r.13),
        rule_draft_offered: same
            && confirmed >= RULE_OFFER_CONFIRMATIONS
            && overridden == 0
            && !v.ruled.contains(&r.1),
    }
}

impl Store {
    async fn view_ctx(&self, project_id: Uuid, launches: &[Uuid]) -> anyhow::Result<ViewCtx> {
        let names: HashMap<Uuid, String> = self
            .defect_types(project_id)
            .await?
            .into_iter()
            .map(|t| (t.id, t.name))
            .collect();
        let numbers: HashMap<Uuid, i64> = sqlx::query_as::<_, (Uuid, i64)>(
            "select id, number from launches where project_id = $1 and id = any($2)",
        )
        .bind(project_id)
        .bind(launches)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect();
        let mut precision = HashMap::new();
        if let Some(r) = self.latest_backtest(project_id).await? {
            for b in r.bands {
                if b.shown
                    && let Some(p) = b.precision
                {
                    precision.insert(b.band, p);
                }
            }
        }
        let ruled: BTreeSet<String> = sqlx::query_scalar::<_, Option<String>>(
            "select match->>'fingerprint' from rules where project_id = $1 and match ? 'fingerprint' limit 10000",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .flatten()
        .collect();
        Ok(ViewCtx {
            names,
            numbers,
            precision,
            ruled,
        })
    }

    /// The undecided clusters of a launch as scoring sees them, without frames (what
    /// ClickHouse history is asked for), at most `max_clusters`.
    pub async fn cluster_facts(&self, ctx: &LaunchCtx<'_>) -> anyhow::Result<Vec<ClusterFacts>> {
        let mut conn = self.pool.acquire().await?;
        let clusters =
            launch_clusters(&mut conn, ctx.launch, ctx.limits.max_clusters.max(1) as i64).await?;
        let canonical: Vec<String> = clusters
            .iter()
            .filter(|c| c.decided.is_none())
            .map(|c| c.fingerprint.clone())
            .collect();
        let cat = catalog(&mut conn, ctx.project_id, &canonical).await?;
        let (env_key, branch): (Option<String>, Option<String>) =
            sqlx::query_as("select env_key, branch from launches where id = $1")
                .bind(ctx.launch.id)
                .fetch_optional(&mut *conn)
                .await?
                .unwrap_or_default();
        Ok(clusters
            .into_iter()
            .filter(|c| c.decided.is_none())
            .map(|c| ClusterFacts {
                message_norm: cat
                    .get(&c.fingerprint)
                    .map(|r| r.1.clone())
                    .unwrap_or_default(),
                aliases: c
                    .fingerprints
                    .iter()
                    .filter(|f| **f != c.fingerprint)
                    .cloned()
                    .collect(),
                fingerprint: c.fingerprint,
                frames: Vec::new(),
                test_case_ids: c.test_case_ids,
                env_key: env_key.clone(),
                branch: branch.clone(),
                results: c.count.max(0) as u32,
                sample_result_id: Some(c.sample_result_id),
            })
            .collect())
    }

    /// Step `suggestions` (and recompute): candidates for every undecided cluster of the
    /// launch (at most `max_clusters`, largest first; `partial` beyond), scored and ranked,
    /// persisted in one transaction (the launch's open suggestions become `superseded`).
    /// `history` holds ClickHouse history rows by cluster fingerprint (night B; empty
    /// without ClickHouse).
    pub async fn compute_suggestions(
        &self,
        ctx: &LaunchCtx<'_>,
        history: &[(String, MemoryRow)],
    ) -> anyhow::Result<StepOutcome> {
        let settings = &ctx.settings.analysis;
        let opts = RankOptions {
            min_score: settings.suggest.min_score,
            max_candidates: (settings.suggest.max_candidates as usize).clamp(1, MAX_CANDIDATES),
        };
        let max = ctx.limits.max_clusters.max(1) as i64;
        let mut conn = self.pool.acquire().await?;
        let mut clusters = launch_clusters(&mut conn, ctx.launch, max + 1).await?;
        let truncated = clusters.len() as i64 > max;
        clusters.truncate(max as usize);
        if truncated {
            metrics::counter!("fastproof_analysis_budget_exhausted_total", "kind" => "clusters")
                .increment(1);
        }
        let open: Vec<&LaunchCluster> = clusters.iter().filter(|c| c.decided.is_none()).collect();
        let canonical: Vec<String> = open.iter().map(|c| c.fingerprint.clone()).collect();
        let aliases = aliases_of(&mut conn, ctx.project_id, &canonical).await?;
        let mut all_fps: BTreeSet<String> = BTreeSet::new();
        for c in &open {
            all_fps.insert(c.fingerprint.clone());
            all_fps.extend(c.fingerprints.iter().cloned());
            if let Some(a) = aliases.get(&c.fingerprint) {
                all_fps.extend(a.iter().cloned());
            }
        }
        let all_fps: Vec<String> = all_fps.into_iter().collect();
        let cat = catalog(&mut conn, ctx.project_id, &canonical).await?;
        let (env_key, branch): (Option<String>, Option<String>) =
            sqlx::query_as("select env_key, branch from launches where id = $1")
                .bind(ctx.launch.id)
                .fetch_optional(&mut *conn)
                .await?
                .unwrap_or_default();
        let same_rows = memory_rows(&mut conn, ctx.project_id, &all_fps).await?;
        let mut by_fp: HashMap<&str, Vec<&MemoryRow>> = HashMap::new();
        for r in &same_rows {
            by_fp.entry(r.fingerprint.as_str()).or_default().push(r);
        }
        let facts: Vec<ClusterFacts> = {
            let samples: Vec<(Uuid, DateTime<Utc>)> = open
                .iter()
                .map(|c| (c.sample_result_id, ctx.launch.created_at))
                .collect();
            let frames = frames_of_results(&mut conn, &samples).await?;
            open.iter()
                .map(|c| {
                    let mut al: Vec<String> = c
                        .fingerprints
                        .iter()
                        .filter(|f| **f != c.fingerprint)
                        .cloned()
                        .collect();
                    if let Some(a) = aliases.get(&c.fingerprint) {
                        for f in a {
                            if !al.contains(f) {
                                al.push(f.clone());
                            }
                        }
                    }
                    ClusterFacts {
                        fingerprint: c.fingerprint.clone(),
                        aliases: al,
                        message_norm: cat
                            .get(&c.fingerprint)
                            .map(|r| r.1.clone())
                            .unwrap_or_default(),
                        frames: frames.get(&c.sample_result_id).cloned().unwrap_or_default(),
                        test_case_ids: c.test_case_ids.clone(),
                        env_key: env_key.clone(),
                        branch: branch.clone(),
                        results: c.count.max(0) as u32,
                        sample_result_id: Some(c.sample_result_id),
                    }
                })
                .collect()
        };
        // Rows similar by message.
        let similar: Vec<Vec<MemoryRow>> = if facts.is_empty() {
            Vec::new()
        } else if self.trigram_installed().await? {
            let messages: Vec<String> = facts.iter().map(|f| f.message_norm.clone()).collect();
            let mut tx = self.pool.begin().await?;
            let mut got = trigram_rows(&mut tx, ctx.project_id, &messages).await?;
            tx.commit().await?;
            (0..facts.len())
                .map(|i| got.remove(&i).unwrap_or_default())
                .collect()
        } else {
            let recent = recent_rows(&mut conn, ctx.project_id, ctx.limits.candidate_rows).await?;
            let keys: Vec<Vec<u64>> = recent
                .iter()
                .map(|r| message_keys(&r.message_norm))
                .collect();
            facts
                .iter()
                .map(|f| {
                    let k = message_keys(&f.message_norm);
                    recent
                        .iter()
                        .zip(&keys)
                        .filter(|(_, rk)| similar_keys(&k, rk))
                        .map(|(r, _)| r.clone())
                        .collect()
                })
                .collect()
        };
        drop(conn);
        // Disabled ingest rules whose dry run matches a cluster's sample.
        let types = self.defect_types(ctx.project_id).await?;
        let group_of: HashMap<Uuid, String> =
            types.iter().map(|t| (t.id, t.group.clone())).collect();
        let mut rule_cands: HashMap<Uuid, Vec<Candidate>> = HashMap::new();
        if !facts.is_empty() {
            let mut rules: Vec<super::rules::Rule> = self
                .rules(ctx.project_id)
                .await?
                .into_iter()
                .filter(|r| {
                    !r.enabled
                        && r.readable()
                        && r.phase() == RulePhase::Ingest
                        && r.action
                            .defect_type_id
                            .is_some_and(|t| group_of.contains_key(&t))
                })
                .collect();
            rules.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
            rules.truncate(MAX_RULE_CANDIDATES);
            let samples: BTreeSet<Uuid> = open.iter().map(|c| c.sample_result_id).collect();
            for rule in rules {
                let (id, name, t) = (
                    rule.id,
                    rule.name.clone(),
                    rule.action.defect_type_id.unwrap_or_default(),
                );
                let Some(Ok(dry)) = self
                    .rule_dry_run(ctx.project_id, ctx.launch.id, rule)
                    .await?
                else {
                    continue;
                };
                for m in dry.matches.into_iter().filter(|m| samples.contains(m)) {
                    rule_cands.entry(m).or_default().push(rule_candidate(
                        id,
                        &name,
                        t,
                        group_of.get(&t).map(String::as_str).unwrap_or_default(),
                        false,
                    ));
                }
            }
        }
        // Signals of the clusters' results only boost.
        let result_ids: Vec<Uuid> = open
            .iter()
            .flat_map(|c| c.result_ids.iter().copied())
            .collect();
        let signals = if result_ids.is_empty() {
            Vec::new()
        } else {
            self.signals_for_results(ctx.project_id, &result_ids)
                .await?
        };
        let mut signals_of: HashMap<Uuid, Vec<super::types::AttemptSignal>> = HashMap::new();
        for s in signals {
            if let Some(r) = s.result_id {
                signals_of.entry(r).or_default().push(s);
            }
        }
        let mut computed: Vec<Computed> = Vec::with_capacity(facts.len());
        for (i, f) in facts.iter().enumerate() {
            let c = open[i];
            let mut rows: Vec<&MemoryRow> = Vec::new();
            let mut seen: BTreeSet<(&str, Uuid)> = BTreeSet::new();
            for fp in std::iter::once(&f.fingerprint).chain(f.aliases.iter()) {
                for r in by_fp.get(fp.as_str()).into_iter().flatten() {
                    if seen.insert((r.fingerprint.as_str(), r.defect_type_id)) {
                        rows.push(r);
                    }
                }
            }
            for r in similar.get(i).into_iter().flatten() {
                if seen.insert((r.fingerprint.as_str(), r.defect_type_id)) {
                    rows.push(r);
                }
            }
            let mut cands: Vec<Candidate> =
                rows.iter().filter_map(|m| score(f, m, ctx.now)).collect();
            for (fp, m) in history {
                if *fp == f.fingerprint
                    && !seen.contains(&(m.fingerprint.as_str(), m.defect_type_id))
                    && let Some(c) = score(f, m, ctx.now)
                {
                    cands.push(as_history(c));
                }
            }
            if let Some(r) = rule_cands.get(&c.sample_result_id) {
                cands.extend(r.iter().cloned());
            }
            let facts_of_signals: Vec<SignalFact> = c
                .result_ids
                .iter()
                .filter_map(|r| signals_of.get(r))
                .flat_map(|s| signal_facts(s))
                .collect();
            let ranked = rank(cands, &opts, &facts_of_signals);
            let row_of = |cand: &Candidate| -> Option<MemoryRow> {
                rows.iter()
                    .find(|m| {
                        m.fingerprint == cand.fingerprint && m.defect_type_id == cand.defect_type_id
                    })
                    .map(|m| (*m).clone())
                    .or_else(|| {
                        history
                            .iter()
                            .find(|(_, m)| {
                                m.fingerprint == cand.fingerprint
                                    && m.defect_type_id == cand.defect_type_id
                            })
                            .map(|(_, m)| m.clone())
                    })
            };
            computed.push(Computed {
                fingerprint: f.fingerprint.clone(),
                candidates: ranked
                    .into_iter()
                    .map(|c| {
                        let m = if c.rule_id.is_some() {
                            None
                        } else {
                            row_of(&c)
                        };
                        (c, m)
                    })
                    .collect(),
            });
        }
        // Persist: the launch's open suggestions superseded, the new ones inserted.
        let mut tx = self.pool.begin().await?;
        let superseded = sqlx::query(
            "update triage_suggestions set state = 'superseded', decided_at = now() \
             where launch_id = $1 and state = 'open'",
        )
        .bind(ctx.launch.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        let mut ids = Vec::new();
        let mut fps = Vec::new();
        let mut ranks: Vec<i16> = Vec::new();
        let mut bands = Vec::new();
        let mut scores: Vec<f32> = Vec::new();
        let mut type_ids = Vec::new();
        let mut groups = Vec::new();
        let mut comments: Vec<Option<String>> = Vec::new();
        let mut issue_urls: Vec<Option<String>> = Vec::new();
        let mut issue_keys: Vec<Option<String>> = Vec::new();
        let mut whys = Vec::new();
        let mut refs = Vec::new();
        let mut sigs = Vec::new();
        let mut per_band: BTreeMap<&'static str, u64> = BTreeMap::new();
        for c in &computed {
            for (k, (cand, m)) in c.candidates.iter().enumerate() {
                let band = cand.band.unwrap_or(SuggestionBand::Low);
                *per_band.entry(band.as_str()).or_default() += 1;
                ids.push(Uuid::now_v7());
                fps.push(no_nul(&c.fingerprint));
                ranks.push(k as i16 + 1);
                bands.push(band.as_str().to_string());
                scores.push(cand.score as f32);
                type_ids.push(cand.defect_type_id);
                groups.push(cand.group.clone());
                comments.push(m.as_ref().and_then(|m| m.comment.as_deref().map(no_nul)));
                issue_urls.push(m.as_ref().and_then(|m| m.issue_url.as_deref().map(no_nul)));
                issue_keys.push(m.as_ref().and_then(|m| m.issue_key.as_deref().map(no_nul)));
                whys.push(json_no_nul(serde_json::to_value(&cand.why)?));
                refs.push(json_no_nul(source_ref(cand, m.as_ref())));
                sigs.push(cand.signal.as_str().to_string());
            }
        }
        if !ids.is_empty() {
            sqlx::query(
                "insert into triage_suggestions (id, project_id, launch_id, fingerprint, rank, band, score, \
                   defect_type_id, grp, comment, issue_url, issue_key, why, source_ref, signal) \
                 select i, $1, $2, f, r, b, s, t, g, c, u, k, w, x, sg \
                 from unnest($3::uuid[], $4::text[], $5::int2[], $6::text[], $7::float4[], $8::uuid[], \
                   $9::text[], $10::text[], $11::text[], $12::text[], $13::jsonb[], $14::jsonb[], $15::text[]) \
                   as t(i, f, r, b, s, t, g, c, u, k, w, x, sg)",
            )
            .bind(ctx.project_id)
            .bind(ctx.launch.id)
            .bind(&ids)
            .bind(&fps)
            .bind(&ranks)
            .bind(&bands)
            .bind(&scores)
            .bind(&type_ids)
            .bind(&groups)
            .bind(&comments)
            .bind(&issue_urls)
            .bind(&issue_keys)
            .bind(&whys)
            .bind(&refs)
            .bind(&sigs)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        for (band, n) in &per_band {
            metrics::counter!("fastproof_analysis_suggestions_total", "band" => *band, "outcome" => "created")
                .increment(*n);
        }
        if superseded > 0 {
            metrics::counter!("fastproof_analysis_suggestions_total", "band" => "any", "outcome" => "superseded")
                .increment(superseded);
        }
        let detail = json!({"clusters": clusters.len(), "undecided": open.len(), "suggested": ids.len(),
            "bands": per_band, "superseded": superseded, "truncated": truncated});
        Ok(if truncated {
            StepOutcome::partial(detail)
        } else {
            StepOutcome::done(detail)
        })
    }

    /// The suggestions of a launch by cluster (`GET .../launches/{id}/suggestions`): open and
    /// accepted ones, with the band's backtest precision when the latest backtest had at least
    /// 50 decisions in it.
    pub async fn launch_suggestions(
        &self,
        project_id: Uuid,
        launch: &Launch,
    ) -> AnalysisResult<LaunchSuggestions> {
        let settings = self.project_settings(project_id).await?;
        if !settings.analysis.enabled {
            return Ok(LaunchSuggestions {
                launch_id: launch.id,
                state: LaunchSuggestionsState::Disabled,
                computed_at: None,
                clusters: Vec::new(),
                truncated: false,
            });
        }
        let mut conn = self.pool.acquire().await?;
        let mut clusters = launch_clusters(&mut conn, launch, MAX_VIEW_CLUSTERS + 1).await?;
        let truncated = clusters.len() as i64 > MAX_VIEW_CLUSTERS;
        clusters.truncate(MAX_VIEW_CLUSTERS as usize);
        let rows: Vec<SuggestionDbRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {SUGGESTION_COLUMNS} from triage_suggestions \
             where launch_id = $1 and project_id = $2 and state in ('open', 'accepted') \
             order by fingerprint, state desc, rank"
        )))
        .bind(launch.id)
        .bind(project_id)
        .fetch_all(&mut *conn)
        .await?;
        let canonical: Vec<String> = clusters.iter().map(|c| c.fingerprint.clone()).collect();
        let cat = catalog(&mut conn, project_id, &canonical).await?;
        let messages: HashMap<Uuid, Option<String>> = sqlx::query_as::<_, (Uuid, Option<String>)>(
            "select id, left(message, 300) from results where id = any($1) and launch_created_at = $2",
        )
        .bind(clusters.iter().map(|c| c.sample_result_id).collect::<Vec<_>>())
        .bind(launch.created_at)
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .collect();
        let has_memory: bool =
            sqlx::query_scalar("select exists(select 1 from triage_memory where project_id = $1)")
                .bind(project_id)
                .fetch_one(&mut *conn)
                .await?;
        drop(conn);
        let status = self.analysis_launch_status(launch.id).await?;
        let sample_launches: Vec<Uuid> = rows
            .iter()
            .filter_map(|r| r.11["sampleLaunchId"].as_str()?.parse().ok())
            .collect();
        let v = self.view_ctx(project_id, &sample_launches).await?;
        let result_ids: Vec<Uuid> = clusters
            .iter()
            .flat_map(|c| c.result_ids.iter().copied())
            .collect();
        let signals = if result_ids.is_empty() {
            Vec::new()
        } else {
            self.signals_for_results(project_id, &result_ids).await?
        };
        let mut by_result: HashMap<Uuid, Vec<&super::types::AttemptSignal>> = HashMap::new();
        for s in &signals {
            if let Some(r) = s.result_id {
                by_result.entry(r).or_default().push(s);
            }
        }
        let computed_at = rows.iter().map(|r| r.14).max();
        let mut by_fp: HashMap<String, Vec<Suggestion>> = HashMap::new();
        for r in rows {
            let fp = r.1.clone();
            by_fp.entry(fp).or_default().push(suggestion_of(r, &v));
        }
        let mut out = Vec::with_capacity(clusters.len());
        for c in clusters {
            let suggestions = by_fp.remove(&c.fingerprint).unwrap_or_default();
            let row = cat.get(&c.fingerprint);
            let is_new = row.is_some_and(|r| r.2 == Some(launch.id));
            let error_state = row.and_then(|r| ErrorState::parse(&r.3));
            let commits = row.and_then(|r| {
                (r.4.is_some() || r.5.is_some()).then(|| CommitRange {
                    first: r.4.clone(),
                    last_good: r.5.clone(),
                    compare_url: None,
                })
            });
            let mut briefs: BTreeMap<(String, String, String), AttemptSignalBrief> =
                BTreeMap::new();
            for s in c
                .result_ids
                .iter()
                .filter_map(|r| by_result.get(r))
                .flatten()
            {
                let key = (
                    s.source.as_str().to_string(),
                    s.kind.clone(),
                    format!("{:?}", s.confidence),
                );
                briefs
                    .entry(key)
                    .or_insert_with(|| AttemptSignalBrief {
                        source: s.source,
                        kind: s.kind.clone(),
                        confidence: s.confidence,
                        count: 0,
                    })
                    .count += 1;
            }
            let no_suggestion = if c.decided.is_some() {
                Some(NoSuggestion::Decided)
            } else if !suggestions.is_empty() {
                None
            } else if is_new || error_state == Some(ErrorState::New) {
                Some(NoSuggestion::NewError)
            } else if !has_memory {
                Some(NoSuggestion::NoHistory)
            } else {
                Some(NoSuggestion::BelowThreshold)
            };
            out.push(ClusterSuggestions {
                message: messages.get(&c.sample_result_id).cloned().flatten(),
                count: c.count,
                sample_result_id: c.sample_result_id,
                defect: c.defect,
                decided: c.decided,
                is_new,
                new_on_branch: false,
                error_state,
                commits,
                signals: briefs.into_values().collect(),
                suggestions,
                no_suggestion,
                fingerprint: c.fingerprint,
            });
        }
        let state = match status.as_ref().map(|s| s.state) {
            Some(super::types::AnalysisLaunchState::Done)
            | Some(super::types::AnalysisLaunchState::Failed) => {
                let partial = status
                    .as_ref()
                    .and_then(|s| s.steps.suggestions.as_ref())
                    .is_some_and(|s| s.status == super::types::AnalysisStepStatus::Partial);
                if partial || truncated {
                    LaunchSuggestionsState::Partial
                } else {
                    LaunchSuggestionsState::Ready
                }
            }
            _ if computed_at.is_some() => LaunchSuggestionsState::Ready,
            _ => LaunchSuggestionsState::Pending,
        };
        Ok(LaunchSuggestions {
            launch_id: launch.id,
            state,
            computed_at: computed_at.or(status.and_then(|s| s.finished_at)),
            clusters: out,
            truncated,
        })
    }

    async fn suggestion_row(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<SuggestionDbRow>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {SUGGESTION_COLUMNS} from triage_suggestions where project_id = $1 and id = $2"
        )))
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// One suggestion as the API shows it; `None` when it is not of the project.
    pub async fn suggestion(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<Suggestion>> {
        let Some(r) = self.suggestion_row(project_id, id).await? else {
            return Ok(None);
        };
        let launches: Vec<Uuid> = r.11["sampleLaunchId"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .into_iter()
            .collect();
        let v = self.view_ctx(project_id, &launches).await?;
        Ok(Some(suggestion_of(r, &v)))
    }

    /// The launch and fingerprint of a suggestion of the project.
    pub async fn suggestion_target(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<(Uuid, String, Uuid, SuggestionState)>> {
        let r: Option<(Uuid, String, Uuid, String)> = sqlx::query_as(
            "select launch_id, fingerprint, defect_type_id, state from triage_suggestions \
             where project_id = $1 and id = $2",
        )
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(|(l, f, t, s)| (l, f, t, state_of(&s))))
    }

    /// Accepts an open suggestion: its defect type (with the comment and issue given) on the
    /// cluster's results (`applyTo: cluster`, default) or on listed results of the cluster,
    /// leaving results a person or an agent decided untouched; journaled with `via =
    /// suggestion` and the suggestion's id, as `user` for people and `ai` for agents. The
    /// suggestion becomes `accepted`, the cluster's other open suggestions `superseded`.
    pub async fn accept_suggestion(
        &self,
        project_id: Uuid,
        project_key: &str,
        id: Uuid,
        req: &SuggestionAccept,
        by: &str,
        actor: &AcceptActor,
    ) -> AnalysisResult<SuggestionAccepted> {
        let Some((launch_id, fp, type_id, state)) = self.suggestion_target(project_id, id).await?
        else {
            return Err(TmError::NotFound("suggestion"));
        };
        if state != SuggestionState::Open {
            return Err(closed(state));
        }
        let Some(launch) = self.launch(project_id, launch_id).await? else {
            return Err(TmError::NotFound("launch"));
        };
        let mut conn = self.pool.acquire().await?;
        let clusters = launch_clusters(&mut conn, &launch, MAX_VIEW_CLUSTERS).await?;
        drop(conn);
        let Some(cluster) = clusters.into_iter().find(|c| c.fingerprint == fp) else {
            return Err(TmError::conflict(
                "suggestion_closed",
                "the cluster of this suggestion has no failed results any more",
            ));
        };
        let apply_to = req.apply_to.unwrap_or(SuggestionApplyTo::Cluster);
        let mut tr = TriageRequest {
            defect_type_id: Some(type_id),
            comment: req.comment.clone(),
            issue: req.issue.clone(),
            source: Some(if actor.agent { "ai" } else { "user" }.into()),
            ..Default::default()
        };
        let total: u64;
        match apply_to {
            SuggestionApplyTo::Results => {
                let ids = req.result_ids.clone().unwrap_or_default();
                if ids.is_empty() || ids.len() > CLUSTER_RESULTS {
                    return Err(TmError::bad_request(format!(
                        "resultIds must list 1 to {CLUSTER_RESULTS} results of the cluster"
                    )));
                }
                let of_cluster: i64 = sqlx::query_scalar(
                    "select count(*) from results where project_id = $1 and launch_id = $2 \
                       and launch_created_at = $3 and id = any($4) and fingerprint = any($5) \
                       and status in ('failed', 'broken')",
                )
                .bind(project_id)
                .bind(launch.id)
                .bind(launch.created_at)
                .bind(&ids)
                .bind(&cluster.fingerprints)
                .fetch_one(&self.pool)
                .await?;
                let distinct: BTreeSet<Uuid> = ids.iter().copied().collect();
                if of_cluster as usize != distinct.len() {
                    return Err(TmError::bad_request(
                        "resultIds must be failed results of the suggestion's cluster",
                    ));
                }
                total = distinct.len() as u64;
                tr.result_ids = Some(distinct.into_iter().collect());
                tr.launch_id = Some(launch.id);
            }
            SuggestionApplyTo::Cluster => {
                if req.result_ids.is_some() {
                    return Err(TmError::bad_request("resultIds are for applyTo: results"));
                }
                total = cluster.count.max(0) as u64;
                if cluster.fingerprints.len() == 1 {
                    tr.launch_id = Some(launch.id);
                    tr.fingerprint = Some(cluster.fingerprints[0].clone());
                } else {
                    tr.result_ids = Some(cluster.result_ids.clone());
                }
            }
        }
        tr.validate(Utc::now()).map_err(TmError::bad_request)?;
        // Claimed first, so two accepts of one suggestion never both apply.
        let claimed = sqlx::query(
            "update triage_suggestions set state = 'accepted', decided_at = now(), decided_by = $3 \
             where project_id = $1 and id = $2 and state = 'open'",
        )
        .bind(project_id)
        .bind(id)
        .bind(no_nul(by))
        .execute(&self.pool)
        .await?
        .rows_affected();
        if claimed == 0 {
            let now_state = self
                .suggestion_target(project_id, id)
                .await?
                .map(|t| t.3)
                .unwrap_or(SuggestionState::Superseded);
            return Err(closed(now_state));
        }
        let ctx = DecisionCtx {
            via: DecisionVia::Suggestion,
            suggestion_id: Some(id),
            source: Some(if actor.agent {
                DecisionSource::Ai
            } else {
                DecisionSource::User
            }),
            skip_protected: true,
            reverts: None,
        };
        let outcome = self
            .triage_with(project_id, project_key, &tr, by, &ctx)
            .await;
        let out = match outcome {
            Ok(Ok(o)) => o,
            other => {
                sqlx::query(
                    "update triage_suggestions set state = 'open', decided_at = null, decided_by = null \
                     where id = $1 and state = 'accepted'",
                )
                .bind(id)
                .execute(&self.pool)
                .await?;
                return match other {
                    Ok(Err(super::triage::TriageRefusal::UnknownDefectType)) => Err(
                        TmError::conflict("suggestion_closed", "the suggested defect type is gone"),
                    ),
                    Ok(Err(super::triage::TriageRefusal::UnknownLaunch)) => {
                        Err(TmError::NotFound("launch"))
                    }
                    Err(e) => Err(e.into()),
                    Ok(Ok(_)) => unreachable!(),
                };
            }
        };
        sqlx::query(
            "update triage_suggestions set state = 'superseded', decided_at = now() \
             where launch_id = $1 and fingerprint = $2 and state = 'open' and id <> $3",
        )
        .bind(launch.id)
        .bind(&fp)
        .bind(id)
        .execute(&self.pool)
        .await?;
        let band: Option<String> =
            sqlx::query_scalar("select band from triage_suggestions where id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        metrics::counter!("fastproof_analysis_suggestions_total",
            "band" => band.unwrap_or_default(), "outcome" => "accepted")
        .increment(1);
        Ok(SuggestionAccepted {
            suggestion_id: id,
            updated: out.updated,
            skipped: total.saturating_sub(out.updated),
            batch_id: out.batch_id,
        })
    }

    /// Rejects an open suggestion (with an optional reason of at most 500 bytes); the memory is
    /// not touched.
    pub async fn reject_suggestion(
        &self,
        project_id: Uuid,
        id: Uuid,
        req: &SuggestionReject,
        by: &str,
    ) -> AnalysisResult<Suggestion> {
        if req
            .reason
            .as_ref()
            .is_some_and(|r| r.len() > MAX_REJECT_REASON)
        {
            return Err(TmError::bad_request(format!(
                "reason must have at most {MAX_REJECT_REASON} bytes"
            )));
        }
        let r: Option<String> = sqlx::query_scalar(
            "update triage_suggestions set state = 'rejected', decided_at = now(), decided_by = $3, \
               reject_reason = $4 where project_id = $1 and id = $2 and state = 'open' returning band",
        )
        .bind(project_id)
        .bind(id)
        .bind(no_nul(by))
        .bind(req.reason.as_deref().map(no_nul))
        .fetch_optional(&self.pool)
        .await?;
        match r {
            Some(band) => {
                metrics::counter!("fastproof_analysis_suggestions_total", "band" => band, "outcome" => "rejected")
                    .increment(1);
            }
            None => {
                return match self.suggestion_target(project_id, id).await? {
                    None => Err(TmError::NotFound("suggestion")),
                    Some(t) => Err(closed(t.3)),
                };
            }
        }
        self.suggestion(project_id, id)
            .await?
            .ok_or(TmError::NotFound("suggestion"))
    }

    /// Auto-apply at the end of the `suggestions` step: when `analysis.autoApply.enabled` and
    /// the analysis gate is open, each undecided cluster whose rank-1 open suggestion is
    /// `same` gets that defect type (nothing else: no comment, issue or mute) on its results
    /// that no person or agent decided; one journal batch for the launch (`via = auto`,
    /// `source = auto`, by [`AUTO_PRINCIPAL`]); the suggestions become `accepted`.
    pub async fn auto_apply(&self, ctx: &LaunchCtx<'_>) -> anyhow::Result<Option<AutoApplied>> {
        let a = &ctx.settings.analysis.auto_apply;
        if !a.enabled || !ctx.settings.analysis.enabled {
            return Ok(None);
        }
        {
            let mut conn = self.pool.acquire().await?;
            if !super::gate::is_open(&mut conn, ctx.limits).await? {
                return Ok(None);
            }
        }
        let bands: Vec<&str> = a.bands.iter().map(|b| b.as_str()).collect();
        let picks: Vec<(Uuid, String, Uuid)> = sqlx::query_as(
            "select id, fingerprint, defect_type_id from triage_suggestions \
             where launch_id = $1 and project_id = $2 and state = 'open' and rank = 1 and band = any($3) \
             order by fingerprint",
        )
        .bind(ctx.launch.id)
        .bind(ctx.project_id)
        .bind(&bands)
        .fetch_all(&self.pool)
        .await?;
        if picks.is_empty() {
            return Ok(Some(AutoApplied {
                batch_id: None,
                clusters: 0,
                results: 0,
            }));
        }
        let mut conn = self.pool.acquire().await?;
        let clusters = launch_clusters(&mut conn, ctx.launch, MAX_VIEW_CLUSTERS).await?;
        drop(conn);
        let by_fp: HashMap<&str, &LaunchCluster> = clusters
            .iter()
            .map(|c| (c.fingerprint.as_str(), c))
            .collect();
        let mut chosen: Vec<(Uuid, &LaunchCluster, Uuid)> = Vec::new();
        for (sid, fp, t) in &picks {
            if let Some(c) = by_fp.get(fp.as_str())
                && c.decided.is_none()
            {
                chosen.push((*sid, c, *t));
            }
        }
        let out = self.write_auto(ctx, &chosen).await?;
        Ok(Some(out))
    }

    async fn write_auto(
        &self,
        ctx: &LaunchCtx<'_>,
        chosen: &[(Uuid, &LaunchCluster, Uuid)],
    ) -> anyhow::Result<AutoApplied> {
        let launch = ctx.launch;
        let mut attempt = 1;
        loop {
            match self.write_auto_once(ctx, chosen).await {
                Err(e)
                    if attempt < crate::ingest::TX_ATTEMPTS
                        && crate::ingest::retryable(&e).is_some() =>
                {
                    crate::ingest::retry_pause("auto-apply", &e, attempt).await;
                    attempt += 1;
                }
                r => {
                    if let Ok(a) = &r
                        && a.clusters > 0
                    {
                        metrics::counter!("fastproof_analysis_suggestions_total", "band" => "same", "outcome" => "auto")
                            .increment(a.clusters);
                    }
                    let _ = launch;
                    return r;
                }
            }
        }
    }

    async fn write_auto_once(
        &self,
        ctx: &LaunchCtx<'_>,
        chosen: &[(Uuid, &LaunchCluster, Uuid)],
    ) -> anyhow::Result<AutoApplied> {
        let launch = ctx.launch;
        let mut tx = self.pool.begin().await?;
        // The order every writer of results takes: the launch row share-locked, the result
        // rows in id order, the launch row for counters at the end (none here).
        sqlx::query("select 1 from launches where id = $1 for key share")
            .bind(launch.id)
            .execute(&mut *tx)
            .await?;
        let mut type_of: HashMap<String, Uuid> = HashMap::new();
        let mut fps: Vec<String> = Vec::new();
        for (_, c, t) in chosen {
            for f in &c.fingerprints {
                type_of.insert(f.clone(), *t);
                fps.push(f.clone());
            }
        }
        let targets: Vec<(Uuid, String, Option<Value>)> = sqlx::query_as(
            "select id, fingerprint, defect from results where launch_id = $1 and launch_created_at = $2 \
               and status in ('failed', 'broken') and fingerprint = any($3) \
             order by id for no key update",
        )
        .bind(launch.id)
        .bind(launch.created_at)
        .bind(&fps)
        .fetch_all(&mut *tx)
        .await?;
        let keys: Vec<(Uuid, DateTime<Utc>)> =
            targets.iter().map(|t| (t.0, launch.created_at)).collect();
        let mut before = read_states(&mut tx, &keys).await?;
        let protected: BTreeSet<Uuid> = before
            .iter()
            .filter(|b| matches!(b.defect_source(), Some("user" | "ai")))
            .map(|b| b.result_id)
            .collect();
        before.retain(|b| !protected.contains(&b.result_id));
        let (by_id, _) = type_refs(&mut tx, ctx.project_id).await?;
        let now = Utc::now();
        let mut ids = Vec::new();
        let mut defects = Vec::new();
        for (rid, fp, d) in targets.iter().filter(|t| !protected.contains(&t.0)) {
            let Some(t) = type_of.get(fp).and_then(|t| by_id.get(t)) else {
                continue;
            };
            let existing: Option<Defect> = d.clone().and_then(|d| serde_json::from_value(d).ok());
            let defect = Defect {
                type_id: t.id,
                group: t.group.clone(),
                name: t.name.clone(),
                comment: existing.as_ref().and_then(|d| d.comment.clone()),
                issue_url: existing.as_ref().and_then(|d| d.issue_url.clone()),
                issue_key: existing.as_ref().and_then(|d| d.issue_key.clone()),
                source: "auto".into(),
                rule_id: None,
                at: now,
                by: AUTO_PRINCIPAL.into(),
                accepted: existing.as_ref().is_some_and(|d| d.accepted),
            };
            ids.push(*rid);
            defects.push(json_no_nul(serde_json::to_value(defect)?));
        }
        if ids.is_empty() {
            tx.rollback().await?;
            return Ok(AutoApplied {
                batch_id: None,
                clusters: 0,
                results: 0,
            });
        }
        sqlx::query(
            "update results r set defect = t.d, updated_at = now() \
             from unnest($1::uuid[], $2::jsonb[]) as t(i, d) \
             where r.id = t.i and r.launch_created_at = $3",
        )
        .bind(&ids)
        .bind(&defects)
        .bind(launch.created_at)
        .execute(&mut *tx)
        .await?;
        let after_keys: Vec<(Uuid, DateTime<Utc>)> =
            ids.iter().map(|i| (*i, launch.created_at)).collect();
        let after = read_states(&mut tx, &after_keys).await?;
        let batch_id = record(
            &mut tx,
            &Batch {
                project_id: ctx.project_id,
                project_key: ctx.project_key,
                via: DecisionVia::Auto,
                source: DecisionSource::Auto,
                by: AUTO_PRINCIPAL,
                suggestion_id: None,
                reverts: None,
            },
            &before,
            &after,
        )
        .await?;
        let applied: Vec<Uuid> = chosen.iter().map(|c| c.0).collect();
        sqlx::query(
            "update triage_suggestions set state = 'accepted', decided_at = now(), decided_by = $2 \
             where id = any($1) and state = 'open'",
        )
        .bind(&applied)
        .bind(AUTO_PRINCIPAL)
        .execute(&mut *tx)
        .await?;
        let cfps: Vec<String> = chosen.iter().map(|c| c.1.fingerprint.clone()).collect();
        sqlx::query(
            "update triage_suggestions set state = 'superseded', decided_at = now() \
             where launch_id = $1 and fingerprint = any($2) and state = 'open'",
        )
        .bind(launch.id)
        .bind(&cfps)
        .execute(&mut *tx)
        .await?;
        crate::events::emit(
            &mut tx,
            "results.triaged",
            Some(ctx.project_key),
            json!({"launchIds": [launch.id], "count": ids.len(), "by": AUTO_PRINCIPAL,
                "defectTypeId": null, "mute": false, "unmute": false}),
        )
        .await?;
        crate::events::notify_launch(&mut tx, ctx.project_key, launch.id).await?;
        let _ = recount_muted;
        tx.commit().await?;
        Ok(AutoApplied {
            batch_id,
            clusters: chosen.len() as u64,
            results: ids.len() as u64,
        })
    }
}

fn closed(state: SuggestionState) -> TmError {
    let s = serde_json::to_value(state).unwrap_or(Value::Null);
    TmError::Conflict {
        code: "suggestion_closed",
        message: format!(
            "the suggestion is {} (only open suggestions can be accepted or rejected)",
            s.as_str().unwrap_or("closed")
        ),
        details: Some(json!({"state": s})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_names_the_state() {
        let e = closed(SuggestionState::Accepted);
        match e {
            TmError::Conflict { code, details, .. } => {
                assert_eq!(code, "suggestion_closed");
                assert_eq!(details.unwrap()["state"], "accepted");
            }
            _ => panic!("conflict expected"),
        }
        assert_eq!(state_of("rejected"), SuggestionState::Rejected);
        assert_eq!(signal_name("rule"), SuggestionSignal::Rule);
    }
}
