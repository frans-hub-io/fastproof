//! Triage rules (like Allure resolutions): conditions on a failed result (regular expressions on
//! the message and trace, test case, fingerprint, environment, status) and an action (defect
//! type, comment, issue, mute, accept). Rules run on ingest, on demand for a launch, and as a
//! dry run.
//!
//! Regular expressions use the `regex` crate, which matches in time linear in the input (no
//! backtracking, no backreferences or lookaround). Patterns are bounded in length and compiled
//! size, inputs are cut to [`MATCH_INPUT_BYTES`], and each evaluation pass has a [`Budget`].

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use fastproof_model::Status;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{Store, json_no_nul, no_nul};

/// Rules per project, at most.
pub const MAX_RULES: i64 = 100;
/// Longest pattern, in bytes.
pub const MAX_PATTERN_BYTES: usize = 1024;
/// Largest compiled program of a pattern.
pub const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// Largest lazy DFA cache of a pattern.
pub const REGEX_DFA_LIMIT: usize = 2 << 20;
/// Deepest nesting of a pattern.
pub const REGEX_NEST_LIMIT: u32 = 50;
/// Bytes of a message or trace that patterns see.
pub const MATCH_INPUT_BYTES: usize = 16 * 1024;
/// Rule evaluations per ingest chunk.
pub const INGEST_EVALUATIONS: u64 = 50_000;
/// Bytes scanned by patterns per ingest chunk.
pub const INGEST_SCAN_BYTES: u64 = 64 << 20;
/// Rule evaluations of one dry run or retroactive pass over a launch.
pub const LAUNCH_EVALUATIONS: u64 = 1_000_000;
/// Bytes scanned by one dry run or retroactive pass over a launch.
pub const LAUNCH_SCAN_BYTES: u64 = 1 << 30;

/// Compiles a user pattern under the size limits.
pub fn compile_regex(p: &str) -> Result<Regex, String> {
    if p.is_empty() {
        return Err("empty pattern".into());
    }
    if p.len() > MAX_PATTERN_BYTES {
        return Err(format!("pattern longer than {MAX_PATTERN_BYTES} bytes"));
    }
    RegexBuilder::new(p)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_LIMIT)
        .nest_limit(REGEX_NEST_LIMIT)
        .build()
        .map_err(|e| match e {
            regex::Error::CompiledTooBig(_) => {
                "pattern is too complex (compiled size limit exceeded)".to_string()
            }
            regex::Error::Syntax(s) => {
                let first = s.lines().last().unwrap_or("invalid syntax").trim();
                format!("invalid pattern: {first}")
            }
            other => format!("invalid pattern: {other}"),
        })
}

/// The first `MATCH_INPUT_BYTES` of a text, on a character boundary.
pub fn match_input(s: &str) -> &str {
    if s.len() <= MATCH_INPUT_BYTES {
        return s;
    }
    let mut end = MATCH_INPUT_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// An issue link.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IssueRef {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// Mute effect of a rule.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MuteAction {
    /// Mute for this many days (1..=365); without it the mute has no end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<u32>,
}

/// Conditions of a rule; all given ones must hold.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleMatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_regex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_regex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_case_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// Environment key of the launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// Result statuses; default `failed` and `broken`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statuses: Option<Vec<String>>,
}

/// Effects of a rule.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleAction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defect_type_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<IssueRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<MuteAction>,
    /// Mark the failure as accepted (a known, tolerated failure).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept: Option<bool>,
}

/// A rule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub order: i32,
    #[serde(rename = "match")]
    pub matcher: RuleMatch,
    pub action: RuleAction,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub hits: i64,
}

/// Body of `POST .../rules` (and the rule of a dry run).
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewRule {
    pub name: String,
    pub enabled: Option<bool>,
    pub order: Option<i32>,
    #[serde(rename = "match")]
    pub matcher: RuleMatch,
    pub action: RuleAction,
}

/// Body of `PATCH .../rules/{id}`.
#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RulePatch {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub order: Option<i32>,
    #[serde(rename = "match")]
    pub matcher: Option<RuleMatch>,
    pub action: Option<RuleAction>,
}

fn http_url(u: &str) -> bool {
    u.len() <= 2048
        && url::Url::parse(u)
            .map(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
            .unwrap_or(false)
}

/// Checks an issue link.
pub fn validate_issue(i: &IssueRef) -> Result<(), String> {
    if !http_url(&i.url) {
        return Err("issue.url must be an http(s) URL of at most 2048 bytes".into());
    }
    if let Some(k) = &i.key
        && (k.is_empty() || k.len() > 128)
    {
        return Err("issue.key must have 1 to 128 bytes".into());
    }
    Ok(())
}

/// Whether a string is a fingerprint (16 lowercase hex digits).
pub fn valid_fingerprint(f: &str) -> bool {
    f.len() == 16
        && f.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Checks a rule's name, conditions and action (not whether the defect type exists).
pub fn validate_rule(name: &str, m: &RuleMatch, a: &RuleAction) -> Result<(), String> {
    if name.trim().is_empty() || name.len() > 200 {
        return Err("name must have 1 to 200 bytes".into());
    }
    if m.message_regex.is_none()
        && m.trace_regex.is_none()
        && m.test_case_id.is_none()
        && m.fingerprint.is_none()
        && m.environment.is_none()
    {
        return Err(
            "match needs messageRegex, traceRegex, testCaseId, fingerprint or environment".into(),
        );
    }
    if let Some(p) = &m.message_regex {
        compile_regex(p).map_err(|e| format!("match.messageRegex: {e}"))?;
    }
    if let Some(p) = &m.trace_regex {
        compile_regex(p).map_err(|e| format!("match.traceRegex: {e}"))?;
    }
    if let Some(f) = &m.fingerprint
        && !valid_fingerprint(f)
    {
        return Err("match.fingerprint must be 16 hex digits".into());
    }
    if let Some(e) = &m.environment
        && (e.is_empty() || e.len() > 256)
    {
        return Err("match.environment must have 1 to 256 bytes".into());
    }
    if let Some(st) = &m.statuses {
        if st.is_empty() || st.len() > 5 {
            return Err("match.statuses must list 1 to 5 statuses".into());
        }
        for s in st {
            if !matches!(
                s.as_str(),
                "passed" | "failed" | "broken" | "skipped" | "unknown"
            ) {
                return Err("match.statuses: passed, failed, broken, skipped or unknown".into());
            }
        }
    }
    if a.defect_type_id.is_none()
        && a.comment.is_none()
        && a.issue.is_none()
        && a.mute.is_none()
        && a.accept.is_none()
    {
        return Err("action needs defectTypeId, comment, issue, mute or accept".into());
    }
    if let Some(c) = &a.comment
        && c.len() > 4000
    {
        return Err("action.comment must have at most 4000 bytes".into());
    }
    if let Some(i) = &a.issue {
        validate_issue(i).map_err(|e| format!("action.{e}"))?;
    }
    if let Some(MuteAction { days: Some(d) }) = &a.mute
        && !(1..=365).contains(d)
    {
        return Err("action.mute.days must be 1..=365".into());
    }
    Ok(())
}

/// Limits of one evaluation pass.
#[derive(Clone, Debug)]
pub struct Budget {
    pub evaluations: u64,
    pub bytes: u64,
    pub exhausted: bool,
}

impl Budget {
    pub fn new(evaluations: u64, bytes: u64) -> Self {
        Self {
            evaluations,
            bytes,
            exhausted: false,
        }
    }

    /// The budget of one ingest chunk.
    pub fn ingest() -> Self {
        Self::new(INGEST_EVALUATIONS, INGEST_SCAN_BYTES)
    }

    /// The budget of a pass over one launch.
    pub fn launch() -> Self {
        Self::new(LAUNCH_EVALUATIONS, LAUNCH_SCAN_BYTES)
    }

    fn take(&mut self, evaluations: u64, bytes: u64) -> bool {
        if self.exhausted || evaluations > self.evaluations || bytes > self.bytes {
            self.exhausted = true;
            return false;
        }
        self.evaluations -= evaluations;
        self.bytes -= bytes;
        true
    }
}

/// What a rule looks at.
#[derive(Clone, Copy, Debug)]
pub struct Subject<'a> {
    pub status: &'a str,
    pub message: Option<&'a str>,
    pub trace: Option<&'a str>,
    pub test_case_id: Uuid,
    pub fingerprint: Option<&'a str>,
    pub environment: Option<&'a str>,
}

/// A rule with its patterns compiled.
#[derive(Debug)]
pub struct CompiledRule {
    pub rule: Rule,
    message: Option<Regex>,
    trace: Option<Regex>,
    statuses: Vec<Status>,
}

impl CompiledRule {
    pub fn compile(rule: Rule) -> Result<Self, String> {
        let message = rule
            .matcher
            .message_regex
            .as_deref()
            .map(compile_regex)
            .transpose()?;
        let trace = rule
            .matcher
            .trace_regex
            .as_deref()
            .map(compile_regex)
            .transpose()?;
        let statuses = match &rule.matcher.statuses {
            Some(s) => s.iter().map(|s| Status::parse_lenient(s)).collect(),
            None => vec![Status::Failed, Status::Broken],
        };
        Ok(Self {
            rule,
            message,
            trace,
            statuses,
        })
    }

    pub fn needs_trace(&self) -> bool {
        self.trace.is_some()
    }

    /// `Some(matched)`, or `None` when the budget ran out.
    pub fn matches(&self, s: &Subject<'_>, budget: &mut Budget) -> Option<bool> {
        if !budget.take(1, 0) {
            return None;
        }
        let m = &self.rule.matcher;
        if !self.statuses.contains(&Status::parse_lenient(s.status)) {
            return Some(false);
        }
        if let Some(tc) = m.test_case_id
            && tc != s.test_case_id
        {
            return Some(false);
        }
        if let Some(f) = &m.fingerprint
            && s.fingerprint != Some(f.as_str())
        {
            return Some(false);
        }
        if let Some(e) = &m.environment
            && s.environment != Some(e.as_str())
        {
            return Some(false);
        }
        for (re, text) in [(&self.message, s.message), (&self.trace, s.trace)] {
            if let Some(re) = re {
                let input = match_input(text.unwrap_or(""));
                if !budget.take(0, input.len() as u64) {
                    return None;
                }
                if !re.is_match(input) {
                    return Some(false);
                }
            }
        }
        Some(true)
    }
}

/// The evaluation budget ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExhausted;

/// The first rule (in order) that matches.
pub fn first_match<'r>(
    rules: &'r [Arc<CompiledRule>],
    s: &Subject<'_>,
    budget: &mut Budget,
) -> Result<Option<&'r CompiledRule>, BudgetExhausted> {
    for r in rules {
        match r.matches(s, budget) {
            None => return Err(BudgetExhausted),
            Some(true) => return Ok(Some(r)),
            Some(false) => {}
        }
    }
    Ok(None)
}

/// Compiled rules by id: version, last use, rule; and when idle entries were last dropped.
struct Cache {
    rules: HashMap<Uuid, (DateTime<Utc>, Instant, Arc<CompiledRule>)>,
    swept: Instant,
}
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| {
    Mutex::new(Cache {
        rules: HashMap::new(),
        swept: Instant::now(),
    })
});
/// Compiled rules kept per replica, at most.
const CACHE_ENTRIES: usize = 2000;
/// Compiled rules not used for this long are dropped (deleted rules among them).
const CACHE_IDLE: Duration = Duration::from_secs(600);

fn compiled(rule: Rule) -> Option<Arc<CompiledRule>> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.swept.elapsed() > Duration::from_secs(60) {
        cache
            .rules
            .retain(|_, (_, used, _)| used.elapsed() < CACHE_IDLE);
        cache.swept = Instant::now();
    }
    if let Some((at, used, c)) = cache.rules.get_mut(&rule.id)
        && *at == rule.updated_at
    {
        *used = Instant::now();
        return Some(c.clone());
    }
    let (id, at) = (rule.id, rule.updated_at);
    match CompiledRule::compile(rule) {
        Ok(c) => {
            if cache.rules.len() >= CACHE_ENTRIES {
                cache
                    .rules
                    .retain(|_, (_, used, _)| used.elapsed() < CACHE_IDLE);
                if cache.rules.len() >= CACHE_ENTRIES {
                    cache.rules.clear();
                }
            }
            let c = Arc::new(c);
            cache.rules.insert(id, (at, Instant::now(), c.clone()));
            Some(c)
        }
        Err(e) => {
            tracing::warn!(rule = %id, error = %e, "a stored rule does not compile; skipped");
            None
        }
    }
}

/// Drops a rule from this replica's compiled-rule cache (after it changed or was deleted).
pub fn forget_compiled(id: Uuid) {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .rules
        .remove(&id);
}

/// Rules in this replica's compiled-rule cache (tests).
pub fn compiled_cached(id: Uuid) -> bool {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .rules
        .contains_key(&id)
}

const COLUMNS: &str = "id, name, enabled, ord, match, action, created_at, updated_at, hits";

fn rule_row(r: &sqlx::postgres::PgRow) -> Result<Rule, sqlx::Error> {
    let m: serde_json::Value = r.try_get("match")?;
    let a: serde_json::Value = r.try_get("action")?;
    Ok(Rule {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        enabled: r.try_get("enabled")?,
        order: r.try_get("ord")?,
        matcher: serde_json::from_value(m).unwrap_or_default(),
        action: serde_json::from_value(a).unwrap_or_default(),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        hits: r.try_get("hits")?,
    })
}

/// The enabled rules of a project in evaluation order, compiled (cached by version).
pub async fn enabled_rules(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> anyhow::Result<Vec<Arc<CompiledRule>>> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {COLUMNS} from rules where project_id = $1 and enabled order by ord, created_at, id limit {MAX_RULES}"
    )))
    .bind(project_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        if let Some(c) = compiled(rule_row(r)?) {
            out.push(c);
        }
    }
    Ok(out)
}

/// Adds hits to rules.
pub async fn add_hits(conn: &mut PgConnection, hits: &HashMap<Uuid, i64>) -> anyhow::Result<()> {
    if hits.is_empty() {
        return Ok(());
    }
    let mut ids: Vec<Uuid> = hits.keys().copied().collect();
    ids.sort();
    let n: Vec<i64> = ids.iter().map(|i| hits[i]).collect();
    sqlx::query(
        "update rules r set hits = r.hits + t.n, last_hit_at = now() \
         from unnest($1::uuid[], $2::bigint[]) as t(i, n) where r.id = t.i",
    )
    .bind(&ids)
    .bind(&n)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Outcome of creating a rule.
#[derive(Debug)]
pub enum RuleChange {
    Done(Box<Rule>),
    NotFound,
    Conflict(String),
}

impl Store {
    pub async fn rules(&self, project_id: Uuid) -> anyhow::Result<Vec<Rule>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from rules where project_id = $1 order by ord, created_at, id limit 1000"
        )))
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(rule_row).collect::<Result<_, _>>()?)
    }

    pub async fn rule(&self, project_id: Uuid, id: Uuid) -> anyhow::Result<Option<Rule>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from rules where project_id = $1 and id = $2"
        )))
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.as_ref().map(rule_row).transpose()?)
    }

    /// Creates a rule (validated by the caller).
    pub async fn create_rule(
        &self,
        project_id: Uuid,
        r: &NewRule,
        created_by: &str,
    ) -> anyhow::Result<RuleChange> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select id from projects where id = $1 for update")
            .bind(project_id)
            .execute(&mut *tx)
            .await?;
        let n: i64 = sqlx::query_scalar("select count(*) from rules where project_id = $1")
            .bind(project_id)
            .fetch_one(&mut *tx)
            .await?;
        if n >= MAX_RULES {
            return Ok(RuleChange::Conflict(format!(
                "at most {MAX_RULES} rules per project"
            )));
        }
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into rules (id, project_id, name, enabled, ord, match, action, created_by) \
             values ($1, $2, $3, $4, $5, $6, $7, $8) returning {COLUMNS}"
        )))
        .bind(Uuid::now_v7())
        .bind(project_id)
        .bind(no_nul(r.name.trim()))
        .bind(r.enabled.unwrap_or(true))
        .bind(r.order.unwrap_or(0))
        .bind(json_no_nul(serde_json::to_value(&r.matcher)?))
        .bind(json_no_nul(serde_json::to_value(&r.action)?))
        .bind(no_nul(created_by))
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(RuleChange::Done(Box::new(rule_row(&row)?)))
    }

    /// Applies a patch (validated by the caller on the merged rule).
    pub async fn update_rule(&self, project_id: Uuid, rule: &Rule) -> anyhow::Result<RuleChange> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update rules set name = $3, enabled = $4, ord = $5, match = $6, action = $7, \
             updated_at = greatest(now(), updated_at + interval '1 microsecond') \
             where project_id = $1 and id = $2 returning {COLUMNS}"
        )))
        .bind(project_id)
        .bind(rule.id)
        .bind(no_nul(rule.name.trim()))
        .bind(rule.enabled)
        .bind(rule.order)
        .bind(json_no_nul(serde_json::to_value(&rule.matcher)?))
        .bind(json_no_nul(serde_json::to_value(&rule.action)?))
        .fetch_optional(&self.pool)
        .await?;
        forget_compiled(rule.id);
        Ok(match row {
            Some(r) => RuleChange::Done(Box::new(rule_row(&r)?)),
            None => RuleChange::NotFound,
        })
    }

    pub async fn delete_rule(&self, project_id: Uuid, id: Uuid) -> anyhow::Result<bool> {
        let r = sqlx::query("delete from rules where project_id = $1 and id = $2")
            .bind(project_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        forget_compiled(id);
        Ok(r.rows_affected() == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(m: RuleMatch) -> CompiledRule {
        CompiledRule::compile(Rule {
            id: Uuid::now_v7(),
            name: "r".into(),
            enabled: true,
            order: 0,
            matcher: m,
            action: RuleAction {
                comment: Some("c".into()),
                ..Default::default()
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
            hits: 0,
        })
        .unwrap()
    }

    fn subject<'a>(msg: &'a str, trace: Option<&'a str>) -> Subject<'a> {
        Subject {
            status: "failed",
            message: Some(msg),
            trace,
            test_case_id: Uuid::nil(),
            fingerprint: None,
            environment: Some("staging"),
        }
    }

    #[test]
    fn catastrophic_patterns_are_bounded_or_rejected() {
        // Exponential for backtracking engines, linear here.
        for p in [r"(a+)+$", r"(a|aa)+$", r"(.*a){20}$", r"^(\w+\s?)*$"] {
            let r = rule(RuleMatch {
                message_regex: Some(p.into()),
                ..Default::default()
            });
            let input = format!("{}!", "a".repeat(1 << 20));
            let started = Instant::now();
            let mut b = Budget::ingest();
            let _ = r.matches(&subject(&input, None), &mut b);
            assert!(
                started.elapsed().as_millis() < 2000,
                "{p} took {:?}",
                started.elapsed()
            );
            assert_eq!(b.bytes, INGEST_SCAN_BYTES - MATCH_INPUT_BYTES as u64);
        }
        for p in [r"(a)\1", r"(?=a)b", r"(?<!a)b"] {
            assert!(
                compile_regex(p).unwrap_err().contains("invalid pattern"),
                "{p}"
            );
        }
        for p in [
            r"(?:a{1000}){1000}",
            r"\w{1000}\w{1000}\w{1000}",
            r"(((a{100}){100}){100})",
        ] {
            assert!(compile_regex(p).unwrap_err().contains("too complex"), "{p}");
        }
        let deep = format!("{}a{}", "(".repeat(200), ")".repeat(200));
        assert!(compile_regex(&deep).is_err());
        assert!(compile_regex(&"a".repeat(MAX_PATTERN_BYTES + 1)).is_err());
    }

    #[test]
    fn budget_stops_evaluation() {
        let rules: Vec<Arc<CompiledRule>> = (0..10)
            .map(|i| {
                Arc::new(rule(RuleMatch {
                    message_regex: Some(format!("never{i}")),
                    ..Default::default()
                }))
            })
            .collect();
        let msg = "x".repeat(10_000);
        let mut b = Budget::new(25, u64::MAX);
        assert!(
            first_match(&rules, &subject(&msg, None), &mut b)
                .unwrap()
                .is_none()
        );
        assert!(
            first_match(&rules, &subject(&msg, None), &mut b)
                .unwrap()
                .is_none()
        );
        assert!(first_match(&rules, &subject(&msg, None), &mut b).is_err());
        assert!(b.exhausted);
        let mut b = Budget::new(u64::MAX, 25_000);
        assert!(first_match(&rules, &subject(&msg, None), &mut b).is_err());
    }

    #[test]
    fn conditions_combine() {
        let r = rule(RuleMatch {
            message_regex: Some("(?i)connection refused".into()),
            trace_regex: Some(r"VaultClient\.connect".into()),
            environment: Some("staging".into()),
            ..Default::default()
        });
        let mut b = Budget::ingest();
        let t = "java.io.IOException\n\tat com.example.VaultClient.connect(VaultClient.java:10)";
        assert_eq!(
            r.matches(&subject("Connection refused", Some(t)), &mut b),
            Some(true)
        );
        assert_eq!(
            r.matches(&subject("Connection refused", None), &mut b),
            Some(false)
        );
        let mut s = subject("Connection refused", Some(t));
        s.environment = Some("prod");
        assert_eq!(r.matches(&s, &mut b), Some(false));
        s.environment = Some("staging");
        s.status = "passed";
        assert_eq!(r.matches(&s, &mut b), Some(false));
    }

    #[test]
    fn validation() {
        let a = RuleAction {
            mute: Some(MuteAction { days: Some(7) }),
            ..Default::default()
        };
        assert!(validate_rule("r", &RuleMatch::default(), &a).is_err());
        let m = RuleMatch {
            fingerprint: Some("0123456789abcdef".into()),
            ..Default::default()
        };
        assert!(validate_rule("r", &m, &a).is_ok());
        assert!(validate_rule("r", &m, &RuleAction::default()).is_err());
        let bad = RuleAction {
            issue: Some(IssueRef {
                url: "javascript:alert(1)".into(),
                key: None,
            }),
            ..Default::default()
        };
        assert!(validate_rule("r", &m, &bad).is_err());
        let big = RuleAction {
            mute: Some(MuteAction { days: Some(1000) }),
            ..Default::default()
        };
        assert!(validate_rule("r", &m, &big).is_err());
    }
}
