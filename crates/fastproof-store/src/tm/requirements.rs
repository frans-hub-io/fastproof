//! Requirements, their links to cases (manual, from code, AI proposals) and the tracker import
//! (`74-tm-next-interfaces.md` §7.6).
//!
//! Requirements form a tree per project: `path_ids` holds the ancestors and the requirement
//! itself, `depth` its length (a top-level requirement has depth 1, at most
//! `max_requirement_depth`). A requirement is `REQ-<n>`; it may carry the key of a tracker
//! issue (`issue_key`, unique per tracker). Links to cases are `active` or `proposed` (AI);
//! links from code follow the trusted sync and disappear when the code drops them, manual
//! links never do.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, Postgres, QueryBuilder, Row};
use uuid::Uuid;

use super::cases::{CaseCtx, MAX_Q_BYTES, case_event};
use super::lookup::{REQUIREMENT_COLS, requirement_by_number, requirement_row};
use super::query::QueryCtx;
use super::sync::CodeData;
use super::{
    CaseRequirement, CaseStatus, ImportReport, ImportRowError, LastResult, QueryEntity,
    Requirement, RequirementLink, RequirementLinkList, RequirementLinkOrigin,
    RequirementLinkStatus, RequirementLinksChange, RequirementList, RequirementOrigin,
    RequirementPatch, RequirementPathEntry, RequirementRow, RequirementStatus, RequirementSummary,
    RequirementTracker, RequirementWrite, ResultSource, TmError, TmJob, TmJobKind, TmJobProgress,
    TmJobStatus, TmResult, check_name, lock_project, violated_constraint,
};
use crate::analysis::settings::CaseSyncSettings;
use crate::launches::like_escape;
use crate::{Store, no_nul};

/// The label of a result that names requirements, besides the link types of
/// `caseSync.requirementLinkTypes`.
pub const REQUIREMENT_LABEL: &str = "requirement";
/// Bytes of a requirement type (`^[a-z][a-z0-9_-]{0,31}$`).
pub const MAX_TYPE_BYTES: usize = 32;
/// Bytes of an issue key.
pub const MAX_ISSUE_KEY_BYTES: usize = 64;
/// Bytes of a tracker id (`^[A-Za-z0-9_-]{1,32}$`).
pub const MAX_TRACKER_ID_BYTES: usize = 32;
/// Bytes of a requirement URL.
pub const MAX_URL_BYTES: usize = 2048;
/// Bytes of an external state.
pub const MAX_STATE_BYTES: usize = 100;
/// Requirement keys one result names, at most (the rest are ignored).
pub const MAX_CODE_KEYS_PER_CASE: usize = 50;
/// Requirements a case lists, at most.
pub const MAX_CASE_REQUIREMENTS: i64 = 1000;
/// Issues one tracker import reads, at most.
pub const MAX_IMPORT_ISSUES: usize = 1000;
/// Bytes of the text of all issues of one tracker import, at most.
pub const MAX_IMPORT_TEXT_BYTES: usize = 8 * 1024 * 1024;
/// Bytes of a tracker query.
pub const MAX_IMPORT_QUERY_BYTES: usize = 1000;
/// Days a dry run of a tracker import waits for its apply before it is cancelled.
pub const IMPORT_READY_DAYS: i64 = 7;
/// Dry runs of tracker imports a project keeps waiting for their apply; a new one cancels the
/// oldest beyond this (each holds up to [`MAX_IMPORT_TEXT_BYTES`] of issues).
pub const MAX_READY_IMPORTS: i64 = 3;
/// Links an AI agent may have proposed on one requirement at once (`status = proposed`); they
/// count toward `max_requirement_cases` too, so an agent cannot fill the requirement.
pub const MAX_PROPOSED_LINKS: i64 = 50;
/// Open link proposals (`status = proposed`) one AI principal may have in a project, over all
/// its requirements, at most: [`MAX_PROPOSED_LINKS`] alone would let one agent leave 50
/// proposals on every requirement of the project.
pub const MAX_AI_PROPOSED_LINKS: i64 = 1000;
/// Draft requirements of one AI creator per project (`origin = ai`, `status = draft`, not
/// archived), at most.
pub const MAX_AI_DRAFT_REQUIREMENTS: i64 = 200;
/// The default type of a requirement.
pub const DEFAULT_TYPE: &str = "requirement";

/// Filters of [`Store::list_requirements`].
#[derive(Clone, Debug, Default)]
pub struct RequirementQuery {
    /// `REQ-12`, `12`, or words of the title, description or issue key.
    pub q: Option<String>,
    /// Requirements anywhere under this one.
    pub parent: Option<i64>,
    pub status: Option<RequirementStatus>,
    pub req_type: Option<String>,
    /// FPQL over requirements.
    pub query: Option<String>,
    /// The caller (`me()` and personal saved queries).
    pub ctx: QueryCtx,
    /// `true` lists the archived ones.
    pub archived: bool,
    pub before: Option<String>,
    pub limit: i64,
}

/// What [`Store::change_requirement_links`] changed (case numbers).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinksChanged {
    pub added: Vec<i64>,
    pub removed: Vec<i64>,
    pub confirmed: Vec<i64>,
}

impl LinksChanged {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.confirmed.is_empty()
    }
}

/// An issue read from a tracker for the requirement import.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedRequirement {
    pub issue_key: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub external_state: Option<String>,
}

/// Parameters of a `requirements` job.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequirementImportParams {
    pub tracker_id: String,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_number: Option<i64>,
    /// `dryRun` (read the tracker) or `apply`.
    pub phase: String,
    /// The issues the dry run read (cleared after the apply).
    #[serde(default)]
    pub items: Vec<ImportedRequirement>,
}

/// The issue of a requirement as it is in the tracker now (state sync).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackedRequirement {
    pub id: Uuid,
    pub project_id: Uuid,
    pub project_key: String,
    pub tracker_id: String,
    pub issue_key: String,
}

// ---------------------------------------------------------------------------------------------
// Validation

/// A requirement write that passed [`check_write`] (the API's rules; imports use them too).
pub(crate) struct Checked {
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) req_type: String,
    pub(crate) status: RequirementStatus,
    pub(crate) tracker_id: Option<String>,
    pub(crate) issue_key: Option<String>,
    pub(crate) url: Option<String>,
}

fn check_type(t: &str) -> TmResult<String> {
    let t = t.trim();
    let ok = !t.is_empty()
        && t.len() <= MAX_TYPE_BYTES
        && t.as_bytes()[0].is_ascii_lowercase()
        && t.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if !ok {
        return Err(TmError::bad_request(
            "type must match ^[a-z][a-z0-9_-]{0,31}$ (for example epic, story, requirement, risk)",
        ));
    }
    Ok(t.to_string())
}

fn check_description(s: &str, max: usize) -> TmResult<String> {
    let s = no_nul(s);
    if s.len() > max {
        return Err(TmError::Invalid {
            code: "invalid_markdown",
            message: format!("description must have at most {max} bytes of Markdown"),
            details: Some(json!({"path": "description", "max": max})),
        });
    }
    Ok(s)
}

/// A tracker id: 1 to 32 letters, digits, `_` or `-`.
pub fn valid_tracker_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_TRACKER_ID_BYTES
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// An issue key: 1 to 64 bytes without spaces or control characters.
pub fn valid_issue_key(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ISSUE_KEY_BYTES
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// An `http`/`https` URL of at most 2048 bytes without spaces or control characters.
pub fn valid_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && s.len() > 8
        && s.len() <= MAX_URL_BYTES
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn opt_text(v: Option<&Option<String>>) -> Option<String> {
    v.and_then(|o| o.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub(crate) fn check_write(
    ctx: &CaseCtx<'_>,
    w: &RequirementWrite,
    origin: RequirementOrigin,
) -> TmResult<Checked> {
    let title = check_name("title", &w.title, ctx.limits.max_name_bytes)?;
    let description = check_description(
        w.description.as_deref().unwrap_or_default(),
        ctx.limits.max_markdown_bytes,
    )?;
    let req_type = check_type(w.req_type.as_deref().unwrap_or(DEFAULT_TYPE))?;
    let status = if origin == RequirementOrigin::Ai {
        RequirementStatus::Draft
    } else {
        w.status.unwrap_or(RequirementStatus::Active)
    };
    let tracker_id = opt_text(w.tracker_id.as_ref());
    if tracker_id.as_deref().is_some_and(|t| !valid_tracker_id(t)) {
        return Err(TmError::bad_request(
            "trackerId must have 1 to 32 letters, digits, _ or -",
        ));
    }
    let issue_key = opt_text(w.issue_key.as_ref());
    if issue_key.as_deref().is_some_and(|k| !valid_issue_key(k)) {
        return Err(TmError::bad_request(
            "issueKey must have 1 to 64 bytes without spaces",
        ));
    }
    if tracker_id.is_some() && issue_key.is_none() {
        return Err(TmError::bad_request("trackerId needs an issueKey"));
    }
    let url = opt_text(w.url.as_ref());
    if url.as_deref().is_some_and(|u| !valid_url(u)) {
        return Err(TmError::bad_request(
            "url must be an http or https URL of at most 2048 bytes",
        ));
    }
    Ok(Checked {
        title,
        description,
        req_type,
        status,
        tracker_id,
        issue_key,
        url,
    })
}

fn issue_taken(e: sqlx::Error) -> TmError {
    if violated_constraint(&e) == Some("requirements_issue") {
        TmError::conflict(
            "issue_key_taken",
            "another requirement of the project has this issue key",
        )
    } else {
        e.into()
    }
}

fn is_req_key(k: &str) -> bool {
    k.len() > 4 && k.is_char_boundary(4) && k[..4].eq_ignore_ascii_case("REQ-")
}

/// `REQ-12`, `req-12` or `12` as a number.
pub fn number_of(s: &str) -> Option<i64> {
    let t = s.trim();
    let digits = if is_req_key(t) { &t[4..] } else { t };
    if digits.is_empty() || digits.len() > 18 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|n| *n > 0)
}

fn number_cursor(before: Option<&str>) -> TmResult<Option<i64>> {
    match before {
        None => Ok(None),
        Some(b) => Ok(Some(
            b.parse::<i64>()
                .ok()
                .filter(|v| *v >= 0)
                .ok_or_else(|| TmError::bad_request("invalid cursor"))?,
        )),
    }
}

// ---------------------------------------------------------------------------------------------
// Rows and views

async fn event(conn: &mut PgConnection, project_key: &str, row: &RequirementRow) -> TmResult<()> {
    case_event(
        conn,
        project_key,
        "requirement.updated",
        json!({"number": row.number, "revision": row.revision}),
    )
    .await?;
    Ok(())
}

pub(crate) async fn lock_by_number(
    conn: &mut PgConnection,
    project_id: Uuid,
    n: i64,
    expected: Option<i64>,
) -> TmResult<RequirementRow> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {REQUIREMENT_COLS} from requirements where project_id = $1 and number = $2 for update"
    )))
    .bind(project_id)
    .bind(n)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(TmError::NotFound("requirement"))?;
    let row = requirement_row(&r)?;
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

async fn reread(conn: &mut PgConnection, project_id: Uuid, id: Uuid) -> TmResult<RequirementRow> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {REQUIREMENT_COLS} from requirements where project_id = $1 and id = $2"
    )))
    .bind(project_id)
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(requirement_row(&r)?)
}

/// The parent of a new or moved requirement: its path and depth (`None`: top level). An
/// archived parent is refused.
async fn parent_of(
    conn: &mut PgConnection,
    project_id: Uuid,
    n: Option<i64>,
) -> TmResult<Option<RequirementRow>> {
    let Some(n) = n else { return Ok(None) };
    let p = requirement_by_number(conn, project_id, n)
        .await?
        .ok_or(TmError::NotFound("parent requirement"))?;
    if p.archived_at.is_some() {
        return Err(TmError::conflict(
            "conflict",
            format!("REQ-{n} is archived; restore it first"),
        ));
    }
    Ok(Some(p))
}

fn view_of(
    row: &RequirementRow,
    path: Vec<RequirementPathEntry>,
    cases: i64,
    children: i64,
) -> Requirement {
    Requirement {
        id: row.id,
        number: row.number,
        title: row.title.clone(),
        description: row.description.clone(),
        description_html: String::new(),
        req_type: row.req_type.clone(),
        status: row.status,
        parent_id: row.parent_id,
        path,
        depth: row.depth.into(),
        tracker: row.issue_key.as_ref().map(|k| RequirementTracker {
            tracker_id: row.tracker_id.clone(),
            issue_key: k.clone(),
            url: row.url.clone(),
            external_state: row.external_state.clone(),
            synced_at: row.synced_at,
        }),
        content_changed_at: row.content_changed_at,
        origin: row.origin,
        cases,
        children,
        revision: row.revision,
        created_by: row.created_by.clone(),
        created_at: row.created_at,
        updated_by: row.updated_by.clone(),
        updated_at: row.updated_at,
        archived_at: row.archived_at,
    }
}

const SUMMARY_SELECT: &str = "select r.number, r.title, r.type, r.status, r.parent_id, r.depth, r.issue_key, \
    r.external_state, r.archived_at is not null as archived, r.updated_at, \
    exists (select 1 from requirements ch where ch.parent_id = r.id and ch.archived_at is null) as has_children, \
    (select count(*) from requirement_links l where l.requirement_id = r.id and l.status = 'active') as cases \
    from requirements r ";

fn summary_of(r: &sqlx::postgres::PgRow) -> sqlx::Result<RequirementSummary> {
    let status: String = r.try_get("status")?;
    Ok(RequirementSummary {
        number: r.try_get("number")?,
        title: r.try_get("title")?,
        req_type: r.try_get("type")?,
        status: RequirementStatus::parse(&status).unwrap_or(RequirementStatus::Active),
        parent_id: r.try_get("parent_id")?,
        depth: r.try_get::<i16, _>("depth")?.into(),
        has_children: r.try_get("has_children")?,
        issue_key: r.try_get("issue_key")?,
        external_state: r.try_get("external_state")?,
        cases: r.try_get("cases")?,
        archived: r.try_get("archived")?,
        updated_at: r.try_get("updated_at")?,
    })
}

fn page(mut items: Vec<RequirementSummary>, limit: i64) -> RequirementList {
    let more = items.len() as i64 > limit;
    items.truncate(limit.max(0) as usize);
    let next = more
        .then(|| items.last().map(|i| i.number.to_string()))
        .flatten();
    RequirementList { items, next }
}

/// The path entries (ancestors and the requirement), active links and children of a view.
async fn view_parts(
    conn: &mut PgConnection,
    row: &RequirementRow,
) -> TmResult<(Vec<RequirementPathEntry>, i64, i64)> {
    let path_rows: Vec<(Uuid, i64, String)> = sqlx::query_as(
        "select id, number, title from requirements where project_id = $1 and id = any($2)",
    )
    .bind(row.project_id)
    .bind(&row.path_ids)
    .fetch_all(&mut *conn)
    .await?;
    let by_id: HashMap<Uuid, (i64, String)> = path_rows
        .into_iter()
        .map(|(id, n, t)| (id, (n, t)))
        .collect();
    let path = row
        .path_ids
        .iter()
        .filter_map(|id| {
            by_id.get(id).map(|(n, t)| RequirementPathEntry {
                id: *id,
                number: *n,
                title: t.clone(),
            })
        })
        .collect();
    let (cases, children): (i64, i64) = sqlx::query_as(
        "select (select count(*) from requirement_links where requirement_id = $1 and status = 'active'), \
           (select count(*) from requirements where parent_id = $1 and archived_at is null)",
    )
    .bind(row.id)
    .fetch_one(&mut *conn)
    .await?;
    Ok((path, cases, children))
}

// ---------------------------------------------------------------------------------------------
// Writes

/// Creates a requirement under `parent_number` (`None`: top level) with the next number, or
/// with `number` (an import that keeps the numbers of its file; the sequence moves past it).
/// The caller holds the project's `requirements` lock.
pub(crate) async fn create_in(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    c: Checked,
    parent_number: Option<i64>,
    number: Option<i64>,
    origin: RequirementOrigin,
) -> TmResult<RequirementRow> {
    let parent = parent_of(conn, ctx.project_id, parent_number).await?;
    let depth = parent.as_ref().map(|p| p.depth as usize).unwrap_or(0) + 1;
    if depth > ctx.limits.max_requirement_depth {
        return Err(TmError::limit(
            "maxRequirementDepth",
            ctx.limits.max_requirement_depth as u64,
        ));
    }
    let seq: i64 = match number {
        None => {
            sqlx::query_scalar(
                "update projects set requirement_seq = requirement_seq + 1 where id = $1 returning requirement_seq",
            )
            .bind(ctx.project_id)
            .fetch_one(&mut *conn)
            .await?
        }
        Some(n) => {
            sqlx::query_scalar(
                "update projects set requirement_seq = greatest(requirement_seq, $2) where id = $1 \
                 returning requirement_seq",
            )
            .bind(ctx.project_id)
            .bind(n)
            .fetch_one(&mut *conn)
            .await?
        }
    };
    if seq as usize > ctx.limits.max_requirements {
        let n: i64 = sqlx::query_scalar("select count(*) from requirements where project_id = $1")
            .bind(ctx.project_id)
            .fetch_one(&mut *conn)
            .await?;
        if n as usize >= ctx.limits.max_requirements {
            return Err(TmError::limit(
                "maxRequirements",
                ctx.limits.max_requirements as u64,
            ));
        }
    }
    let id = Uuid::now_v7();
    let mut path_ids = parent
        .as_ref()
        .map(|p| p.path_ids.clone())
        .unwrap_or_default();
    path_ids.push(id);
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "insert into requirements (id, project_id, number, title, description, type, status, parent_id, path_ids, \
           depth, tracker_id, issue_key, url, origin, created_by, updated_by) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $15) returning {REQUIREMENT_COLS}"
    )))
    .bind(id)
    .bind(ctx.project_id)
    .bind(number.unwrap_or(seq))
    .bind(&c.title)
    .bind(&c.description)
    .bind(&c.req_type)
    .bind(c.status.as_str())
    .bind(parent.as_ref().map(|p| p.id))
    .bind(&path_ids)
    .bind(depth as i16)
    .bind(&c.tracker_id)
    .bind(&c.issue_key)
    .bind(&c.url)
    .bind(origin.as_str())
    .bind(no_nul(ctx.by))
    .fetch_one(&mut *conn)
    .await
    .map_err(issue_taken)?;
    let row = requirement_row(&r)?;
    event(conn, ctx.project_key, &row).await?;
    Ok(row)
}

/// Moves a locked requirement and its subtree under `parent` (`None`: top level). The caller
/// holds the project's `requirements` lock (taken before any row lock, as creates take it), so
/// two moves cannot make a cycle together.
async fn move_in(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    cur: &RequirementRow,
    parent_number: Option<i64>,
) -> TmResult<()> {
    let parent = parent_of(conn, ctx.project_id, parent_number).await?;
    if parent.as_ref().map(|p| p.id) == cur.parent_id {
        return Ok(());
    }
    if let Some(p) = &parent
        && p.path_ids.contains(&cur.id)
    {
        return Err(TmError::Conflict {
            code: "requirement_cycle",
            message: format!(
                "REQ-{} cannot move under REQ-{}, which is inside it",
                cur.number, p.number
            ),
            details: Some(json!({"number": cur.number, "parentNumber": p.number})),
        });
    }
    let deepest: i16 = sqlx::query_scalar(
        "select coalesce(max(depth), 0)::smallint from requirements where project_id = $1 and path_ids @> array[$2]::uuid[]",
    )
    .bind(ctx.project_id)
    .bind(cur.id)
    .fetch_one(&mut *conn)
    .await?;
    let new_depth = parent.as_ref().map(|p| p.depth as usize).unwrap_or(0) + 1;
    let below = (deepest.max(cur.depth) - cur.depth) as usize;
    if new_depth + below > ctx.limits.max_requirement_depth {
        return Err(TmError::limit(
            "maxRequirementDepth",
            ctx.limits.max_requirement_depth as u64,
        ));
    }
    let mut new_path = parent
        .as_ref()
        .map(|p| p.path_ids.clone())
        .unwrap_or_default();
    new_path.push(cur.id);
    sqlx::query(
        "update requirements set path_ids = $3::uuid[] || path_ids[$4:], depth = depth + $5, \
           updated_at = now() \
         where project_id = $1 and path_ids @> array[$2]::uuid[]",
    )
    .bind(ctx.project_id)
    .bind(cur.id)
    .bind(&new_path)
    .bind(cur.depth as i32 + 1)
    .bind(new_depth as i16 - cur.depth)
    .execute(&mut *conn)
    .await?;
    sqlx::query("update requirements set parent_id = $2 where id = $1")
        .bind(cur.id)
        .bind(parent.as_ref().map(|p| p.id))
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The fields of a write applied to a locked requirement.
pub(crate) struct Change {
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) req_type: Option<String>,
    pub(crate) status: Option<RequirementStatus>,
    /// `Some(None)`: top level.
    pub(crate) parent: Option<Option<i64>>,
    /// `Some(..)`: replace tracker id, issue key and URL.
    pub(crate) tracker: Option<(Option<String>, Option<String>, Option<String>)>,
}

pub(crate) async fn change_in(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    cur: &RequirementRow,
    ch: Change,
) -> TmResult<RequirementRow> {
    if cur.archived_at.is_some() {
        return Err(TmError::conflict(
            "conflict",
            format!("REQ-{} is archived; restore it first", cur.number),
        ));
    }
    if let Some(p) = ch.parent {
        let cur_parent = match cur.parent_id {
            Some(pid) => {
                sqlx::query_scalar::<_, i64>("select number from requirements where id = $1")
                    .bind(pid)
                    .fetch_optional(&mut *conn)
                    .await?
            }
            None => None,
        };
        if cur_parent != p {
            move_in(conn, ctx, cur, p).await?;
        }
    }
    let title = ch.title.unwrap_or_else(|| cur.title.clone());
    let description = ch.description.unwrap_or_else(|| cur.description.clone());
    let req_type = ch.req_type.unwrap_or_else(|| cur.req_type.clone());
    let status = ch.status.unwrap_or(cur.status);
    let (tracker_id, issue_key, url) = ch.tracker.unwrap_or_else(|| {
        (
            cur.tracker_id.clone(),
            cur.issue_key.clone(),
            cur.url.clone(),
        )
    });
    let content = title != cur.title || description != cur.description;
    let moved = reread(conn, ctx.project_id, cur.id).await?;
    let changed = content
        || req_type != cur.req_type
        || status != cur.status
        || tracker_id != cur.tracker_id
        || issue_key != cur.issue_key
        || url != cur.url
        || moved.parent_id != cur.parent_id;
    if !changed {
        return Ok(moved);
    }
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "update requirements set title = $3, description = $4, type = $5, status = $6, tracker_id = $7, \
           issue_key = $8, url = $9, \
           external_state = case when $8 is not distinct from issue_key and $7 is not distinct from tracker_id \
             then external_state end, \
           synced_at = case when $8 is not distinct from issue_key and $7 is not distinct from tracker_id \
             then synced_at end, \
           content_changed_at = case when $10 then now() else content_changed_at end, \
           revision = revision + 1, updated_by = $11, updated_at = now() \
         where project_id = $1 and id = $2 returning {REQUIREMENT_COLS}"
    )))
    .bind(ctx.project_id)
    .bind(cur.id)
    .bind(&title)
    .bind(&description)
    .bind(&req_type)
    .bind(status.as_str())
    .bind(&tracker_id)
    .bind(&issue_key)
    .bind(&url)
    .bind(content)
    .bind(no_nul(ctx.by))
    .fetch_one(&mut *conn)
    .await
    .map_err(issue_taken)?;
    let row = requirement_row(&r)?;
    event(conn, ctx.project_key, &row).await?;
    Ok(row)
}

async fn bump(conn: &mut PgConnection, ctx: &CaseCtx<'_>, id: Uuid) -> TmResult<RequirementRow> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "update requirements set revision = revision + 1, updated_by = $3, updated_at = now() \
         where project_id = $1 and id = $2 returning {REQUIREMENT_COLS}"
    )))
    .bind(ctx.project_id)
    .bind(id)
    .bind(no_nul(ctx.by))
    .fetch_one(&mut *conn)
    .await?;
    let row = requirement_row(&r)?;
    event(conn, ctx.project_key, &row).await?;
    Ok(row)
}

/// Case ids by number in the project; an unknown number is 404.
async fn case_ids(
    conn: &mut PgConnection,
    project_id: Uuid,
    numbers: &[i64],
) -> TmResult<HashMap<i64, Uuid>> {
    if numbers.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(i64, Uuid)> =
        sqlx::query_as("select number, id from cases where project_id = $1 and number = any($2)")
            .bind(project_id)
            .bind(numbers)
            .fetch_all(&mut *conn)
            .await?;
    let found: HashMap<i64, Uuid> = rows.into_iter().collect();
    if numbers.iter().any(|n| !found.contains_key(n)) {
        return Err(TmError::NotFound("case"));
    }
    Ok(found)
}

fn dedup(v: Option<&Vec<i64>>) -> Vec<i64> {
    let set: BTreeSet<i64> = v.into_iter().flatten().copied().collect();
    set.into_iter().collect()
}

// ---------------------------------------------------------------------------------------------
// Code links (trusted sync)

/// Requirement keys a result names: links of a type in `link_types` (case-insensitive; the
/// link name, else the last segment of its URL) and values of the `requirement` label.
pub fn code_keys(settings: &CaseSyncSettings, data: &CodeData) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |k: &str| {
        let k = k.trim();
        if valid_issue_key(k) && !out.iter().any(|o| o == k) && out.len() < MAX_CODE_KEYS_PER_CASE {
            out.push(k.to_string());
        }
    };
    for l in &data.raw_links {
        if !settings
            .requirement_link_types
            .iter()
            .any(|t| t.eq_ignore_ascii_case(l.link_type.trim()))
        {
            continue;
        }
        let name = l.name.trim();
        if !name.is_empty() {
            push(name);
        } else if let Some(seg) = l
            .url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .map(|s| s.split(['?', '#']).next().unwrap_or_default())
        {
            push(seg);
        }
    }
    for (n, v) in &data.labels {
        if n.eq_ignore_ascii_case(REQUIREMENT_LABEL) {
            for part in v.split(',') {
                push(part);
            }
        }
    }
    out
}

/// Links cases to requirements from the requirement links of trusted code; returns how many
/// links changed. A key is `REQ-<n>` or the issue key of an existing requirement that is not
/// archived (unknown keys create nothing). Links from code of these cases that the code no
/// longer names are removed; manual, imported and confirmed links stay. A proposed (AI) link
/// the code names becomes an active link from code. A requirement at
/// `max_requirement_cases` gets no new links.
pub async fn sync_code_links(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    settings: &CaseSyncSettings,
    items: &[(Uuid /*case*/, &CodeData)],
) -> TmResult<u64> {
    if items.is_empty() {
        return Ok(0);
    }
    let mut keys_of: BTreeMap<Uuid, Vec<String>> = BTreeMap::new();
    for (case_id, data) in items {
        let e = keys_of.entry(*case_id).or_default();
        for k in code_keys(settings, data) {
            if !e.contains(&k) && e.len() < MAX_CODE_KEYS_PER_CASE {
                e.push(k);
            }
        }
    }
    let case_list: Vec<Uuid> = keys_of.keys().copied().collect();
    let existing: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "select requirement_id, case_id from requirement_links where case_id = any($1) and origin = 'code'",
    )
    .bind(&case_list)
    .fetch_all(&mut *conn)
    .await?;
    let all_keys: BTreeSet<&String> = keys_of.values().flatten().collect();
    if all_keys.is_empty() && existing.is_empty() {
        return Ok(0);
    }
    let numbers: Vec<i64> = all_keys
        .iter()
        .filter(|k| is_req_key(k))
        .filter_map(|k| number_of(k))
        .collect();
    let issue_keys: Vec<String> = all_keys.iter().map(|k| k.to_string()).collect();
    let mut by_number: HashMap<i64, Uuid> = HashMap::new();
    let mut by_issue: HashMap<String, Uuid> = HashMap::new();
    if !all_keys.is_empty() {
        let rows: Vec<(Uuid, i64, Option<String>)> = sqlx::query_as(
            "select id, number, issue_key from requirements where project_id = $1 and archived_at is null \
             and (number = any($2) or issue_key = any($3))",
        )
        .bind(ctx.project_id)
        .bind(&numbers)
        .bind(&issue_keys)
        .fetch_all(&mut *conn)
        .await?;
        for (id, n, k) in rows {
            by_number.insert(n, id);
            if let Some(k) = k {
                by_issue.entry(k).or_insert(id);
            }
        }
    }
    let mut want: HashSet<(Uuid, Uuid)> = HashSet::new();
    for (case_id, keys) in &keys_of {
        for k in keys {
            let id = if is_req_key(k) {
                number_of(k)
                    .and_then(|n| by_number.get(&n))
                    .or_else(|| by_issue.get(k))
            } else {
                by_issue.get(k)
            };
            if let Some(id) = id {
                want.insert((*id, *case_id));
            }
        }
    }
    let have: HashSet<(Uuid, Uuid)> = existing.into_iter().collect();
    let gone: Vec<(Uuid, Uuid)> = have.difference(&want).copied().collect();
    let mut new: Vec<(Uuid, Uuid)> = want.difference(&have).copied().collect();
    new.sort();
    let touched_reqs: Vec<Uuid> = gone
        .iter()
        .chain(&new)
        .map(|(r, _)| *r)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if !touched_reqs.is_empty() {
        // The requirements first, in id order: a person changing the links of one holds its
        // row and then writes a link row, so writing link rows before waiting for the
        // requirement (the foreign key check) would deadlock with them.
        sqlx::query("select 1 from requirements where id = any($1) order by id for key share")
            .bind(&touched_reqs)
            .execute(&mut *conn)
            .await?;
    }
    let mut changed_reqs: BTreeSet<Uuid> = BTreeSet::new();
    let mut n: u64 = 0;
    if !gone.is_empty() {
        let (r, c): (Vec<Uuid>, Vec<Uuid>) = gone.iter().copied().unzip();
        n += sqlx::query(
            "delete from requirement_links l using unnest($1::uuid[], $2::uuid[]) as g(r, c) \
             where l.requirement_id = g.r and l.case_id = g.c and l.origin = 'code'",
        )
        .bind(&r)
        .bind(&c)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        changed_reqs.extend(r);
    }
    if !new.is_empty() {
        let reqs: Vec<Uuid> = new
            .iter()
            .map(|(r, _)| *r)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let counts: HashMap<Uuid, i64> = sqlx::query_as::<_, (Uuid, i64)>(
            "select requirement_id, count(*) from requirement_links where requirement_id = any($1) group by requirement_id",
        )
        .bind(&reqs)
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .collect();
        let mut room: HashMap<Uuid, i64> = reqs
            .iter()
            .map(|r| {
                (
                    *r,
                    ctx.limits.max_requirement_cases as i64 - counts.get(r).copied().unwrap_or(0),
                )
            })
            .collect();
        let mut r_ids = Vec::new();
        let mut c_ids = Vec::new();
        for (r, c) in new {
            let left = room.entry(r).or_insert(0);
            if *left > 0 {
                *left -= 1;
                r_ids.push(r);
                c_ids.push(c);
            }
        }
        if !r_ids.is_empty() {
            let touched: Vec<Uuid> = sqlx::query_scalar(
                "insert into requirement_links (requirement_id, case_id, project_id, origin, status, created_by) \
                 select g.r, g.c, $3, 'code', 'active', 'code' from unnest($1::uuid[], $2::uuid[]) as g(r, c) \
                 on conflict (requirement_id, case_id) do update set status = 'active', origin = 'code' \
                   where requirement_links.status = 'proposed' \
                 returning requirement_id",
            )
            .bind(&r_ids)
            .bind(&c_ids)
            .bind(ctx.project_id)
            .fetch_all(&mut *conn)
            .await?;
            n += touched.len() as u64;
            changed_reqs.extend(touched);
        }
    }
    if !changed_reqs.is_empty() {
        let ids: Vec<Uuid> = changed_reqs.into_iter().collect();
        let rows: Vec<(i64, i64)> = sqlx::query_as(
            "update requirements set revision = revision + 1, updated_at = now() \
             where project_id = $1 and id = any($2) returning number, revision",
        )
        .bind(ctx.project_id)
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?;
        for (number, revision) in rows {
            case_event(
                conn,
                ctx.project_key,
                "requirement.updated",
                json!({"number": number, "revision": revision}),
            )
            .await?;
        }
    }
    Ok(n)
}

// ---------------------------------------------------------------------------------------------
// Tracker import

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Text from a tracker as one line: control characters (line breaks included) become spaces.
fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Cleans issues read from a tracker: valid keys, titles cut to `max_name_bytes`, descriptions
/// to `max_markdown_bytes`, at most [`MAX_IMPORT_ISSUES`] issues and [`MAX_IMPORT_TEXT_BYTES`]
/// of text; duplicates dropped. Problems go to the report's errors.
pub fn clean_import(
    limits: &super::TmLimits,
    found: Vec<ImportedRequirement>,
    report: &mut ImportReport,
) -> Vec<ImportedRequirement> {
    let mut out: Vec<ImportedRequirement> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut bytes = 0usize;
    for (i, mut it) in found.into_iter().enumerate() {
        let row = i as i64 + 1;
        let mut err = |code: &str, message: String| {
            if report.errors.len() < 1000 {
                report.errors.push(ImportRowError {
                    row,
                    code: code.into(),
                    message,
                });
            }
        };
        if out.len() >= MAX_IMPORT_ISSUES {
            err(
                "limit_reached",
                format!("an import reads at most {MAX_IMPORT_ISSUES} issues"),
            );
            report.skip += 1;
            continue;
        }
        it.issue_key = it.issue_key.trim().to_string();
        if !valid_issue_key(&it.issue_key) || !seen.insert(it.issue_key.clone()) {
            report.skip += 1;
            continue;
        }
        it.title = clip(one_line(&it.title).trim(), limits.max_name_bytes);
        if it.title.is_empty() {
            it.title = it.issue_key.clone();
        }
        it.description = clip(&no_nul(&it.description), limits.max_markdown_bytes);
        it.url = it.url.filter(|u| valid_url(u));
        it.external_state = it
            .external_state
            .map(|s| clip(one_line(&s).trim(), MAX_STATE_BYTES))
            .filter(|s| !s.is_empty());
        bytes += it.title.len() + it.description.len();
        if bytes > MAX_IMPORT_TEXT_BYTES {
            err(
                "limit_reached",
                format!("an import reads at most {MAX_IMPORT_TEXT_BYTES} bytes of text"),
            );
            report.skip += 1;
            continue;
        }
        out.push(it);
    }
    out
}

async fn existing_by_issue(
    conn: &mut PgConnection,
    project_id: Uuid,
    tracker_id: &str,
    keys: &[String],
) -> sqlx::Result<HashMap<String, RequirementRow>> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {REQUIREMENT_COLS} from requirements where project_id = $1 and tracker_id = $2 and issue_key = any($3)"
    )))
    .bind(project_id)
    .bind(tracker_id)
    .bind(keys)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = HashMap::new();
    for r in &rows {
        let row = requirement_row(r)?;
        if let Some(k) = row.issue_key.clone() {
            out.insert(k, row);
        }
    }
    Ok(out)
}

fn import_differs(cur: &RequirementRow, it: &ImportedRequirement) -> bool {
    cur.title != it.title
        || cur.description != it.description
        || (it.url.is_some() && cur.url != it.url)
        || cur.external_state != it.external_state
}

// ---------------------------------------------------------------------------------------------
// Store API

impl Store {
    /// A page of requirements by number.
    pub async fn list_requirements(
        &self,
        project_id: Uuid,
        q: &RequirementQuery,
    ) -> TmResult<RequirementList> {
        let after = number_cursor(q.before.as_deref())?;
        if q.q.as_ref().is_some_and(|t| t.len() > MAX_Q_BYTES) {
            return Err(TmError::bad_request(format!(
                "q has at most {MAX_Q_BYTES} bytes"
            )));
        }
        let req_type = match q
            .req_type
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            Some(t) => Some(check_type(t)?),
            None => None,
        };
        let mut tx = self.pool.begin().await?;
        let parent_id = match q.parent {
            Some(n) => Some(
                requirement_by_number(&mut tx, project_id, n)
                    .await?
                    .ok_or(TmError::NotFound("parent requirement"))?
                    .id,
            ),
            None => None,
        };
        let src = q.query.as_deref().filter(|s| !s.trim().is_empty());
        let text =
            q.q.as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string);
        if src.is_some() || text.is_some() {
            super::query::set_timeout(&mut tx).await?;
        }
        let fpql = match src {
            Some(src) => Some(
                super::query::prepare(&mut tx, project_id, QueryEntity::Requirements, src, &q.ctx)
                    .await?,
            ),
            None => None,
        };
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(SUMMARY_SELECT);
        qb.push("where r.project_id = ").push_bind(project_id);
        qb.push(if q.archived {
            " and r.archived_at is not null"
        } else {
            " and r.archived_at is null"
        });
        if let Some(pid) = parent_id {
            qb.push(" and r.id <> ")
                .push_bind(pid)
                .push(" and r.path_ids @> array[")
                .push_bind(pid)
                .push("]::uuid[]");
        }
        if let Some(s) = q.status {
            qb.push(" and r.status = ").push_bind(s.as_str());
        }
        if let Some(t) = req_type {
            qb.push(" and r.type = ").push_bind(t);
        }
        if let Some(t) = &text {
            let like = format!("%{}%", like_escape(t));
            qb.push(" and (r.title ilike ")
                .push_bind(like.clone())
                .push(" escape '\\' or r.issue_key ilike ")
                .push_bind(like.clone())
                .push(" escape '\\' or r.description ilike ")
                .push_bind(like)
                .push(" escape '\\'");
            if let Some(n) = number_of(t) {
                qb.push(" or r.number = ").push_bind(n);
            }
            qb.push(")");
        }
        if let Some(f) = &fpql {
            super::query::compile::push_query(&mut qb, project_id, f);
        }
        if let Some(a) = after {
            qb.push(" and r.number > ").push_bind(a);
        }
        qb.push(" order by r.number limit ").push_bind(q.limit + 1);
        let rows = qb
            .build()
            .fetch_all(&mut *tx)
            .await
            .map_err(super::query::run_error(src.unwrap_or("")))?;
        let items = rows.iter().map(summary_of).collect::<sqlx::Result<_>>()?;
        tx.commit().await?;
        Ok(page(items, q.limit))
    }

    /// The direct children of `parent` (top-level requirements when `None`), not archived.
    pub async fn requirement_tree(
        &self,
        project_id: Uuid,
        parent: Option<i64>,
        before: Option<&str>,
        limit: i64,
    ) -> TmResult<RequirementList> {
        let after = number_cursor(before)?;
        let mut conn = self.pool.acquire().await?;
        let parent_id = match parent {
            Some(n) => Some(
                requirement_by_number(&mut conn, project_id, n)
                    .await?
                    .ok_or(TmError::NotFound("requirement"))?
                    .id,
            ),
            None => None,
        };
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{SUMMARY_SELECT} where r.project_id = $1 and r.archived_at is null \
               and r.parent_id is not distinct from $2 and ($3::bigint is null or r.number > $3) \
             order by r.number limit $4"
        )))
        .bind(project_id)
        .bind(parent_id)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(&mut *conn)
        .await?;
        let items = rows.iter().map(summary_of).collect::<sqlx::Result<_>>()?;
        Ok(page(items, limit))
    }

    /// The requirement with number `n` (archived ones included).
    pub async fn requirement_row(&self, project_id: Uuid, n: i64) -> TmResult<RequirementRow> {
        let mut conn = self.pool.acquire().await?;
        requirement_by_number(&mut conn, project_id, n)
            .await?
            .ok_or(TmError::NotFound("requirement"))
    }

    /// The API view of the requirement with number `n` (HTML empty: the server renders it).
    pub async fn requirement_view(&self, project_id: Uuid, n: i64) -> TmResult<Requirement> {
        let row = self.requirement_row(project_id, n).await?;
        self.requirement_view_of(&row).await
    }

    /// The API view of a requirement row.
    pub async fn requirement_view_of(&self, row: &RequirementRow) -> TmResult<Requirement> {
        let mut conn = self.pool.acquire().await?;
        let (path, cases, children) = view_parts(&mut conn, row).await?;
        Ok(view_of(row, path, cases, children))
    }

    /// Creates a requirement (`REQ-<n>`, `requirement.updated`). An AI draft is always `draft`.
    pub async fn create_requirement(
        &self,
        ctx: &CaseCtx<'_>,
        w: &RequirementWrite,
        origin: RequirementOrigin,
    ) -> TmResult<RequirementRow> {
        let c = check_write(ctx, w, origin)?;
        let parent = w.parent_number.flatten();
        let mut tx = self.pool.begin().await?;
        lock_project(&mut tx, ctx.project_id, "requirements").await?;
        if origin == RequirementOrigin::Ai {
            let drafts: i64 = sqlx::query_scalar(
                "select count(*) from requirements where project_id = $1 and origin = 'ai' \
                 and status = 'draft' and archived_at is null and created_by = $2",
            )
            .bind(ctx.project_id)
            .bind(no_nul(ctx.by))
            .fetch_one(&mut *tx)
            .await?;
            if drafts >= MAX_AI_DRAFT_REQUIREMENTS {
                return Err(TmError::limit(
                    "maxAiDraftRequirements",
                    MAX_AI_DRAFT_REQUIREMENTS as u64,
                ));
            }
        }
        let row = create_in(&mut tx, ctx, c, parent, None, origin).await?;
        tx.commit().await?;
        Ok(row)
    }

    /// Replaces a requirement when its revision is `expected` (fields left out take their
    /// defaults: top level, no tracker issue); returns the row before and after.
    pub async fn update_requirement(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        expected: i64,
        w: &RequirementWrite,
    ) -> TmResult<(RequirementRow, RequirementRow)> {
        let c = check_write(ctx, w, RequirementOrigin::Ui)?;
        let mut tx = self.pool.begin().await?;
        lock_project(&mut tx, ctx.project_id, "requirements").await?;
        let cur = lock_by_number(&mut tx, ctx.project_id, n, Some(expected)).await?;
        let row = change_in(
            &mut tx,
            ctx,
            &cur,
            Change {
                title: Some(c.title),
                description: Some(c.description),
                req_type: Some(c.req_type),
                status: Some(c.status),
                parent: Some(w.parent_number.flatten()),
                tracker: Some((c.tracker_id, c.issue_key, c.url)),
            },
        )
        .await?;
        tx.commit().await?;
        Ok((cur, row))
    }

    /// Changes the given fields of a requirement when its revision is `expected`.
    pub async fn patch_requirement(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        expected: i64,
        p: &RequirementPatch,
    ) -> TmResult<(RequirementRow, RequirementRow)> {
        let title = match &p.title {
            Some(t) => Some(check_name("title", t, ctx.limits.max_name_bytes)?),
            None => None,
        };
        let description = match &p.description {
            Some(d) => Some(check_description(d, ctx.limits.max_markdown_bytes)?),
            None => None,
        };
        let req_type = match &p.req_type {
            Some(t) => Some(check_type(t)?),
            None => None,
        };
        let mut tx = self.pool.begin().await?;
        if p.parent_number.is_some() {
            lock_project(&mut tx, ctx.project_id, "requirements").await?;
        }
        let cur = lock_by_number(&mut tx, ctx.project_id, n, Some(expected)).await?;
        let row = change_in(
            &mut tx,
            ctx,
            &cur,
            Change {
                title,
                description,
                req_type,
                status: p.status,
                parent: p.parent_number,
                tracker: None,
            },
        )
        .await?;
        tx.commit().await?;
        Ok((cur, row))
    }

    /// Archives a requirement with everything under it, or restores what was archived with
    /// it (refused while its parent is archived). Returns the row and the number of
    /// requirements changed.
    pub async fn set_requirement_archived(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        archive: bool,
    ) -> TmResult<(RequirementRow, u64)> {
        let mut tx = self.pool.begin().await?;
        lock_project(&mut tx, ctx.project_id, "requirements").await?;
        let cur = lock_by_number(&mut tx, ctx.project_id, n, None).await?;
        if cur.archived_at.is_some() == archive {
            return Ok((cur, 0));
        }
        let changed = if archive {
            sqlx::query(
                "update requirements set archived_at = now(), revision = revision + 1, updated_by = $3, \
                   updated_at = now() \
                 where project_id = $1 and path_ids @> array[$2]::uuid[] and archived_at is null",
            )
            .bind(ctx.project_id)
            .bind(cur.id)
            .bind(no_nul(ctx.by))
            .execute(&mut *tx)
            .await?
            .rows_affected()
        } else {
            if let Some(pid) = cur.parent_id {
                let parent_archived: Option<(i64, bool)> = sqlx::query_as(
                    "select number, archived_at is not null from requirements where id = $1",
                )
                .bind(pid)
                .fetch_optional(&mut *tx)
                .await?;
                if let Some((pn, true)) = parent_archived {
                    return Err(TmError::conflict(
                        "conflict",
                        format!("REQ-{pn} is archived; restore it first"),
                    ));
                }
            }
            sqlx::query(
                "update requirements set archived_at = null, revision = revision + 1, updated_by = $3, \
                   updated_at = now() \
                 where project_id = $1 and path_ids @> array[$2]::uuid[] and archived_at = $4",
            )
            .bind(ctx.project_id)
            .bind(cur.id)
            .bind(no_nul(ctx.by))
            .bind(cur.archived_at)
            .execute(&mut *tx)
            .await?
            .rows_affected()
        };
        let row = reread(&mut tx, ctx.project_id, cur.id).await?;
        event(&mut tx, ctx.project_key, &row).await?;
        tx.commit().await?;
        Ok((row, changed))
    }

    /// Adds, removes and confirms links of a requirement to cases (by case number). `origin`
    /// `ai` only proposes: its links are `proposed` and it may neither remove nor confirm.
    /// Adding a proposed link confirms it; adding a link that came from code makes it a manual
    /// one (it stays when the code drops it). At most `max_requirement_cases` links per
    /// requirement (409 `limit_reached`); an unknown case is 404.
    pub async fn change_requirement_links(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        change: &RequirementLinksChange,
        origin: RequirementLinkOrigin,
    ) -> TmResult<(RequirementRow, LinksChanged)> {
        let add = dedup(change.add.as_ref());
        let remove = dedup(change.remove.as_ref());
        let confirm = dedup(change.confirm.as_ref());
        let max = ctx.limits.max_requirement_cases;
        if add.len() + remove.len() + confirm.len() > max {
            return Err(TmError::bad_request(format!(
                "a change names at most {max} cases"
            )));
        }
        if add.is_empty() && remove.is_empty() && confirm.is_empty() {
            return Err(TmError::bad_request("name cases to add, remove or confirm"));
        }
        if origin == RequirementLinkOrigin::Ai && (!remove.is_empty() || !confirm.is_empty()) {
            return Err(TmError::bad_request(
                "a proposal only adds links; a person confirms or removes them",
            ));
        }
        let mut tx = self.pool.begin().await?;
        let cur = lock_by_number(&mut tx, ctx.project_id, n, None).await?;
        if cur.archived_at.is_some() {
            return Err(TmError::conflict(
                "conflict",
                format!("REQ-{} is archived; restore it first", cur.number),
            ));
        }
        if origin == RequirementLinkOrigin::Ai {
            // Serializes the proposals of one principal across requirements, so the count below
            // cannot be passed by parallel calls on different requirements. Only this path
            // takes the lock, always after the requirement row, so no cycle can form.
            lock_project(
                &mut tx,
                ctx.project_id,
                &format!("ai-links:{}", no_nul(ctx.by)),
            )
            .await?;
        }
        let all: Vec<i64> = add
            .iter()
            .chain(&remove)
            .chain(&confirm)
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let ids = case_ids(&mut tx, ctx.project_id, &all).await?;
        let by_id: HashMap<Uuid, i64> = ids.iter().map(|(n, id)| (*id, *n)).collect();
        let pick =
            |v: &[i64]| -> Vec<Uuid> { v.iter().filter_map(|n| ids.get(n).copied()).collect() };
        let mut out = LinksChanged::default();
        if !remove.is_empty() {
            let gone: Vec<Uuid> = sqlx::query_scalar(
                "delete from requirement_links where requirement_id = $1 and case_id = any($2) returning case_id",
            )
            .bind(cur.id)
            .bind(pick(&remove))
            .fetch_all(&mut *tx)
            .await?;
            out.removed = gone.iter().filter_map(|c| by_id.get(c).copied()).collect();
        }
        if !confirm.is_empty() {
            let done: Vec<Uuid> = sqlx::query_scalar(
                "update requirement_links set status = 'active' where requirement_id = $1 and case_id = any($2) \
                 and status = 'proposed' returning case_id",
            )
            .bind(cur.id)
            .bind(pick(&confirm))
            .fetch_all(&mut *tx)
            .await?;
            out.confirmed = done.iter().filter_map(|c| by_id.get(c).copied()).collect();
        }
        if !add.is_empty() {
            let status = if origin == RequirementLinkOrigin::Ai {
                "proposed"
            } else {
                "active"
            };
            let targets = pick(&add);
            let before: HashMap<Uuid, String> = sqlx::query_as::<_, (Uuid, String)>(
                "select case_id, status from requirement_links where requirement_id = $1 and case_id = any($2)",
            )
            .bind(cur.id)
            .bind(&targets)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect();
            let touched: Vec<Uuid> = sqlx::query_scalar(
                "insert into requirement_links (requirement_id, case_id, project_id, origin, status, created_by) \
                 select $1, c, $2, $3, $4, $5 from unnest($6::uuid[]) as c \
                 on conflict (requirement_id, case_id) do update set status = 'active', \
                   origin = case when requirement_links.origin = 'code' then excluded.origin \
                     else requirement_links.origin end \
                   where excluded.status = 'active' \
                     and (requirement_links.status = 'proposed' or requirement_links.origin = 'code') \
                 returning case_id",
            )
            .bind(cur.id)
            .bind(ctx.project_id)
            .bind(origin.as_str())
            .bind(status)
            .bind(no_nul(ctx.by))
            .bind(&targets)
            .fetch_all(&mut *tx)
            .await?;
            let proposed_new = origin == RequirementLinkOrigin::Ai && !touched.is_empty();
            for c in touched {
                if let Some(n) = by_id.get(&c) {
                    match before.get(&c).map(String::as_str) {
                        Some("proposed") => out.confirmed.push(*n),
                        _ => out.added.push(*n),
                    }
                }
            }
            let total: i64 = sqlx::query_scalar(
                "select count(*) from requirement_links where requirement_id = $1",
            )
            .bind(cur.id)
            .fetch_one(&mut *tx)
            .await?;
            if total as usize > max {
                return Err(TmError::limit("maxRequirementCases", max as u64));
            }
            if proposed_new {
                let proposed: i64 = sqlx::query_scalar(
                    "select count(*) from requirement_links where requirement_id = $1 and status = 'proposed'",
                )
                .bind(cur.id)
                .fetch_one(&mut *tx)
                .await?;
                if proposed > MAX_PROPOSED_LINKS {
                    return Err(TmError::limit(
                        "maxProposedLinks",
                        MAX_PROPOSED_LINKS as u64,
                    ));
                }
                let mine: i64 = sqlx::query_scalar(
                    "select count(*) from requirement_links where project_id = $1 and status = 'proposed' \
                     and created_by = $2",
                )
                .bind(ctx.project_id)
                .bind(no_nul(ctx.by))
                .fetch_one(&mut *tx)
                .await?;
                if mine > MAX_AI_PROPOSED_LINKS {
                    return Err(TmError::limit(
                        "maxAiProposedLinks",
                        MAX_AI_PROPOSED_LINKS as u64,
                    ));
                }
            }
        }
        out.added.sort();
        out.removed.sort();
        out.confirmed.sort();
        out.confirmed.dedup();
        if out.is_empty() {
            return Ok((cur, out));
        }
        let row = bump(&mut tx, ctx, cur.id).await?;
        tx.commit().await?;
        Ok((row, out))
    }

    /// A page of the cases linked to a requirement, by case number.
    pub async fn requirement_cases(
        &self,
        project_id: Uuid,
        n: i64,
        link_status: Option<RequirementLinkStatus>,
        before: Option<&str>,
        limit: i64,
    ) -> TmResult<RequirementLinkList> {
        let after = number_cursor(before)?;
        let mut conn = self.pool.acquire().await?;
        let req = requirement_by_number(&mut conn, project_id, n)
            .await?
            .ok_or(TmError::NotFound("requirement"))?;
        let rows = sqlx::query(
            "select c.number, c.name, c.status, c.automated, l.origin, l.status as link_status, l.created_by, \
               l.created_at, c.last_status, c.last_status_at, c.last_source, c.last_launch_id, c.last_result_id \
             from requirement_links l join cases c on c.id = l.case_id \
             where l.requirement_id = $1 and c.project_id = $2 and ($3::text is null or l.status = $3) \
               and ($4::bigint is null or c.number > $4) \
             order by c.number limit $5",
        )
        .bind(req.id)
        .bind(project_id)
        .bind(link_status.map(|s| s.as_str()))
        .bind(after)
        .bind(limit + 1)
        .fetch_all(&mut *conn)
        .await?;
        let mut items = Vec::with_capacity(rows.len());
        for r in &rows {
            let status: String = r.try_get("status")?;
            let origin: String = r.try_get("origin")?;
            let ls: String = r.try_get("link_status")?;
            let last = match (
                r.try_get::<Option<String>, _>("last_status")?,
                r.try_get::<Option<DateTime<Utc>>, _>("last_status_at")?,
                r.try_get::<Option<Uuid>, _>("last_launch_id")?,
                r.try_get::<Option<Uuid>, _>("last_result_id")?,
            ) {
                (Some(status), Some(at), Some(launch_id), Some(result_id)) => Some(LastResult {
                    status,
                    at,
                    source: r
                        .try_get::<Option<String>, _>("last_source")?
                        .and_then(|s| ResultSource::parse(&s))
                        .unwrap_or_default(),
                    launch_id,
                    result_id,
                }),
                _ => None,
            };
            items.push(RequirementLink {
                case_number: r.try_get("number")?,
                name: r.try_get("name")?,
                status: CaseStatus::parse(&status).unwrap_or_default(),
                automated: r.try_get("automated")?,
                origin: RequirementLinkOrigin::parse(&origin).unwrap_or(RequirementLinkOrigin::Ui),
                link_status: RequirementLinkStatus::parse(&ls)
                    .unwrap_or(RequirementLinkStatus::Active),
                created_by: r.try_get("created_by")?,
                created_at: r.try_get("created_at")?,
                last_result: last,
            });
        }
        let more = items.len() as i64 > limit;
        items.truncate(limit.max(0) as usize);
        let next = more
            .then(|| items.last().map(|i| i.case_number.to_string()))
            .flatten();
        Ok(RequirementLinkList { items, next })
    }

    /// The requirements linked to a case (at most [`MAX_CASE_REQUIREMENTS`], by number).
    pub async fn case_requirements(
        &self,
        project_id: Uuid,
        case_number: i64,
    ) -> TmResult<Vec<CaseRequirement>> {
        let mut conn = self.pool.acquire().await?;
        let case_id: Uuid =
            sqlx::query_scalar("select id from cases where project_id = $1 and number = $2")
                .bind(project_id)
                .bind(case_number)
                .fetch_optional(&mut *conn)
                .await?
                .ok_or(TmError::NotFound("case"))?;
        let rows: Vec<(i64, String, String, String, String, String)> = sqlx::query_as(
            "select r.number, r.title, r.type, r.status, l.origin, l.status from requirement_links l \
             join requirements r on r.id = l.requirement_id \
             where l.case_id = $1 and r.project_id = $2 and r.archived_at is null \
             order by r.number limit $3",
        )
        .bind(case_id)
        .bind(project_id)
        .bind(MAX_CASE_REQUIREMENTS)
        .fetch_all(&mut *conn)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(number, title, req_type, status, origin, ls)| CaseRequirement {
                    number,
                    title,
                    req_type,
                    status: RequirementStatus::parse(&status).unwrap_or(RequirementStatus::Active),
                    origin: RequirementLinkOrigin::parse(&origin)
                        .unwrap_or(RequirementLinkOrigin::Ui),
                    link_status: RequirementLinkStatus::parse(&ls)
                        .unwrap_or(RequirementLinkStatus::Active),
                },
            )
            .collect())
    }

    // -----------------------------------------------------------------------------------------
    // Tracker import jobs

    /// Queues the dry run of a tracker import (`requirements` job). Refused with 409
    /// `import_busy` while another one of the project is queued or running; dry runs nobody
    /// applied within [`IMPORT_READY_DAYS`], and the oldest beyond [`MAX_READY_IMPORTS`] with
    /// the new one, are cancelled and their issues dropped.
    pub async fn start_requirement_import(
        &self,
        ctx: &CaseCtx<'_>,
        owner: &str,
        params: &RequirementImportParams,
    ) -> TmResult<TmJob> {
        let mut tx = self.pool.begin().await?;
        lock_project(&mut tx, ctx.project_id, "imports").await?;
        if let Some(n) = params.parent_number {
            parent_of(&mut tx, ctx.project_id, Some(n)).await?;
        }
        sqlx::query(
            "update tm_jobs set status = 'cancelled', finished_at = now(), params = params || '{\"items\": []}'::jsonb \
             where project_id = $1 and kind = 'requirements' and status = 'ready' \
               and (created_at < now() - make_interval(days => $2) or id in ( \
                 select id from tm_jobs where project_id = $1 and kind = 'requirements' and status = 'ready' \
                 order by created_at desc, id desc offset $3))",
        )
        .bind(ctx.project_id)
        .bind(IMPORT_READY_DAYS as i32)
        .bind(MAX_READY_IMPORTS - 1)
        .execute(&mut *tx)
        .await?;
        let other: Option<Uuid> = sqlx::query_scalar(
            "select id from tm_jobs where project_id = $1 and kind = 'requirements' \
             and status in ('queued', 'running') limit 1",
        )
        .bind(ctx.project_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(o) = other {
            return Err(TmError::Conflict {
                code: "import_busy",
                message: "another requirement import of the project is running".into(),
                details: Some(json!({"jobId": o})),
            });
        }
        let id = Uuid::now_v7();
        let job = insert_job(
            &mut tx,
            ctx,
            id,
            TmJobKind::Requirements,
            &serde_json::to_value(params)?,
            owner,
        )
        .await?;
        tx.commit().await?;
        Ok(job)
    }

    /// Asks a dry-run import that is `ready` to apply (queued again with `phase: apply`).
    pub async fn apply_requirement_import(
        &self,
        project_id: Uuid,
        project_key: &str,
        id: Uuid,
    ) -> TmResult<TmJob> {
        let mut tx = self.pool.begin().await?;
        lock_project(&mut tx, project_id, "imports").await?;
        let status: Option<String> = sqlx::query_scalar(
            "select status from tm_jobs where project_id = $1 and id = $2 and kind = 'requirements' for update",
        )
        .bind(project_id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        match status.as_deref() {
            None => return Err(TmError::NotFound("import")),
            Some("ready") => {}
            Some(s) => {
                return Err(TmError::conflict(
                    "job_not_ready",
                    format!("the import is {s}; only a ready import can be applied"),
                ));
            }
        }
        let r = sqlx::query(
            "update tm_jobs set status = 'queued', lease_owner = null, lease_until = null, finished_at = null, \
               params = params || '{\"phase\": \"apply\"}'::jsonb, \
               progress = jsonb_build_object('done', 0, 'total', progress -> 'total') \
             where id = $1 returning id, kind, status, progress, report, created_by, created_at, finished_at",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let job = job_of(&r)?;
        job_event(&mut tx, project_key, &job).await?;
        tx.commit().await?;
        Ok(job)
    }

    /// The dry run of a tracker import: what applying `items` would create, update or leave.
    pub async fn plan_requirement_import(
        &self,
        project_id: Uuid,
        tracker_id: &str,
        items: &[ImportedRequirement],
        max_requirements: usize,
        report: &mut ImportReport,
    ) -> TmResult<()> {
        let mut conn = self.pool.acquire().await?;
        let keys: Vec<String> = items.iter().map(|i| i.issue_key.clone()).collect();
        let existing = existing_by_issue(&mut conn, project_id, tracker_id, &keys).await?;
        let total: i64 =
            sqlx::query_scalar("select count(*) from requirements where project_id = $1")
                .bind(project_id)
                .fetch_one(&mut *conn)
                .await?;
        let mut room = (max_requirements as i64 - total).max(0);
        for it in items {
            match existing.get(&it.issue_key) {
                Some(cur) if import_differs(cur, it) => report.update += 1,
                Some(_) => report.skip += 1,
                None if room > 0 => {
                    room -= 1;
                    report.create += 1
                }
                None => report.skip += 1,
            }
        }
        Ok(())
    }

    /// Applies a tracker import in one transaction: new issues become requirements
    /// (`origin = tracker`, under `parent`), existing ones (same tracker and issue key) get
    /// the tracker's title, description, URL and state. Idempotent.
    pub async fn apply_requirement_items(
        &self,
        ctx: &CaseCtx<'_>,
        tracker_id: &str,
        parent: Option<i64>,
        items: &[ImportedRequirement],
    ) -> TmResult<ImportReport> {
        let mut report = ImportReport::default();
        let mut tx = self.pool.begin().await?;
        lock_project(&mut tx, ctx.project_id, "requirements").await?;
        let keys: Vec<String> = items.iter().map(|i| i.issue_key.clone()).collect();
        let existing = existing_by_issue(&mut tx, ctx.project_id, tracker_id, &keys).await?;
        for (i, it) in items.iter().enumerate() {
            match existing.get(&it.issue_key) {
                Some(cur) if !import_differs(cur, it) => report.skip += 1,
                Some(cur) => {
                    let content = cur.title != it.title || cur.description != it.description;
                    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
                        "update requirements set title = $3, description = $4, url = coalesce($5, url), \
                           external_state = $6, synced_at = now(), \
                           content_changed_at = case when $7 then now() else content_changed_at end, \
                           revision = revision + 1, updated_by = $8, updated_at = now() \
                         where project_id = $1 and id = $2 returning {REQUIREMENT_COLS}"
                    )))
                    .bind(ctx.project_id)
                    .bind(cur.id)
                    .bind(&it.title)
                    .bind(&it.description)
                    .bind(&it.url)
                    .bind(&it.external_state)
                    .bind(content)
                    .bind(no_nul(ctx.by))
                    .fetch_one(&mut *tx)
                    .await?;
                    let row = requirement_row(&r)?;
                    event(&mut tx, ctx.project_key, &row).await?;
                    report.update += 1;
                }
                None => {
                    let c = Checked {
                        title: it.title.clone(),
                        description: it.description.clone(),
                        req_type: DEFAULT_TYPE.into(),
                        status: RequirementStatus::Active,
                        tracker_id: Some(tracker_id.to_string()),
                        issue_key: Some(it.issue_key.clone()),
                        url: it.url.clone(),
                    };
                    match create_in(&mut tx, ctx, c, parent, None, RequirementOrigin::Tracker).await
                    {
                        Ok(row) => {
                            sqlx::query(
                                "update requirements set external_state = $2, synced_at = now() where id = $1",
                            )
                            .bind(row.id)
                            .bind(&it.external_state)
                            .execute(&mut *tx)
                            .await?;
                            report.create += 1;
                        }
                        Err(TmError::Conflict { code, message, .. }) => {
                            report.skip += 1;
                            if report.errors.len() < 1000 {
                                report.errors.push(ImportRowError {
                                    row: i as i64 + 1,
                                    code: code.into(),
                                    message,
                                });
                            }
                            if code == "limit_reached" {
                                break;
                            }
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        tx.commit().await?;
        Ok(report)
    }

    // -----------------------------------------------------------------------------------------
    // Tracker state sync

    /// Claims up to `limit` requirements with a tracker issue that were not read for `every`
    /// (at most `per_tracker` per project and tracker) and marks them read now, so other
    /// replicas skip them.
    pub async fn claim_requirement_syncs(
        &self,
        limit: i64,
        per_tracker: i64,
        every: std::time::Duration,
    ) -> anyhow::Result<Vec<TrackedRequirement>> {
        let rows: Vec<(Uuid, Uuid, String, String, String)> = sqlx::query_as(
            "with due as ( \
               select r.id from requirements r join projects p on p.id = r.project_id \
               where r.tracker_id is not null and r.issue_key is not null and r.archived_at is null \
                 and p.deleted_at is null \
                 and (r.synced_at is null or r.synced_at < now() - make_interval(secs => $3)) \
               order by r.synced_at nulls first, r.id limit $1 * 4 for update of r skip locked), \
             ranked as ( \
               select r.id, row_number() over (partition by r.project_id, r.tracker_id order by r.synced_at nulls first, r.id) as k \
               from requirements r join due on due.id = r.id), \
             picked as (select id from ranked where k <= $2 limit $1) \
             update requirements r set synced_at = now() from picked, projects p \
             where r.id = picked.id and p.id = r.project_id \
             returning r.id, r.project_id, p.key, r.tracker_id, r.issue_key",
        )
        .bind(limit)
        .bind(per_tracker)
        .bind(every.as_secs_f64())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(id, project_id, project_key, tracker_id, issue_key)| TrackedRequirement {
                    id,
                    project_id,
                    project_key,
                    tracker_id,
                    issue_key,
                },
            )
            .collect())
    }

    /// Stores what the tracker says about the issue of a requirement: its state and title
    /// (one way; a new title counts as a content change). Returns whether anything changed.
    pub async fn record_requirement_state(
        &self,
        t: &TrackedRequirement,
        title: &str,
        state: &str,
        url: Option<&str>,
        max_title_bytes: usize,
    ) -> anyhow::Result<bool> {
        let title = clip(one_line(title).trim(), max_title_bytes);
        let state = clip(one_line(state).trim(), MAX_STATE_BYTES);
        let url = url.filter(|u| valid_url(u));
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update requirements set \
               title = case when $3 <> '' then $3 else title end, \
               content_changed_at = case when $3 <> '' and $3 <> title then now() else content_changed_at end, \
               external_state = nullif($4, ''), url = coalesce($5, url), synced_at = now(), \
               revision = revision + 1, updated_at = now() \
             where id = $1 and project_id = $2 and issue_key = $6 and tracker_id = $7 \
               and ((($3 <> '' and $3 <> title) or nullif($4, '') is distinct from external_state \
                 or ($5::text is not null and $5 is distinct from url))) \
             returning {REQUIREMENT_COLS}"
        )))
        .bind(t.id)
        .bind(t.project_id)
        .bind(&title)
        .bind(&state)
        .bind(url)
        .bind(&t.issue_key)
        .bind(&t.tracker_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(r) = r else {
            return Ok(false);
        };
        let row = requirement_row(&r)?;
        event(&mut tx, &t.project_key, &row)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        tx.commit().await?;
        Ok(true)
    }
}

const JOB_COLS: &str = "id, kind, status, progress, report, created_by, created_at, finished_at";

/// A `tm_jobs` row as the API shows it.
pub(crate) fn job_of(r: &sqlx::postgres::PgRow) -> sqlx::Result<TmJob> {
    let kind: String = r.try_get("kind")?;
    let status: String = r.try_get("status")?;
    let progress: serde_json::Value = r.try_get("progress")?;
    let report: Option<serde_json::Value> = r.try_get("report")?;
    Ok(TmJob {
        id: r.try_get("id")?,
        kind: TmJobKind::parse(&kind).unwrap_or(TmJobKind::Import),
        status: TmJobStatus::parse(&status).unwrap_or(TmJobStatus::Queued),
        progress: serde_json::from_value::<TmJobProgress>(progress).unwrap_or_default(),
        report: report.and_then(|r| serde_json::from_value(r).ok()),
        created_by: r.try_get("created_by")?,
        created_at: r.try_get("created_at")?,
        finished_at: r.try_get("finished_at")?,
        download_url: None,
    })
}

/// `tmJob.updated` of a job.
pub(crate) async fn job_event(
    conn: &mut PgConnection,
    key: &str,
    job: &TmJob,
) -> anyhow::Result<()> {
    case_event(
        conn,
        key,
        "tmJob.updated",
        json!({"id": job.id, "kind": job.kind.as_str(), "status": job.status.as_str(),
            "progress": job.progress}),
    )
    .await
}

/// Inserts a queued job of `kind` with its live event.
pub(crate) async fn insert_job(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    id: Uuid,
    kind: TmJobKind,
    params: &serde_json::Value,
    owner: &str,
) -> TmResult<TmJob> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "insert into tm_jobs (id, project_id, kind, params, status, progress, created_by, owner_key) \
         values ($1, $2, $3, $4, 'queued', '{{\"done\": 0}}', $5, $6) returning {JOB_COLS}"
    )))
    .bind(id)
    .bind(ctx.project_id)
    .bind(kind.as_str())
    .bind(crate::json_no_nul(params.clone()))
    .bind(no_nul(ctx.by))
    .bind(no_nul(owner))
    .fetch_one(&mut *conn)
    .await?;
    let job = job_of(&r)?;
    job_event(conn, ctx.project_key, &job).await?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tm::sync::RawLink;

    fn data(links: &[(&str, &str, &str)], labels: &[(&str, &str)]) -> CodeData {
        CodeData {
            raw_links: links
                .iter()
                .map(|(t, n, u)| RawLink {
                    link_type: t.to_string(),
                    name: n.to_string(),
                    url: u.to_string(),
                })
                .collect(),
            labels: labels
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn code_keys_take_configured_link_types_and_the_label() {
        let s = CaseSyncSettings::default();
        let d = data(
            &[
                ("requirement", "REQ-3", ""),
                (
                    "Requirement",
                    "",
                    "https://tracker.example.com/browse/SHOP-7?x=1",
                ),
                ("issue", "SHOP-9", ""),
                ("tms", "TC-1", ""),
            ],
            &[("requirement", "REQ-4, SHOP-8"), ("epic", "SHOP-1")],
        );
        assert_eq!(
            code_keys(&s, &d),
            vec!["REQ-3", "SHOP-7", "REQ-4", "SHOP-8"]
        );
        let s = CaseSyncSettings {
            requirement_link_types: vec!["story".into()],
            ..Default::default()
        };
        let d = data(
            &[("story", "SHOP-2", ""), ("requirement", "REQ-1", "")],
            &[],
        );
        assert_eq!(code_keys(&s, &d), vec!["SHOP-2"]);
    }

    #[test]
    fn code_keys_are_bounded_and_clean() {
        let s = CaseSyncSettings::default();
        let many: Vec<(String, String)> = (0..200)
            .map(|i| ("requirement".to_string(), format!("REQ-{i}")))
            .collect();
        let d = CodeData {
            labels: many,
            ..Default::default()
        };
        assert_eq!(code_keys(&s, &d).len(), MAX_CODE_KEYS_PER_CASE);
        let d = data(
            &[],
            &[
                ("requirement", "has space"),
                ("requirement", &"x".repeat(65)),
            ],
        );
        assert!(code_keys(&s, &d).is_empty());
    }

    #[test]
    fn numbers_and_values() {
        assert_eq!(number_of("REQ-12"), Some(12));
        assert_eq!(number_of("req-12"), Some(12));
        assert_eq!(number_of("12"), Some(12));
        assert_eq!(number_of("REQ-0"), None);
        assert_eq!(number_of("REQ-x"), None);
        assert!(valid_url("https://tracker.example.com/browse/A-1"));
        assert!(!valid_url("javascript:alert(1)"));
        assert!(!valid_url("https://a b"));
        assert!(valid_tracker_id("jira-main"));
        assert!(!valid_tracker_id("bad id"));
        assert!(check_type("story").is_ok());
        assert!(check_type("Story").is_err());
        assert!(check_type(&"a".repeat(33)).is_err());
    }

    #[test]
    fn import_items_are_cleaned_and_bounded() {
        let limits = super::super::TmLimits::default();
        let mut report = ImportReport::default();
        let items = vec![
            ImportedRequirement {
                issue_key: " SHOP-1 ".into(),
                title: "Login\u{0007}".into(),
                url: Some("javascript:x".into()),
                ..Default::default()
            },
            ImportedRequirement {
                issue_key: "SHOP-1".into(),
                title: "dup".into(),
                ..Default::default()
            },
            ImportedRequirement {
                issue_key: "SHOP-2".into(),
                title: "".into(),
                description: "d".repeat(limits.max_markdown_bytes + 10),
                ..Default::default()
            },
        ];
        let out = clean_import(&limits, items, &mut report);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].issue_key, "SHOP-1");
        assert_eq!(out[0].title, "Login");
        assert_eq!(out[0].url, None);
        assert_eq!(out[1].title, "SHOP-2");
        assert_eq!(out[1].description.len(), limits.max_markdown_bytes);
        assert_eq!(report.skip, 1);
    }
}
