//! Version review of cases and shared steps (§7.3). F writes the policy reads and the runnable
//! version rule; the review area fills the save hooks, votes, bulk review, the queue and the
//! review requests the notifications report.
//!
//! State machine of the head version (`review_state`, `approved_version`), for objects in the
//! policy's scope:
//! - a save that changes review-relevant keys (`name`, `preconditions`, `steps`, `parameters`;
//!   with `resetOn: any` every content key but `status`; code-owned keys of automated cases
//!   never count) makes the head `pending`; `approved_version` becomes the previous version
//!   when the object was never under review, else it stays (a new object starts with none);
//! - any other save keeps the state, an approved head carries `approved_version` along, and a
//!   head with requested changes waits for votes again (`pending`);
//! - an import with `imports: bypass` approves the new head;
//! - votes are on the head only: any `request_changes` → `changes_requested`, else `approvals`
//!   distinct approvers → `approved` (`approved_version` = head), else `pending`.

use std::collections::{BTreeSet, HashMap};

use base64::Engine;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Postgres, QueryBuilder, Row};
use uuid::Uuid;

use super::cases::{CaseCtx, MAX_BULK_NOTIFY, code_owned_keys, lock_case_by_number};
use super::lookup::{SHARED_STEP_COLS, case_by_number, shared_step_by_number, shared_step_row};
use super::versions::{changed_keys, snapshot_of};
use super::{
    CaseBody, CaseReviewSettings, CaseRow, CaseSnapshot, ReviewBulk, ReviewBulkResult,
    ReviewBulkSkip, ReviewDecision, ReviewImports, ReviewKind, ReviewMode, ReviewQueue,
    ReviewQueueItem, ReviewResetOn, ReviewState, ReviewThread, ReviewVote, ReviewVoteWrite,
    SharedStepOrigin, SharedStepRow, TmError, TmResult, VersionSource,
};
use crate::analysis::settings::ProjectSettings;
use crate::events::{LiveEvent, emit_many, notify};
use crate::{Store, no_nul};

/// Bytes of a vote comment, at most.
pub const MAX_COMMENT_BYTES: usize = 4000;
/// Votes a review thread lists, newest first, at most.
pub const MAX_THREAD_VOTES: i64 = 200;
/// Objects a review request lists, at most.
pub const MAX_REQUEST_ITEMS: i64 = 10;
/// Authors of new review requests read for the "never to the actor alone" rule, at most.
pub const MAX_REQUEST_AUTHORS: i64 = 50;
/// How far back the throttle of review requests looks for a notification's last delivery.
pub const REQUEST_HORIZON: std::time::Duration = std::time::Duration::from_secs(25 * 3600);

/// The smallest UUIDv7 of a time: deliveries (whose ids are UUIDv7) newer than it are read
/// through the `(project_id, id)` index instead of every delivery of the project.
fn v7_floor(t: DateTime<Utc>) -> Uuid {
    uuid::Builder::from_unix_timestamp_millis(t.timestamp_millis().max(0) as u64, &[0; 10])
        .into_uuid()
}

/// Content keys of a case that reset an approval with `resetOn: content`.
pub const CONTENT_RESET_KEYS: &[&str] = &["name", "preconditions", "steps", "parameters"];
/// Keys of a shared step that reset an approval with `resetOn: content`.
pub const SHARED_CONTENT_RESET_KEYS: &[&str] = &["name", "steps"];

/// The `caseReview` policy of a project (defaults when absent or invalid).
pub async fn review_settings(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> anyhow::Result<CaseReviewSettings> {
    let v: Option<Value> = sqlx::query_scalar("select settings from projects where id = $1")
        .bind(project_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(ProjectSettings::from_value(v.unwrap_or(Value::Null)).case_review)
}

/// SQL of the runnable version of a case over alias `c`: the head (`c.version`) when review is
/// off, when the case is out of scope (`manual` mode and automated) or was never edited under
/// review (`review_state is null`); otherwise `c.approved_version` (null: not runnable).
pub fn runnable_version_sql(policy: &CaseReviewSettings) -> &'static str {
    match policy.mode {
        ReviewMode::Off => "c.version",
        ReviewMode::Manual => {
            "(case when c.automated or c.review_state is null then c.version else c.approved_version end)"
        }
        ReviewMode::All => {
            "(case when c.review_state is null then c.version else c.approved_version end)"
        }
    }
}

/// Whether a changed case key needs a new review under `policy`. `owned` are the code-owned
/// keys of an automated case; `fields` is decided by the field values themselves (a change of
/// code-owned field values only does not count).
fn case_key_counts(
    policy: &CaseReviewSettings,
    key: &str,
    owned: &[String],
    before: &CaseSnapshot,
    after: &CaseSnapshot,
) -> bool {
    let in_set = match policy.reset_on {
        ReviewResetOn::Content => CONTENT_RESET_KEYS.contains(&key),
        ReviewResetOn::Any => key != "status",
    };
    if !in_set {
        return false;
    }
    if key == "fields" {
        let keys: BTreeSet<&String> = before.fields.keys().chain(after.fields.keys()).collect();
        return keys
            .into_iter()
            .any(|k| !owned.contains(k) && before.fields.get(k) != after.fields.get(k));
    }
    !owned.iter().any(|o| o == key)
}

fn empty_snapshot(after: &CaseSnapshot) -> CaseSnapshot {
    CaseSnapshot {
        name: String::new(),
        status: after.status,
        folder_id: None,
        body: CaseBody::default(),
        fields: Default::default(),
        tags: Vec::new(),
    }
}

/// Whether a case save changes keys that need a new review.
pub fn case_change_counts(
    policy: &CaseReviewSettings,
    before: Option<&CaseRow>,
    after: &CaseRow,
    changed: &[String],
    owned: &[String],
) -> bool {
    let a = snapshot_of(after);
    let (b, keys) = match before {
        Some(b) => (snapshot_of(b), changed.to_vec()),
        None => {
            let b = empty_snapshot(&a);
            let keys = changed_keys(&b, &a);
            (b, keys)
        }
    };
    keys.iter()
        .any(|k| case_key_counts(policy, k, owned, &b, &a))
}

/// Whether a shared step save changes keys that need a new review.
pub fn shared_change_counts(
    policy: &CaseReviewSettings,
    before: Option<&SharedStepRow>,
    after: &SharedStepRow,
    changed: &[String],
) -> bool {
    let counts = |k: &str| match policy.reset_on {
        ReviewResetOn::Content => SHARED_CONTENT_RESET_KEYS.contains(&k),
        ReviewResetOn::Any => true,
    };
    match before {
        Some(_) => changed.iter().any(|k| counts(k)),
        None => {
            counts("name")
                || (!after.description.is_empty() && counts("description"))
                || (!after.steps.is_empty() && counts("steps"))
        }
    }
}

/// The review state and approved version of a head after a save (§7.3). `bypass` (an import
/// with `imports: bypass`) approves the new head only when nothing else waits for review: an
/// import never approves earlier unreviewed changes of people along with its own.
pub fn next_state(
    before: Option<(Option<ReviewState>, Option<i32>, i32)>,
    new_version: i32,
    relevant: bool,
    bypass: bool,
) -> (Option<ReviewState>, Option<i32>) {
    let waiting = matches!(
        before,
        Some((
            Some(ReviewState::Pending | ReviewState::ChangesRequested),
            _,
            _
        ))
    );
    if bypass && !waiting {
        return (Some(ReviewState::Approved), Some(new_version));
    }
    let (state, approved, prev_version) = match before {
        Some(b) => b,
        None => {
            return if relevant {
                (Some(ReviewState::Pending), None)
            } else {
                (None, None)
            };
        }
    };
    if relevant {
        let approved = match state {
            None => Some(prev_version),
            Some(_) => approved,
        };
        return (Some(ReviewState::Pending), approved);
    }
    match state {
        None => (None, approved),
        Some(ReviewState::Approved) => (Some(ReviewState::Approved), Some(new_version)),
        Some(ReviewState::Pending) | Some(ReviewState::ChangesRequested) => {
            (Some(ReviewState::Pending), approved)
        }
    }
}

/// Whether writing `new` over the case `cur` makes a head that waits for review (the policy's
/// scope and reset keys, code-owned keys of automated cases left out).
pub async fn write_needs_review(
    conn: &mut PgConnection,
    project_id: Uuid,
    cur: &CaseRow,
    new: &CaseSnapshot,
) -> TmResult<bool> {
    let policy = review_settings(conn, project_id).await?;
    if !policy.in_scope(cur.automated) {
        return Ok(false);
    }
    let owned = if cur.automated {
        code_owned_keys(conn, project_id).await?
    } else {
        Vec::new()
    };
    let before = snapshot_of(cur);
    Ok(changed_keys(&before, new)
        .iter()
        .any(|k| case_key_counts(&policy, k, &owned, &before, new)))
}

/// After a case version was written: moves the review state of the head (§7.3). Nothing is
/// written for a case out of the policy's scope.
pub async fn on_save(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    before: Option<&CaseRow>,
    after: &CaseRow,
    changed: &[String],
    source: VersionSource,
) -> TmResult<()> {
    let policy = review_settings(conn, ctx.project_id).await?;
    if !policy.in_scope(after.automated) {
        return Ok(());
    }
    let owned = if after.automated {
        code_owned_keys(conn, ctx.project_id).await?
    } else {
        Vec::new()
    };
    let relevant = case_change_counts(&policy, before, after, changed, &owned);
    let bypass = source == VersionSource::Import && policy.imports == ReviewImports::Bypass;
    let (state, approved) = next_state(
        before.map(|b| (b.review_state, b.approved_version, b.version)),
        after.version,
        relevant,
        bypass,
    );
    if state != after.review_state || approved != after.approved_version {
        sqlx::query("update cases set review_state = $2, approved_version = $3 where id = $1")
            .bind(after.id)
            .bind(state.map(|s| s.as_str()))
            .bind(approved)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// The review state imported cases created in bulk take (`tm/import/plan.rs` inserts them
/// without [`on_save`]): `pending` with no approved version, or `approved` at version 1 with
/// `imports: bypass`, for the cases in the policy's scope.
pub async fn on_bulk_create(
    conn: &mut PgConnection,
    project_id: Uuid,
    ids: &[Uuid],
    source: VersionSource,
) -> TmResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let policy = review_settings(conn, project_id).await?;
    if policy.mode == ReviewMode::Off {
        return Ok(());
    }
    let bypass = source == VersionSource::Import && policy.imports == ReviewImports::Bypass;
    let (state, approved) = if bypass {
        (ReviewState::Approved, Some(1))
    } else {
        (ReviewState::Pending, None)
    };
    sqlx::query(
        "update cases set review_state = $3, approved_version = $4 \
         where project_id = $1 and id = any($2) and ($5 or not automated)",
    )
    .bind(project_id)
    .bind(ids)
    .bind(state.as_str())
    .bind(approved)
    .bind(policy.mode == ReviewMode::All)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// After a shared step version was written: moves its review state (§7.3; shared steps are
/// in scope whenever review is on).
pub async fn on_shared_step_save(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    before: Option<&SharedStepRow>,
    after: &SharedStepRow,
    changed: &[String],
    source: VersionSource,
) -> TmResult<()> {
    let policy = review_settings(conn, ctx.project_id).await?;
    if !policy.in_scope(false) {
        return Ok(());
    }
    let relevant = shared_change_counts(&policy, before, after, changed);
    let bypass = source == VersionSource::Import && policy.imports == ReviewImports::Bypass;
    let (state, approved) = next_state(
        before.map(|b| (b.review_state, b.approved_version, b.version)),
        after.version,
        relevant,
        bypass,
    );
    if state != after.review_state || approved != after.approved_version {
        sqlx::query(
            "update shared_steps set review_state = $2, approved_version = $3 where id = $1",
        )
        .bind(after.id)
        .bind(state.map(|s| s.as_str()))
        .bind(approved)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// What is reviewed: a case or a shared step, as the vote rules see it.
#[derive(Clone, Debug)]
struct Subject {
    kind: ReviewKind,
    id: Uuid,
    number: i64,
    name: String,
    version: i32,
    approved_version: Option<i32>,
    review_state: Option<ReviewState>,
    automated: bool,
    revision: i64,
    /// The creator of an unreviewed AI draft.
    ai_draft_by: Option<String>,
    case: Option<CaseRow>,
}

impl Subject {
    fn of_case(c: &CaseRow) -> Self {
        Self {
            kind: ReviewKind::Case,
            id: c.id,
            number: c.number,
            name: c.name.clone(),
            version: c.version,
            approved_version: c.approved_version,
            review_state: c.review_state,
            automated: c.automated,
            revision: c.revision,
            ai_draft_by: (c.origin == super::CaseOrigin::Ai && c.ai_unreviewed)
                .then(|| c.created_by.clone())
                .flatten(),
            case: Some(c.clone()),
        }
    }

    fn of_shared(s: &SharedStepRow) -> Self {
        Self {
            kind: ReviewKind::SharedStep,
            id: s.id,
            number: s.number,
            name: s.name.clone(),
            version: s.version,
            approved_version: s.approved_version,
            review_state: s.review_state,
            automated: false,
            revision: s.revision,
            ai_draft_by: (s.origin == SharedStepOrigin::Ai && s.ai_unreviewed)
                .then(|| s.created_by.clone())
                .flatten(),
            case: None,
        }
    }
}

fn table(kind: ReviewKind) -> &'static str {
    match kind {
        ReviewKind::Case => "cases",
        ReviewKind::SharedStep => "shared_steps",
    }
}

fn not_found(kind: ReviewKind) -> TmError {
    match kind {
        ReviewKind::Case => TmError::NotFound("case"),
        ReviewKind::SharedStep => TmError::NotFound("shared step"),
    }
}

/// Why a principal may not vote on the head: the code of the refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    NotEnabled,
    VersionMoved(i32),
    NotPending,
    AiSelfReview,
    SelfApproval,
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Refusal::NotEnabled => "review_not_enabled",
            Refusal::VersionMoved(_) => "version_moved",
            Refusal::NotPending => "not_pending",
            Refusal::AiSelfReview => super::ai::AI_SELF_REVIEW,
            Refusal::SelfApproval => "self_approval",
        }
    }

    fn error(self) -> TmError {
        match self {
            Refusal::NotEnabled => {
                TmError::conflict("review_not_enabled", "review is off for this object")
            }
            Refusal::VersionMoved(v) => TmError::Conflict {
                code: "version_moved",
                message: format!("the head is version {v} now"),
                details: Some(json!({"currentVersion": v})),
            },
            Refusal::NotPending => {
                TmError::conflict("not_pending", "the head version is not waiting for review")
            }
            Refusal::AiSelfReview => TmError::Forbidden {
                code: super::ai::AI_SELF_REVIEW,
                message:
                    "an AI draft is reviewed by someone other than the principal that created it"
                        .into(),
            },
            Refusal::SelfApproval => TmError::Forbidden {
                code: "self_approval",
                message: "the author of a version does not review it".into(),
            },
        }
    }
}

/// Whether two principal names are the same principal (names compare without case: an
/// identity provider or a token name may spell one principal in several ways).
pub fn same_principal(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// The first rule a vote of `by` on `version` breaks, if any. `authors` are the authors (in
/// lower case) of the change under review ([`change_authors`]).
fn refusal(
    policy: &CaseReviewSettings,
    s: &Subject,
    authors: &BTreeSet<String>,
    by: &str,
    version: i32,
) -> Option<Refusal> {
    let in_scope = match s.kind {
        ReviewKind::Case => policy.in_scope(s.automated),
        ReviewKind::SharedStep => policy.in_scope(false),
    };
    if !in_scope {
        return Some(Refusal::NotEnabled);
    }
    if version != s.version {
        return Some(Refusal::VersionMoved(s.version));
    }
    if !matches!(
        s.review_state,
        Some(ReviewState::Pending) | Some(ReviewState::ChangesRequested)
    ) {
        return Some(Refusal::NotPending);
    }
    if s.ai_draft_by
        .as_deref()
        .is_some_and(|a| same_principal(a, by))
    {
        return Some(Refusal::AiSelfReview);
    }
    if !policy.allow_self_approval && authors.contains(&by.to_lowercase()) {
        return Some(Refusal::SelfApproval);
    }
    None
}

/// The version keys whose change makes the author of a version an author of the change under
/// review (`None`: every change counts): the keys that reset an approval under the policy.
fn authored_keys(policy: &CaseReviewSettings, kind: ReviewKind) -> Option<Vec<String>> {
    let keys: &[&str] = match (policy.reset_on, kind) {
        (ReviewResetOn::Content, ReviewKind::Case) => CONTENT_RESET_KEYS,
        (ReviewResetOn::Content, ReviewKind::SharedStep) => SHARED_CONTENT_RESET_KEYS,
        (ReviewResetOn::Any, ReviewKind::Case) => super::versions::CONTENT_KEYS,
        (ReviewResetOn::Any, ReviewKind::SharedStep) => return None,
    };
    Some(
        keys.iter()
            .filter(|k| **k != "status")
            .map(|k| k.to_string())
            .collect(),
    )
}

/// The lowest version (exclusive) whose author wrote part of the change under review: the
/// approved version, else none; the head always counts.
fn change_floor(s: &Subject) -> i32 {
    s.approved_version.unwrap_or(0).min(s.version - 1)
}

/// The authors (in lower case) of the change under review of each subject: who wrote the head
/// and every version after the approved one that changed keys [`authored_keys`] names. A
/// person who made any part of the change does not approve it, even when someone else wrote a
/// later version (a tag, a transition, a remap) on top.
async fn change_authors(
    conn: &mut PgConnection,
    policy: &CaseReviewSettings,
    kind: ReviewKind,
    subjects: &[Subject],
) -> sqlx::Result<HashMap<Uuid, BTreeSet<String>>> {
    let mut out: HashMap<Uuid, BTreeSet<String>> = HashMap::new();
    if subjects.is_empty() {
        return Ok(out);
    }
    let ids: Vec<Uuid> = subjects.iter().map(|s| s.id).collect();
    let floors: Vec<i32> = subjects.iter().map(change_floor).collect();
    let heads: Vec<i32> = subjects.iter().map(|s| s.version).collect();
    let (table, col) = versions_table(kind);
    let rows: Vec<(Uuid, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select v.{col}, lower(v.created_by) from {table} v \
         join unnest($1::uuid[], $2::int[], $3::int[]) as h(id, lo, hi) \
           on v.{col} = h.id and v.version > h.lo and v.version <= h.hi \
         where v.version = h.hi or $4::text[] is null or cardinality(v.changed) = 0 \
           or v.changed && $4::text[] \
         group by 1, 2"
    )))
    .bind(&ids)
    .bind(&floors)
    .bind(&heads)
    .bind(authored_keys(policy, kind))
    .fetch_all(&mut *conn)
    .await?;
    for (id, a) in rows {
        out.entry(id).or_default().insert(a);
    }
    Ok(out)
}

/// The version table of a kind and its object column.
fn versions_table(kind: ReviewKind) -> (&'static str, &'static str) {
    match kind {
        ReviewKind::Case => ("case_versions", "case_id"),
        ReviewKind::SharedStep => ("shared_step_versions", "shared_step_id"),
    }
}

/// The state the votes on a head give it.
fn decide(votes: &HashMap<String, ReviewDecision>, required: i32) -> ReviewState {
    if votes.values().any(|d| *d == ReviewDecision::RequestChanges) {
        ReviewState::ChangesRequested
    } else if votes.len() as i64 >= required as i64 {
        ReviewState::Approved
    } else {
        ReviewState::Pending
    }
}

fn check_comment(c: Option<&str>) -> TmResult<String> {
    let c = c.unwrap_or_default();
    if c.len() > MAX_COMMENT_BYTES {
        return Err(TmError::bad_request(format!(
            "comment: at most {MAX_COMMENT_BYTES} bytes"
        )));
    }
    if c.chars()
        .any(|ch| ch.is_control() && ch != '\n' && ch != '\t' && ch != '\r')
    {
        return Err(TmError::bad_request(
            "comment: no control characters but line breaks and tabs",
        ));
    }
    Ok(no_nul(c))
}

fn forbidden_without_review() -> TmError {
    TmError::Forbidden {
        code: "forbidden",
        message: "voting needs cases:review".into(),
    }
}

async fn lock_shared(
    conn: &mut PgConnection,
    project_id: Uuid,
    n: i64,
    expected: Option<i64>,
) -> TmResult<SharedStepRow> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {SHARED_STEP_COLS} from shared_steps where project_id = $1 and number = $2 for update"
    )))
    .bind(project_id)
    .bind(n)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(TmError::NotFound("shared step"))?;
    let row = shared_step_row(&r)?;
    if let Some(e) = expected
        && e != row.revision
    {
        return Err(TmError::Stale {
            current_revision: row.revision,
            changed_by: row.updated_by.clone(),
            source: None,
        });
    }
    Ok(row)
}

async fn lock_subject(
    conn: &mut PgConnection,
    project_id: Uuid,
    kind: ReviewKind,
    n: i64,
    expected: Option<i64>,
) -> TmResult<Subject> {
    Ok(match kind {
        ReviewKind::Case => {
            Subject::of_case(&lock_case_by_number(conn, project_id, n, expected).await?)
        }
        ReviewKind::SharedStep => {
            Subject::of_shared(&lock_shared(conn, project_id, n, expected).await?)
        }
    })
}

async fn read_subject(
    conn: &mut PgConnection,
    project_id: Uuid,
    kind: ReviewKind,
    n: i64,
) -> TmResult<Subject> {
    Ok(match kind {
        ReviewKind::Case => Subject::of_case(
            &case_by_number(conn, project_id, n)
                .await?
                .ok_or(not_found(kind))?,
        ),
        ReviewKind::SharedStep => Subject::of_shared(
            &shared_step_by_number(conn, project_id, n)
                .await?
                .ok_or(not_found(kind))?,
        ),
    })
}

/// Authors of versions: `(object id, version)` → `created_by`.
async fn authors(
    conn: &mut PgConnection,
    kind: ReviewKind,
    heads: &[(Uuid, i32)],
) -> sqlx::Result<HashMap<Uuid, String>> {
    if heads.is_empty() {
        return Ok(HashMap::new());
    }
    let (ids, versions): (Vec<Uuid>, Vec<i32>) = heads.iter().copied().unzip();
    let sql = match kind {
        ReviewKind::Case => {
            "select v.case_id, v.created_by from case_versions v \
             join unnest($1::uuid[], $2::int[]) as h(id, ver) on v.case_id = h.id and v.version = h.ver"
        }
        ReviewKind::SharedStep => {
            "select v.shared_step_id, v.created_by from shared_step_versions v \
             join unnest($1::uuid[], $2::int[]) as h(id, ver) on v.shared_step_id = h.id and v.version = h.ver"
        }
    };
    let rows: Vec<(Uuid, String)> = sqlx::query_as(sql)
        .bind(&ids)
        .bind(&versions)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.into_iter().collect())
}

/// Votes on heads: object id → reviewer (in lower case: one principal counts once however it
/// is spelled, its request for changes winning) → decision.
async fn head_votes(
    conn: &mut PgConnection,
    kind: ReviewKind,
    heads: &[(Uuid, i32)],
) -> sqlx::Result<HashMap<Uuid, HashMap<String, ReviewDecision>>> {
    let mut out: HashMap<Uuid, HashMap<String, ReviewDecision>> = HashMap::new();
    if heads.is_empty() {
        return Ok(out);
    }
    let (ids, versions): (Vec<Uuid>, Vec<i32>) = heads.iter().copied().unzip();
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "select v.object_id, v.reviewer, v.decision from review_votes v \
         join unnest($2::uuid[], $3::int[]) as h(id, ver) on v.object_id = h.id and v.version = h.ver \
         where v.kind = $1",
    )
    .bind(kind.as_str())
    .bind(&ids)
    .bind(&versions)
    .fetch_all(&mut *conn)
    .await?;
    for (id, reviewer, d) in rows {
        if let Some(d) = ReviewDecision::parse(&d) {
            let e = out
                .entry(id)
                .or_default()
                .entry(reviewer.to_lowercase())
                .or_insert(d);
            if d == ReviewDecision::RequestChanges {
                *e = d;
            }
        }
    }
    Ok(out)
}

fn event_kind(kind: ReviewKind) -> &'static str {
    match kind {
        ReviewKind::Case => "case.reviewed",
        ReviewKind::SharedStep => "sharedStep.reviewed",
    }
}

/// One applied vote: the object, the new state and whether the row changed.
struct Applied {
    id: Uuid,
    number: i64,
    version: i32,
    state: ReviewState,
    approved: Option<i32>,
    changed: bool,
}

/// Records the votes of `by` on the heads of `subjects` (already checked) and moves their
/// states; one statement per step for all of them.
async fn apply_votes(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    policy: &CaseReviewSettings,
    kind: ReviewKind,
    subjects: &[Subject],
    decision: ReviewDecision,
    comment: &str,
) -> TmResult<Vec<Applied>> {
    if subjects.is_empty() {
        return Ok(Vec::new());
    }
    let heads: Vec<(Uuid, i32)> = subjects.iter().map(|s| (s.id, s.version)).collect();
    let (ids, versions): (Vec<Uuid>, Vec<i32>) = heads.iter().copied().unzip();
    sqlx::query(
        "insert into review_votes (project_id, kind, object_id, version, reviewer, decision, comment) \
         select $1, $2, t.id, t.ver, $3, $4, $5 from unnest($6::uuid[], $7::int[]) as t(id, ver) \
         on conflict (kind, object_id, version, reviewer) do update \
           set decision = excluded.decision, comment = excluded.comment, created_at = now()",
    )
    .bind(ctx.project_id)
    .bind(kind.as_str())
    .bind(no_nul(ctx.by))
    .bind(decision.as_str())
    .bind(comment)
    .bind(&ids)
    .bind(&versions)
    .execute(&mut *conn)
    .await?;
    let votes = head_votes(conn, kind, &heads).await?;
    let mut out = Vec::with_capacity(subjects.len());
    for s in subjects {
        let state = decide(
            votes.get(&s.id).unwrap_or(&HashMap::new()),
            policy.approvals,
        );
        let approved = if state == ReviewState::Approved {
            Some(s.version)
        } else {
            s.approved_version
        };
        out.push(Applied {
            id: s.id,
            number: s.number,
            version: s.version,
            state,
            approved,
            changed: Some(state) != s.review_state || approved != s.approved_version,
        });
    }
    let changed: Vec<&Applied> = out.iter().filter(|a| a.changed).collect();
    if !changed.is_empty() {
        let ids: Vec<Uuid> = changed.iter().map(|a| a.id).collect();
        let states: Vec<&str> = changed.iter().map(|a| a.state.as_str()).collect();
        let approved: Vec<Option<i32>> = changed.iter().map(|a| a.approved).collect();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "update {} t set review_state = u.st, approved_version = u.av, revision = t.revision + 1 \
             from unnest($2::uuid[], $3::text[], $4::int[]) as u(id, st, av) \
             where t.project_id = $1 and t.id = u.id",
            table(kind)
        )))
        .bind(ctx.project_id)
        .bind(&ids)
        .bind(&states)
        .bind(&approved)
        .execute(&mut *conn)
        .await?;
    }
    if kind == ReviewKind::Case {
        let moved: Vec<Uuid> = out
            .iter()
            .zip(subjects)
            .filter(|(a, s)| a.approved != s.approved_version)
            .map(|(a, _)| a.id)
            .collect();
        approved_version_moved(conn, ctx.project_id, &moved).await?;
    }
    let payloads: Vec<Value> = out
        .iter()
        .map(|a| {
            json!({"number": a.number, "version": a.version, "reviewState": a.state.as_str(),
                "approvedVersion": a.approved})
        })
        .collect();
    let ev = event_kind(kind);
    emit_many(&mut *conn, ev, Some(ctx.project_key), payloads.clone()).await?;
    if payloads.len() <= MAX_BULK_NOTIFY {
        for d in payloads {
            notify(
                &mut *conn,
                &LiveEvent {
                    project: ctx.project_key.to_string(),
                    kind: ev.to_string(),
                    data: d,
                },
            )
            .await?;
        }
    } else {
        notify(
            &mut *conn,
            &LiveEvent {
                project: ctx.project_key.to_string(),
                kind: ev.to_string(),
                data: json!({"count": payloads.len()}),
            },
        )
        .await?;
    }
    Ok(out)
}

/// Cases whose `approved_version` a vote moved, in the vote's transaction: shared step usages
/// follow the runnable version.
async fn approved_version_moved(
    conn: &mut PgConnection,
    project_id: Uuid,
    case_ids: &[Uuid],
) -> TmResult<()> {
    for id in case_ids {
        super::shared_steps::resync_case_usages(conn, project_id, *id).await?;
    }
    Ok(())
}

/// Query of `GET /reviews`.
#[derive(Clone, Debug, Default)]
pub struct ReviewQueueQuery {
    pub kind: Option<ReviewKind>,
    /// `pending` or `changes_requested`.
    pub state: Option<ReviewState>,
    /// Only objects the caller may approve and has not voted on at the head.
    pub reviewer_me: bool,
    /// Only objects whose head the caller made.
    pub author_me: bool,
    /// Cases of this folder and its subfolders (shared steps have no folder).
    pub folder: Option<Uuid>,
    pub before: Option<String>,
    pub limit: i64,
}

fn encode_cursor(t: DateTime<Utc>, id: Uuid) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(
        "{}|{id}",
        t.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
    ))
}

fn decode_cursor(c: &str) -> Option<(DateTime<Utc>, Uuid)> {
    if c.len() > 200 {
        return None;
    }
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c)
        .ok()?;
    let s = String::from_utf8(raw).ok()?;
    let (t, id) = s.split_once('|')?;
    Some((
        DateTime::parse_from_rfc3339(t).ok()?.with_timezone(&Utc),
        id.parse().ok()?,
    ))
}

/// A queue row before the approvals are counted.
struct QueueRow {
    item: ReviewQueueItem,
    id: Uuid,
}

fn queue_row(kind: ReviewKind, r: &PgRow) -> sqlx::Result<QueueRow> {
    let state: String = r.try_get("review_state")?;
    Ok(QueueRow {
        id: r.try_get("id")?,
        item: ReviewQueueItem {
            kind,
            number: r.try_get("number")?,
            name: r.try_get("name")?,
            head_version: r.try_get("version")?,
            approved_version: r.try_get("approved_version")?,
            review_state: ReviewState::parse(&state).unwrap_or(ReviewState::Pending),
            author: r.try_get("author")?,
            changed: r
                .try_get::<Option<Vec<String>>, _>("changed")?
                .unwrap_or_default(),
            approvals: 0,
            required: 0,
            folder_id: r.try_get("folder_id")?,
            updated_at: r.try_get("updated_at")?,
        },
    })
}

#[allow(clippy::too_many_arguments)]
fn push_queue_filters(
    qb: &mut QueryBuilder<Postgres>,
    kind: ReviewKind,
    alias: &str,
    q: &ReviewQueueQuery,
    policy: &CaseReviewSettings,
    me: &str,
    cursor: Option<(DateTime<Utc>, Uuid)>,
) {
    match q.state {
        Some(s) => {
            qb.push(format!(" and {alias}.review_state = "))
                .push_bind(s.as_str());
        }
        None => {
            qb.push(format!(
                " and {alias}.review_state in ('pending', 'changes_requested')"
            ));
        }
    }
    let me = me.to_lowercase();
    if q.author_me {
        qb.push(" and lower(v.created_by) = ").push_bind(me.clone());
    }
    if q.reviewer_me {
        qb.push(format!(
            " and not exists (select 1 from review_votes rv where rv.kind = '{}' \
             and rv.object_id = {alias}.id and rv.version = {alias}.version and lower(rv.reviewer) = ",
            kind.as_str()
        ))
        .push_bind(me.clone())
        .push(")");
        if !policy.allow_self_approval {
            let (table, col) = versions_table(kind);
            qb.push(format!(
                " and not exists (select 1 from {table} av where av.{col} = {alias}.id \
                 and av.version > least(coalesce({alias}.approved_version, 0), {alias}.version - 1) \
                 and av.version <= {alias}.version and lower(av.created_by) = "
            ))
            .push_bind(me.clone())
            .push(" and (av.version = ")
            .push(format!("{alias}.version or "))
            .push_bind(authored_keys(policy, kind))
            .push("::text[] is null or cardinality(av.changed) = 0 or av.changed && ")
            .push_bind(authored_keys(policy, kind))
            .push("::text[]))");
        }
        qb.push(format!(
            " and not ({alias}.origin = 'ai' and {alias}.ai_unreviewed and lower({alias}.created_by) is not distinct from "
        ))
        .push_bind(me)
        .push(")");
    }
    if let Some((t, id)) = cursor {
        qb.push(format!(" and ({alias}.updated_at, {alias}.id) < ("))
            .push_bind(t)
            .push(", ")
            .push_bind(id)
            .push(")");
    }
    qb.push(format!(
        " order by {alias}.updated_at desc, {alias}.id desc limit "
    ))
    .push_bind(q.limit + 1);
}

/// Review requests of a project since a time: what the `caseReviewRequested` notification
/// reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewRequests {
    /// Objects waiting for review now (`pending`).
    pub waiting: i64,
    /// Objects whose head became pending after `since`.
    pub fresh: i64,
    /// The newest of them, at most [`MAX_REQUEST_ITEMS`].
    pub items: Vec<ReviewRequestItem>,
    /// Authors of the fresh heads, at most [`MAX_REQUEST_AUTHORS`].
    pub authors: Vec<String>,
}

/// One object of a review request.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRequestItem {
    pub kind: ReviewKind,
    pub number: i64,
    /// `TC-<n>` or `SS-<n>`.
    pub key: String,
    pub name: String,
    pub author: Option<String>,
    pub requested_at: DateTime<Utc>,
}

/// A pending head: kind, number, name, author, when its version was written.
type RequestRow = (String, i64, String, Option<String>, DateTime<Utc>);

/// SQL of the pending heads of a project with their head version's author and time
/// (`$1` project, `$2` true when automated cases are in scope).
const PENDING_HEADS: &str = "select 'case' as kind, c.number, c.name, v.created_by as author, v.created_at as at \
       from cases c join case_versions v on v.case_id = c.id and v.version = c.version \
       where c.project_id = $1 and c.review_state = 'pending' and c.archived_at is null and ($2 or not c.automated) \
     union all \
     select 'sharedStep', s.number, s.name, v.created_by, v.created_at \
       from shared_steps s join shared_step_versions v on v.shared_step_id = s.id and v.version = s.version \
       where s.project_id = $1 and s.review_state = 'pending' and s.archived_at is null";

impl Store {
    /// The review of a case or shared step for `me`: head, approved version, votes (newest
    /// first), whether `me` may vote and why not (`permission`, `review_not_enabled`,
    /// `version_moved`, `not_pending`, `ai_self_review`, `self_approval`), and the workflow
    /// transitions of a case.
    pub async fn review_thread(
        &self,
        project_id: Uuid,
        kind: ReviewKind,
        n: i64,
        me: &str,
        can_review: bool,
    ) -> TmResult<ReviewThread> {
        Ok(self
            .review_thread_tagged(project_id, kind, n, me, can_review)
            .await?
            .0)
    }

    /// [`Store::review_thread`] with the revision of the case or shared step (its `ETag`,
    /// which a vote sends as `If-Match`).
    pub async fn review_thread_tagged(
        &self,
        project_id: Uuid,
        kind: ReviewKind,
        n: i64,
        me: &str,
        can_review: bool,
    ) -> TmResult<(ReviewThread, i64)> {
        let mut conn = self.pool.acquire().await?;
        let policy = review_settings(&mut conn, project_id).await?;
        let s = read_subject(&mut conn, project_id, kind, n).await?;
        let author = authors(&mut conn, kind, &[(s.id, s.version)])
            .await?
            .remove(&s.id);
        let changed_by = change_authors(&mut conn, &policy, kind, std::slice::from_ref(&s))
            .await?
            .remove(&s.id)
            .unwrap_or_default();
        let rows: Vec<(String, String, String, i32, DateTime<Utc>)> = sqlx::query_as(
            "select reviewer, decision, comment, version, created_at from review_votes \
             where kind = $1 and object_id = $2 order by created_at desc, reviewer limit $3",
        )
        .bind(kind.as_str())
        .bind(s.id)
        .bind(MAX_THREAD_VOTES)
        .fetch_all(&mut *conn)
        .await?;
        let votes: Vec<ReviewVote> = rows
            .into_iter()
            .filter_map(|(reviewer, d, comment, version, created_at)| {
                Some(ReviewVote {
                    reviewer,
                    decision: ReviewDecision::parse(&d)?,
                    comment,
                    version,
                    created_at,
                })
            })
            .collect();
        let approvals = head_votes(&mut conn, kind, &[(s.id, s.version)])
            .await?
            .remove(&s.id)
            .map(|v| {
                v.values()
                    .filter(|d| **d == ReviewDecision::Approve)
                    .count() as i64
            })
            .unwrap_or(0);
        let why_not = if can_review {
            refusal(&policy, &s, &changed_by, me, s.version).map(|r| r.code().to_string())
        } else {
            Some("permission".to_string())
        };
        let transitions = match &s.case {
            Some(c) => {
                super::workflow::allowed_transitions(&mut conn, project_id, c, me, can_review)
                    .await?
            }
            None => Vec::new(),
        };
        Ok((
            ReviewThread {
                kind,
                number: s.number,
                name: s.name,
                head_version: s.version,
                approved_version: s.approved_version,
                review_state: s.review_state,
                author,
                approvals,
                required: policy.approvals,
                votes,
                can_vote: why_not.is_none(),
                why_not,
                transitions,
            },
            s.revision,
        ))
    }

    /// A vote of `ctx.by` on the head of a case or shared step whose revision is `expected`
    /// (§7.3): `cases:review`, review on and the object in scope (409 `review_not_enabled`),
    /// the head version (409 `version_moved {currentVersion}`), a head waiting for review (409
    /// `not_pending`), not the creator of an unreviewed AI draft (403 `ai_self_review`), not
    /// the head's author unless `allowSelfApproval` (403 `self_approval`). A later vote of the
    /// same reviewer on the same version replaces the earlier one. Returns the review and the
    /// object's revision.
    pub async fn vote(
        &self,
        ctx: &CaseCtx<'_>,
        kind: ReviewKind,
        n: i64,
        expected: i64,
        w: &ReviewVoteWrite,
    ) -> TmResult<(ReviewThread, i64)> {
        if !ctx.can_review {
            return Err(forbidden_without_review());
        }
        let comment = check_comment(w.comment.as_deref())?;
        let mut tx = self.pool.begin().await?;
        let policy = review_settings(&mut tx, ctx.project_id).await?;
        let s = lock_subject(&mut tx, ctx.project_id, kind, n, Some(expected)).await?;
        let changed_by = change_authors(&mut tx, &policy, kind, std::slice::from_ref(&s))
            .await?
            .remove(&s.id)
            .unwrap_or_default();
        if let Some(r) = refusal(&policy, &s, &changed_by, ctx.by, w.version) {
            return Err(r.error());
        }
        apply_votes(
            &mut tx,
            ctx,
            &policy,
            kind,
            std::slice::from_ref(&s),
            w.decision,
            &comment,
        )
        .await?;
        tx.commit().await?;
        self.review_thread_tagged(ctx.project_id, kind, n, ctx.by, ctx.can_review)
            .await
    }

    /// One decision on many heads ("as one PR"): 1 to `maxBulkReview` items, each checked like
    /// a single vote; the ones that fail are skipped with the refusal's code (`version_moved`,
    /// `self_approval`, `ai_self_review`, `not_pending`, `review_not_enabled`, `not_found`).
    /// One transaction.
    pub async fn bulk_review(
        &self,
        ctx: &CaseCtx<'_>,
        b: &ReviewBulk,
    ) -> TmResult<ReviewBulkResult> {
        if !ctx.can_review {
            return Err(forbidden_without_review());
        }
        if b.items.is_empty() {
            return Err(TmError::bad_request("items: at least one"));
        }
        if b.items.len() > ctx.limits.max_bulk_review {
            return Err(TmError::limit(
                "maxBulkReview",
                ctx.limits.max_bulk_review as u64,
            ));
        }
        let comment = check_comment(b.comment.as_deref())?;
        let mut seen = BTreeSet::new();
        let items: Vec<_> = b
            .items
            .iter()
            .filter(|i| seen.insert((i.kind.as_str(), i.number)))
            .collect();
        let mut tx = self.pool.begin().await?;
        let policy = review_settings(&mut tx, ctx.project_id).await?;
        let mut out = ReviewBulkResult::default();
        let mut skips: HashMap<(ReviewKind, i64), &'static str> = HashMap::new();
        for kind in [ReviewKind::Case, ReviewKind::SharedStep] {
            let numbers: Vec<i64> = items
                .iter()
                .filter(|i| i.kind == kind)
                .map(|i| i.number)
                .collect();
            if numbers.is_empty() {
                continue;
            }
            let subjects: Vec<Subject> = match kind {
                ReviewKind::Case => {
                    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
                        "select {} from cases where project_id = $1 and number = any($2) order by id for update",
                        super::lookup::CASE_COLS
                    )))
                    .bind(ctx.project_id)
                    .bind(&numbers)
                    .fetch_all(&mut *tx)
                    .await?;
                    rows.iter()
                        .map(|r| super::lookup::case_row(r).map(|c| Subject::of_case(&c)))
                        .collect::<sqlx::Result<_>>()?
                }
                ReviewKind::SharedStep => {
                    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
                        "select {SHARED_STEP_COLS} from shared_steps where project_id = $1 and number = any($2) \
                         order by id for update"
                    )))
                    .bind(ctx.project_id)
                    .bind(&numbers)
                    .fetch_all(&mut *tx)
                    .await?;
                    rows.iter()
                        .map(|r| shared_step_row(r).map(|s| Subject::of_shared(&s)))
                        .collect::<sqlx::Result<_>>()?
                }
            };
            let by_number: HashMap<i64, &Subject> =
                subjects.iter().map(|s| (s.number, s)).collect();
            let author_of = change_authors(&mut tx, &policy, kind, &subjects).await?;
            let none = BTreeSet::new();
            let mut ok: Vec<Subject> = Vec::new();
            for i in items.iter().filter(|i| i.kind == kind) {
                let Some(s) = by_number.get(&i.number) else {
                    skips.insert((kind, i.number), "not_found");
                    continue;
                };
                match refusal(
                    &policy,
                    s,
                    author_of.get(&s.id).unwrap_or(&none),
                    ctx.by,
                    i.version,
                ) {
                    Some(r) => {
                        skips.insert((kind, i.number), r.code());
                    }
                    None => ok.push((*s).clone()),
                }
            }
            let applied =
                apply_votes(&mut tx, ctx, &policy, kind, &ok, b.decision, &comment).await?;
            out.applied += applied.len() as i64;
        }
        tx.commit().await?;
        out.skipped = items
            .iter()
            .filter_map(|i| {
                skips.get(&(i.kind, i.number)).map(|code| ReviewBulkSkip {
                    kind: i.kind,
                    number: i.number,
                    code: code.to_string(),
                })
            })
            .collect();
        Ok(out)
    }

    /// `GET /reviews`: cases and shared steps waiting for review (`pending` or
    /// `changes_requested`), most recently changed first; `reviewer_me` keeps those `me` may
    /// approve and has not voted on at the head (none without `cases:review`), `author_me`
    /// those whose head `me` made. Empty while review is off.
    pub async fn review_queue(
        &self,
        project_id: Uuid,
        q: &ReviewQueueQuery,
        me: &str,
        can_review: bool,
    ) -> TmResult<ReviewQueue> {
        if q.state == Some(ReviewState::Approved) {
            return Err(TmError::bad_request(
                "state must be pending or changes_requested",
            ));
        }
        let cursor = match &q.before {
            Some(c) => {
                Some(decode_cursor(c).ok_or_else(|| TmError::bad_request("invalid cursor"))?)
            }
            None => None,
        };
        let mut conn = self.pool.acquire().await?;
        if let Some(f) = q.folder
            && super::folders::folder(&mut conn, project_id, f)
                .await?
                .is_none()
        {
            return Err(TmError::NotFound("folder"));
        }
        let policy = review_settings(&mut conn, project_id).await?;
        if policy.mode == ReviewMode::Off || (q.reviewer_me && !can_review) {
            return Ok(ReviewQueue {
                items: Vec::new(),
                next: None,
            });
        }
        let mut rows: Vec<QueueRow> = Vec::new();
        if q.kind != Some(ReviewKind::SharedStep) {
            let mut qb = QueryBuilder::<Postgres>::new(
                "select c.id, c.number, c.name, c.version, c.approved_version, c.review_state, \
                 v.created_by as author, v.changed, c.folder_id, c.updated_at \
                 from cases c left join case_versions v on v.case_id = c.id and v.version = c.version \
                 where c.project_id = ",
            );
            qb.push_bind(project_id).push(" and c.archived_at is null");
            if policy.mode == ReviewMode::Manual {
                qb.push(" and not c.automated");
            }
            if let Some(f) = q.folder {
                qb.push(
                    " and c.folder_id in (select d.id from case_folders d where d.project_id = ",
                )
                .push_bind(project_id)
                .push(" and d.path_ids && ")
                .push_bind(vec![f])
                .push(")");
            }
            push_queue_filters(&mut qb, ReviewKind::Case, "c", q, &policy, me, cursor);
            for r in qb.build().fetch_all(&mut *conn).await? {
                rows.push(queue_row(ReviewKind::Case, &r)?);
            }
        }
        if q.kind != Some(ReviewKind::Case) && q.folder.is_none() {
            let mut qb = QueryBuilder::<Postgres>::new(
                "select s.id, s.number, s.name, s.version, s.approved_version, s.review_state, \
                 v.created_by as author, v.changed, null::uuid as folder_id, s.updated_at \
                 from shared_steps s left join shared_step_versions v \
                   on v.shared_step_id = s.id and v.version = s.version \
                 where s.project_id = ",
            );
            qb.push_bind(project_id).push(" and s.archived_at is null");
            push_queue_filters(&mut qb, ReviewKind::SharedStep, "s", q, &policy, me, cursor);
            for r in qb.build().fetch_all(&mut *conn).await? {
                rows.push(queue_row(ReviewKind::SharedStep, &r)?);
            }
        }
        rows.sort_by_key(|r| std::cmp::Reverse((r.item.updated_at, r.id)));
        let limit = q.limit.max(1) as usize;
        let next = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().map(|r| encode_cursor(r.item.updated_at, r.id))
        } else {
            None
        };
        for kind in [ReviewKind::Case, ReviewKind::SharedStep] {
            let heads: Vec<(Uuid, i32)> = rows
                .iter()
                .filter(|r| r.item.kind == kind)
                .map(|r| (r.id, r.item.head_version))
                .collect();
            let votes = head_votes(&mut conn, kind, &heads).await?;
            for r in rows.iter_mut().filter(|r| r.item.kind == kind) {
                r.item.approvals = votes
                    .get(&r.id)
                    .map(|v| {
                        v.values()
                            .filter(|d| **d == ReviewDecision::Approve)
                            .count() as i64
                    })
                    .unwrap_or(0);
                r.item.required = policy.approvals;
            }
        }
        Ok(ReviewQueue {
            items: rows.into_iter().map(|r| r.item).collect(),
            next,
        })
    }

    /// Review requests of a project since `since` (§7.3 `caseReviewRequested`): how many heads
    /// wait, how many became pending after `since`, the newest of those and their authors.
    pub async fn review_requests(
        &self,
        project_id: Uuid,
        since: DateTime<Utc>,
    ) -> TmResult<ReviewRequests> {
        let mut conn = self.pool.acquire().await?;
        let policy = review_settings(&mut conn, project_id).await?;
        if policy.mode == ReviewMode::Off {
            return Ok(ReviewRequests::default());
        }
        let all = policy.mode == ReviewMode::All;
        let (waiting, fresh): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select count(*), count(*) filter (where at > $3) from ({PENDING_HEADS}) p"
        )))
        .bind(project_id)
        .bind(all)
        .bind(since)
        .fetch_one(&mut *conn)
        .await?;
        if fresh == 0 {
            return Ok(ReviewRequests {
                waiting,
                ..Default::default()
            });
        }
        let rows: Vec<RequestRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select kind, number, name, author, at from ({PENDING_HEADS}) p where at > $3 \
                 order by at desc, kind, number limit $4"
        )))
        .bind(project_id)
        .bind(all)
        .bind(since)
        .bind(MAX_REQUEST_ITEMS)
        .fetch_all(&mut *conn)
        .await?;
        let authors: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "select distinct author from ({PENDING_HEADS}) p where at > $3 and author is not null limit $4"
        )))
        .bind(project_id)
        .bind(all)
        .bind(since)
        .bind(MAX_REQUEST_AUTHORS)
        .fetch_all(&mut *conn)
        .await?;
        Ok(ReviewRequests {
            waiting,
            fresh,
            items: rows
                .into_iter()
                .map(|(kind, number, name, author, at)| {
                    let kind = ReviewKind::parse(&kind).unwrap_or(ReviewKind::Case);
                    let prefix = match kind {
                        ReviewKind::Case => "TC",
                        ReviewKind::SharedStep => "SS",
                    };
                    ReviewRequestItem {
                        kind,
                        number,
                        key: format!("{prefix}-{number}"),
                        name,
                        author,
                        requested_at: at,
                    }
                })
                .collect(),
            authors,
        })
    }

    /// Projects (id, key) with a notification on `caseReviewRequested`.
    pub async fn review_notify_projects(&self) -> anyhow::Result<Vec<(Uuid, String)>> {
        Ok(sqlx::query_as(
            "select id, key from projects where deleted_at is null \
             and settings -> 'notifications' @> '[{\"on\": [\"caseReviewRequested\"]}]' \
             and coalesce(settings #>> '{caseReview,mode}', 'off') <> 'off' order by key limit 10000",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// When the last `caseReviewRequested` delivery of a notification was queued, within
    /// [`REQUEST_HORIZON`].
    pub async fn last_review_delivery(
        &self,
        project_id: Uuid,
        target_id: &str,
    ) -> anyhow::Result<Option<DateTime<Utc>>> {
        Ok(sqlx::query_scalar(
            "select max(created_at) from integration_deliveries where project_id = $1 \
             and id >= $3 and target_id = $2 and events @> array['caseReviewRequested']",
        )
        .bind(project_id)
        .bind(no_nul(target_id))
        .bind(v7_floor(
            Utc::now() - chrono::Duration::from_std(REQUEST_HORIZON).unwrap_or_default(),
        ))
        .fetch_one(&self.pool)
        .await?)
    }

    /// Queues a `caseReviewRequested` delivery unless the notification got one within
    /// `throttle` (checked and written under a per-notification lock, so replicas do not both
    /// send). Returns whether it was queued.
    pub async fn queue_review_delivery(
        &self,
        project_id: Uuid,
        channel: &str,
        target_id: &str,
        payload: Value,
        max_attempts: i32,
        throttle: std::time::Duration,
    ) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "select pg_advisory_xact_lock(hashtextextended('fastproof:review-notify:' || $1::text || ':' || $2, 0))",
        )
        .bind(project_id)
        .bind(no_nul(target_id))
        .execute(&mut *tx)
        .await?;
        let recent: bool = sqlx::query_scalar(
            "select exists (select 1 from integration_deliveries where project_id = $1 and id >= $4 \
             and target_id = $2 and events @> array['caseReviewRequested'] \
             and created_at > now() - make_interval(secs => $3))",
        )
        .bind(project_id)
        .bind(no_nul(target_id))
        .bind(throttle.as_secs_f64())
        .bind(v7_floor(
            Utc::now() - chrono::Duration::from_std(throttle * 2).unwrap_or_default(),
        ))
        .fetch_one(&mut *tx)
        .await?;
        if recent {
            return Ok(false);
        }
        sqlx::query(
            "insert into integration_deliveries (id, project_id, launch_id, channel, target_id, events, payload, max_attempts) \
             values ($1, $2, null, $3, $4, array['caseReviewRequested'], $5, $6)",
        )
        .bind(Uuid::now_v7())
        .bind(project_id)
        .bind(no_nul(channel))
        .bind(no_nul(target_id))
        .bind(crate::json_no_nul(payload))
        .bind(max_attempts.max(1))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_state_follows_the_contract() {
        use ReviewState::*;
        assert_eq!(next_state(None, 1, true, false), (Some(Pending), None));
        assert_eq!(next_state(None, 1, false, false), (None, None));
        assert_eq!(next_state(None, 1, true, true), (Some(Approved), Some(1)));
        assert_eq!(
            next_state(Some((None, None, 3)), 4, true, false),
            (Some(Pending), Some(3)),
            "never under review: the previous head stays runnable"
        );
        assert_eq!(
            next_state(Some((Some(Pending), Some(2), 5)), 6, true, false),
            (Some(Pending), Some(2))
        );
        assert_eq!(
            next_state(Some((Some(Approved), Some(5), 5)), 6, false, false),
            (Some(Approved), Some(6)),
            "the approval follows a change that needs none"
        );
        assert_eq!(
            next_state(Some((Some(ChangesRequested), Some(2), 5)), 6, false, false),
            (Some(Pending), Some(2)),
            "requested changes wait for a new version"
        );
        assert_eq!(
            next_state(Some((None, None, 5)), 6, false, false),
            (None, None)
        );
        assert_eq!(
            next_state(Some((Some(Pending), Some(2), 5)), 6, false, true),
            (Some(Pending), Some(2)),
            "an import does not approve changes that wait for review"
        );
        assert_eq!(
            next_state(Some((Some(ChangesRequested), Some(2), 5)), 6, true, true),
            (Some(Pending), Some(2))
        );
        assert_eq!(
            next_state(Some((Some(Approved), Some(5), 5)), 6, true, true),
            (Some(Approved), Some(6))
        );
        assert_eq!(
            next_state(Some((None, None, 5)), 6, true, true),
            (Some(Approved), Some(6))
        );
    }

    #[test]
    fn votes_decide_the_state() {
        let mut v = HashMap::new();
        assert_eq!(decide(&v, 1), ReviewState::Pending);
        v.insert("a".to_string(), ReviewDecision::Approve);
        assert_eq!(decide(&v, 1), ReviewState::Approved);
        assert_eq!(decide(&v, 2), ReviewState::Pending);
        v.insert("b".to_string(), ReviewDecision::RequestChanges);
        assert_eq!(decide(&v, 1), ReviewState::ChangesRequested);
        v.insert("b".to_string(), ReviewDecision::Approve);
        assert_eq!(decide(&v, 2), ReviewState::Approved, "a vote is replaced");
    }

    #[test]
    fn v7_floors_order_before_ids_of_their_time() {
        let t = Utc::now() - chrono::Duration::hours(25);
        assert!(v7_floor(t) <= Uuid::now_v7());
        assert!(v7_floor(t) > v7_floor(t - chrono::Duration::seconds(1)));
        assert_eq!(v7_floor(t).get_version_num(), 7);
    }

    #[test]
    fn cursors_round_trip_and_reject_garbage() {
        let t = Utc::now();
        let id = Uuid::now_v7();
        let (t2, id2) = decode_cursor(&encode_cursor(t, id)).unwrap();
        assert_eq!(id2, id);
        assert_eq!(t2.timestamp_micros(), t.timestamp_micros());
        assert!(decode_cursor("not-a-cursor").is_none());
        assert!(decode_cursor(&"A".repeat(300)).is_none());
    }

    #[test]
    fn comments_are_bounded() {
        assert!(check_comment(Some(&"x".repeat(MAX_COMMENT_BYTES))).is_ok());
        assert!(check_comment(Some(&"x".repeat(MAX_COMMENT_BYTES + 1))).is_err());
        assert!(check_comment(Some("line\nnext\tok")).is_ok());
        assert!(check_comment(Some("bell\u{7}")).is_err());
    }
}
