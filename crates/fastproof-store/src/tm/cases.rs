//! Cases: numbering, validation of writes against field definitions and limits, create, read
//! (folder path, identities with branches, keys, code-owned fields), update and patch with a
//! revision check, archive, restore, purge, lists with filters and opaque cursors, the tree,
//! bulk actions, identity links, attachments, history across identities and AI suggestions.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

use base64::Engine;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Postgres, QueryBuilder, Row};
use uuid::Uuid;

use super::fields::{check_value, field_defs};
use super::folders::{folder, folder_level, folder_path};
use super::identity::manual_identity;
use super::keys::{case_keys, set_case_key};
use super::lookup::{CASE_COLS, case_by_id, case_by_number, case_row};
use super::versions::{CONTENT_KEYS, changed_keys, insert_version, last_source, snapshot_of};
use super::{
    AttachmentRef, Case, CaseBody, CaseBulk, CaseBulkAction, CaseBulkResult, CaseBulkSkip,
    CaseFieldDef, CaseFilter, CaseFolderRef, CaseHistoryEntry, CaseHtml, CaseIdentity, CaseKeyKind,
    CaseLink, CaseList, CaseOrigin, CasePatch, CaseProposal, CaseRow, CaseSnapshot, CaseStatus,
    CaseStep, CaseSuggestion, CaseSummary, CaseTree, CaseTreeGroupBy, CaseTreeNode, CaseWrite,
    FieldValue, FolderCounts, IdentityKind, LastResult, MAX_TAG_BYTES, ResultSource,
    SuggestionStatus, TREE_ID_SEPARATOR, TestIdentity, TmError, TmLimits, TmResult, VersionSource,
    check_name, violated_constraint,
};
use crate::analysis::settings::{CaseSyncSettings, FieldPolicy, ProjectSettings};
use crate::events::{LiveEvent, emit, notify};
use crate::launches::like_escape;
use crate::{Store, no_nul};

/// Bytes of `cases.search_text`, at most.
pub const MAX_SEARCH_TEXT: usize = 64 * 1024;
/// Links per case, at most.
pub const MAX_LINKS: usize = 100;
/// Bytes of a link URL, at most.
pub const MAX_URL_BYTES: usize = 2048;
/// Bytes of a step id, at most.
pub const MAX_STEP_ID_BYTES: usize = 64;
/// Values in one case filter (folders, field values, tags), at most.
pub const MAX_FILTER_VALUES: usize = 1000;
/// Bytes of the text query of a filter, at most.
pub const MAX_Q_BYTES: usize = 500;
/// Identities shown on a case, at most.
pub const MAX_CASE_IDENTITIES: i64 = 100;
/// Branches shown per identity, at most.
pub const MAX_BRANCHES: usize = 20;
/// Nodes of one tree level, at most.
pub const MAX_TREE_NODES: i64 = 1000;
/// Live `case.updated` notifications sent by one bulk action; above it one notification
/// without a number tells clients to reload.
pub const MAX_BULK_NOTIFY: usize = 500;
const MAX_PROBLEMS: usize = 50;
const UNIQUE_NUMBER: &str = "cases_number_unique";

/// Who changes cases of which project, under which limits.
#[derive(Clone, Copy, Debug)]
pub struct CaseCtx<'a> {
    pub project_id: Uuid,
    pub project_key: &'a str,
    pub by: &'a str,
    pub limits: &'a TmLimits,
}

/// How [`commit`] records a change.
#[derive(Clone, Copy, Debug)]
pub struct Commit {
    pub source: VersionSource,
    pub launch_id: Option<Uuid>,
    /// Bump the revision even when the content did not change (e.g. the case became automated).
    pub force_revision: bool,
}

/// What a new case is, besides its content.
#[derive(Clone, Debug)]
pub struct NewCaseMeta {
    pub origin: CaseOrigin,
    pub source: VersionSource,
    pub launch_id: Option<Uuid>,
    pub ai_unreviewed: bool,
    pub automated: bool,
    pub code_hash: Option<String>,
    /// The number: `None` takes the next one of the project; `Some((n, false))` uses `n` and
    /// raises `case_seq` to it (imports); `Some((n, true))` uses `n` that the caller already
    /// reserved with [`reserve_numbers`].
    pub number: Option<(i64, bool)>,
}

impl NewCaseMeta {
    /// A case made in the UI (or through the API by a person).
    pub fn ui() -> Self {
        Self {
            origin: CaseOrigin::Ui,
            source: VersionSource::Ui,
            launch_id: None,
            ai_unreviewed: false,
            automated: false,
            code_hash: None,
            number: None,
        }
    }
}

#[derive(Default)]
struct Problems(Vec<Value>);

impl Problems {
    fn add(&mut self, path: impl Into<String>, message: impl Into<String>) {
        if self.0.len() < MAX_PROBLEMS {
            self.0
                .push(json!({"path": path.into(), "message": message.into()}));
        }
    }

    fn into_result(self) -> TmResult<()> {
        if self.0.is_empty() {
            return Ok(());
        }
        let first = self.0[0]["message"].as_str().unwrap_or("invalid case");
        let path = self.0[0]["path"].as_str().unwrap_or("");
        Err(TmError::Invalid {
            code: "invalid_case",
            message: format!("{path}: {first}"),
            details: Some(Value::Array(self.0)),
        })
    }
}

/// 400 `invalid_case` with one problem.
pub fn invalid_case(path: &str, message: impl Into<String>) -> TmError {
    let mut p = Problems::default();
    p.add(path, message);
    p.into_result().unwrap_err()
}

fn markdown(path: &str, s: &mut String, limits: &TmLimits) -> TmResult<()> {
    if s.contains('\0') {
        *s = s.replace('\0', "");
    }
    if s.len() > limits.max_markdown_bytes {
        return Err(TmError::Invalid {
            code: "invalid_markdown",
            message: format!(
                "{path} must have at most {} bytes of Markdown",
                limits.max_markdown_bytes
            ),
            details: Some(json!({"path": path, "max": limits.max_markdown_bytes})),
        });
    }
    Ok(())
}

/// A new stable step id: `s_` and a UUIDv7 without dashes.
pub fn new_step_id() -> String {
    format!("s_{}", Uuid::now_v7().simple())
}

/// `s_` followed by 1 to 62 letters, digits, `_` or `-`.
pub fn valid_step_id(id: &str) -> bool {
    id.len() <= MAX_STEP_ID_BYTES
        && id.len() > 2
        && id.starts_with("s_")
        && id[2..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

struct StepWalk<'a> {
    limits: &'a TmLimits,
    count: usize,
    ids: HashSet<String>,
    attachments: Vec<Uuid>,
}

impl StepWalk<'_> {
    fn walk(
        &mut self,
        steps: &mut [CaseStep],
        prefix: &str,
        depth: usize,
        p: &mut Problems,
    ) -> TmResult<()> {
        if steps.is_empty() {
            return Ok(());
        }
        if depth > self.limits.max_step_depth {
            return Err(TmError::limit(
                "maxStepDepth",
                self.limits.max_step_depth as u64,
            ));
        }
        for (i, s) in steps.iter_mut().enumerate() {
            self.count += 1;
            if self.count > self.limits.max_steps {
                return Err(TmError::limit("maxSteps", self.limits.max_steps as u64));
            }
            let path = format!("{prefix}[{i}]");
            markdown(&format!("{path}.action"), &mut s.action, self.limits)?;
            markdown(&format!("{path}.expected"), &mut s.expected, self.limits)?;
            markdown(&format!("{path}.data"), &mut s.data, self.limits)?;
            if s.id.is_empty() {
                s.id = new_step_id();
            } else if !valid_step_id(&s.id) {
                p.add(
                    format!("{path}.id"),
                    "step ids are s_ and up to 62 letters, digits, _ or -",
                );
            }
            if !self.ids.insert(s.id.clone()) {
                p.add(format!("{path}.id"), "duplicate step id");
            }
            if s.attachments.len() > self.limits.max_step_attachments {
                return Err(TmError::limit(
                    "maxStepAttachments",
                    self.limits.max_step_attachments as u64,
                ));
            }
            let mut seen = HashSet::new();
            s.attachments.retain(|a| seen.insert(*a));
            self.attachments.extend(s.attachments.iter().copied());
            self.walk(&mut s.steps, &format!("{path}.steps"), depth + 1, p)?;
        }
        Ok(())
    }
}

fn attachment_ref_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"attachment:([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})")
            .expect("attachment reference pattern")
    })
}

fn markdown_texts(body: &CaseBody) -> Vec<&str> {
    fn steps<'a>(s: &'a [CaseStep], out: &mut Vec<&'a str>) {
        for st in s {
            out.push(&st.action);
            out.push(&st.expected);
            out.push(&st.data);
            steps(&st.steps, out);
        }
    }
    let mut out = vec![body.description.as_str(), body.preconditions.as_str()];
    steps(&body.steps, &mut out);
    out
}

/// Attachment ids a body references: the step attachments and `attachment:<uuid>` images.
pub fn referenced_attachments(body: &CaseBody) -> Vec<Uuid> {
    fn steps(s: &[CaseStep], out: &mut Vec<Uuid>) {
        for st in s {
            out.extend(st.attachments.iter().copied());
            steps(&st.steps, out);
        }
    }
    let mut out = Vec::new();
    steps(&body.steps, &mut out);
    for t in markdown_texts(body) {
        for c in attachment_ref_re().captures_iter(t) {
            if let Ok(id) = Uuid::parse_str(&c[1]) {
                out.push(id);
            }
        }
    }
    let mut seen = HashSet::new();
    out.retain(|a| seen.insert(*a));
    out
}

/// Plain text of a case for full-text search: name, description, preconditions and step
/// texts without Markdown punctuation, capped at [`MAX_SEARCH_TEXT`] bytes.
pub fn search_text(name: &str, body: &CaseBody) -> String {
    let mut out = String::with_capacity(256);
    let mut push = |t: &str| {
        if out.len() >= MAX_SEARCH_TEXT {
            return;
        }
        let mut last_space = out.is_empty() || out.ends_with(' ');
        for ch in t.chars() {
            let ch = if ch.is_whitespace()
                || ch.is_control()
                || matches!(
                    ch,
                    '*' | '_' | '`' | '#' | '>' | '|' | '~' | '[' | ']' | '(' | ')' | '!' | '<'
                ) {
                ' '
            } else {
                ch
            };
            if ch == ' ' {
                if last_space {
                    continue;
                }
                last_space = true;
            } else {
                last_space = false;
            }
            if out.len() + ch.len_utf8() > MAX_SEARCH_TEXT {
                return;
            }
            out.push(ch);
        }
        if !last_space && out.len() < MAX_SEARCH_TEXT {
            out.push(' ');
        }
    };
    push(name);
    for t in markdown_texts(body) {
        push(t);
    }
    out.trim_end().to_string()
}

fn check_links(links: &mut [CaseLink], limits: &TmLimits, p: &mut Problems) {
    if links.len() > MAX_LINKS {
        p.add("body.links", format!("at most {MAX_LINKS} links"));
        return;
    }
    for (i, l) in links.iter_mut().enumerate() {
        l.url = l.url.trim().to_string();
        l.name = l.name.trim().replace('\0', "");
        let path = format!("body.links[{i}]");
        if !valid_link_url(&l.url) {
            p.add(
                format!("{path}.url"),
                "links must be http, https or mailto URLs (or /p/ paths) of at most 2048 bytes",
            );
        }
        if l.name.len() > limits.max_name_bytes || l.name.chars().any(char::is_control) {
            p.add(
                format!("{path}.name"),
                format!(
                    "link names have at most {} bytes without control characters",
                    limits.max_name_bytes
                ),
            );
        }
    }
}

/// An http, https or mailto URL, or a path inside FastProof (`/p/...`).
pub fn valid_link_url(u: &str) -> bool {
    if u.is_empty()
        || u.len() > MAX_URL_BYTES
        || u.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return false;
    }
    if let Some(rest) = u.strip_prefix("/p/") {
        return !rest.is_empty()
            && !rest.starts_with('/')
            && !rest.contains("//")
            && !rest.contains('\\');
    }
    let lower = u.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://")) && u.len() > 8
        || lower.starts_with("mailto:") && u.len() > 7
}

/// Trimmed, deduplicated tags; errors name the tag.
fn normalize_tags(tags: &[String], limits: &TmLimits, p: &mut Problems) -> TmResult<Vec<String>> {
    let mut out: Vec<String> = Vec::with_capacity(tags.len());
    for (i, t) in tags.iter().enumerate() {
        let t = t.trim();
        if t.is_empty() || t.len() > MAX_TAG_BYTES || t.chars().any(char::is_control) {
            p.add(
                format!("tags[{i}]"),
                format!("tags have 1 to {MAX_TAG_BYTES} bytes without control characters"),
            );
            continue;
        }
        if !out.iter().any(|o| o == t) {
            out.push(t.to_string());
        }
    }
    if out.len() > limits.max_tags {
        return Err(TmError::limit("maxTags", limits.max_tags as u64));
    }
    Ok(out)
}

fn clean_value(v: &mut FieldValue) {
    match v {
        FieldValue::Text(s) if s.contains('\0') => *s = s.replace('\0', ""),
        FieldValue::Many(vs) => {
            for s in vs.iter_mut() {
                if s.contains('\0') {
                    *s = s.replace('\0', "");
                }
            }
        }
        _ => {}
    }
}

/// Validates field values against the definitions: unknown and archived fields are refused
/// unless the value is the one the case already has; `null` removes a value. `create` fills
/// in defaults; `required` fields need a value unless the case is a draft.
fn check_fields(
    fields: &mut BTreeMap<String, FieldValue>,
    current: Option<&BTreeMap<String, FieldValue>>,
    defs: &[CaseFieldDef],
    status: CaseStatus,
    create: bool,
    limits: &TmLimits,
    p: &mut Problems,
) {
    let active: HashMap<&str, &CaseFieldDef> = defs
        .iter()
        .filter(|d| !d.archived)
        .map(|d| (d.key.as_str(), d))
        .collect();
    let mut out = BTreeMap::new();
    for (k, mut v) in std::mem::take(fields) {
        clean_value(&mut v);
        if v.is_null() {
            continue;
        }
        let path = format!("fields.{k}");
        match active.get(k.as_str()) {
            Some(def) => {
                if let Err(m) = check_value(def, &v, limits) {
                    p.add(path, m);
                    continue;
                }
            }
            None if current.and_then(|c| c.get(&k)) == Some(&v) => {}
            None => {
                p.add(path, "unknown field");
                continue;
            }
        }
        out.insert(k, v);
    }
    if create {
        for d in active.values() {
            if !d.default_value.is_null() && !out.contains_key(&d.key) {
                out.insert(d.key.clone(), d.default_value.clone());
            }
        }
    }
    if status != CaseStatus::Draft {
        for d in active.values() {
            if d.required && !out.contains_key(&d.key) {
                p.add(format!("fields.{}", d.key), "required");
            }
        }
    }
    *fields = out;
}

/// Resolves attachment ids (or attachment reference ids) of the project to attachment ids.
async fn resolve_attachments(
    conn: &mut PgConnection,
    project_id: Uuid,
    ids: &[Uuid],
) -> sqlx::Result<HashMap<Uuid, Uuid>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "select id, id from attachments where project_id = $1 and id = any($2) \
         union all \
         select r.id, a.id from attachment_refs r join attachments a on a.id = r.attachment_id \
           where a.project_id = $1 and r.id = any($2)",
    )
    .bind(project_id)
    .bind(ids)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Validates a case write and returns the content to store: trimmed name, step ids assigned
/// (kept when given), Markdown sizes, nesting and step counts, links, tags, field values,
/// folder and step attachments of the project (other ids: 404). `current` is the case being
/// replaced; fields, tags and status absent from `w` keep its values.
pub async fn prepare_write(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    w: &CaseWrite,
    current: Option<&CaseRow>,
    defs: &[CaseFieldDef],
) -> TmResult<CaseSnapshot> {
    let l = ctx.limits;
    let name = check_name("name", &w.name, l.max_name_bytes).map_err(|e| match e {
        TmError::Invalid { message, .. } => invalid_case("name", message),
        e => e,
    })?;
    let mut p = Problems::default();
    let mut body = w.body.clone();
    markdown("body.description", &mut body.description, l)?;
    markdown("body.preconditions", &mut body.preconditions, l)?;
    let mut walk = StepWalk {
        limits: l,
        count: 0,
        ids: HashSet::new(),
        attachments: Vec::new(),
    };
    walk.walk(&mut body.steps, "body.steps", 1, &mut p)?;
    check_links(&mut body.links, l, &mut p);
    if serde_json::to_vec(&body)?.len() > l.max_body_bytes {
        return Err(TmError::limit("maxBodyBytes", l.max_body_bytes as u64));
    }
    if let Some(f) = w.folder_id
        && folder(conn, ctx.project_id, f).await?.is_none()
    {
        return Err(TmError::NotFound("folder"));
    }
    let status = w
        .status
        .or(current.map(|c| c.status))
        .unwrap_or(CaseStatus::Active);
    let tags = match (&w.tags, current) {
        (Some(t), _) => normalize_tags(t, l, &mut p)?,
        (None, Some(c)) => c.tags.clone(),
        (None, None) => Vec::new(),
    };
    let mut fields = match (&w.fields, current) {
        (Some(f), _) => f.clone(),
        (None, Some(c)) => c.fields.clone(),
        (None, None) => BTreeMap::new(),
    };
    check_fields(
        &mut fields,
        current.map(|c| &c.fields),
        defs,
        status,
        current.is_none(),
        l,
        &mut p,
    );
    if !walk.attachments.is_empty() {
        let found = resolve_attachments(conn, ctx.project_id, &walk.attachments).await?;
        if walk.attachments.iter().any(|a| !found.contains_key(a)) {
            return Err(TmError::NotFound("attachment"));
        }
    }
    p.into_result()?;
    Ok(CaseSnapshot {
        name,
        status,
        folder_id: w.folder_id,
        body,
        fields,
        tags,
    })
}

/// The `caseSync` settings of a project.
pub async fn case_sync_settings(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> anyhow::Result<CaseSyncSettings> {
    let v: Option<Value> = sqlx::query_scalar("select settings from projects where id = $1")
        .bind(project_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(ProjectSettings::from_value(v.unwrap_or(Value::Null)).case_sync)
}

/// Keys whose values come from code on automated cases: `name`, `description`, `links`,
/// `tags` and the fields with a source label, unless their policy is `case`.
pub fn owned_keys(s: &CaseSyncSettings, defs: &[CaseFieldDef]) -> Vec<String> {
    let mut out: Vec<String> = ["name", "description", "links", "tags"]
        .iter()
        .filter(|k| s.fields.policy(k) == Some(FieldPolicy::Code))
        .map(|k| k.to_string())
        .collect();
    for d in defs.iter().filter(|d| !d.archived) {
        if let Some(label) = d.source_label.as_deref()
            && s.fields.policy(label).unwrap_or(FieldPolicy::Code) == FieldPolicy::Code
        {
            out.push(d.key.clone());
        }
    }
    out
}

/// [`owned_keys`] of a project.
pub async fn code_owned_keys(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> anyhow::Result<Vec<String>> {
    let s = case_sync_settings(conn, project_id).await?;
    let defs = field_defs(conn, project_id, false).await?;
    Ok(owned_keys(&s, &defs))
}

/// Code-owned keys whose value `new` changes on an automated case.
pub fn code_conflicts(cur: &CaseRow, new: &CaseSnapshot, owned: &[String]) -> Vec<String> {
    if !cur.automated {
        return Vec::new();
    }
    owned
        .iter()
        .filter(|k| match k.as_str() {
            "name" => cur.name != new.name,
            "description" => cur.body.description != new.body.description,
            "links" => cur.body.links != new.body.links,
            "tags" => cur.tags != new.tags,
            key => cur.fields.get(key) != new.fields.get(key),
        })
        .cloned()
        .collect()
}

/// Puts the current code-owned values of `cur` into `snap`.
pub fn keep_code_owned(cur: &CaseRow, snap: &mut CaseSnapshot, owned: &[String]) {
    for k in owned {
        match k.as_str() {
            "name" => snap.name = cur.name.clone(),
            "description" => snap.body.description = cur.body.description.clone(),
            "links" => snap.body.links = cur.body.links.clone(),
            "tags" => snap.tags = cur.tags.clone(),
            key => match cur.fields.get(key) {
                Some(v) => {
                    snap.fields.insert(key.to_string(), v.clone());
                }
                None => {
                    snap.fields.remove(key);
                }
            },
        }
    }
}

fn field_from_code(fields: Vec<String>) -> TmError {
    TmError::Conflict {
        code: "field_from_code",
        message: format!(
            "{} come(s) from code on this automated case",
            fields.join(", ")
        ),
        details: Some(json!({ "fields": fields })),
    }
}

/// Writes an outbox event and its live notification.
pub async fn case_event(
    conn: &mut PgConnection,
    project_key: &str,
    kind: &str,
    data: Value,
) -> anyhow::Result<()> {
    emit(&mut *conn, kind, Some(project_key), data.clone()).await?;
    notify(
        &mut *conn,
        &LiveEvent {
            project: project_key.to_string(),
            kind: kind.to_string(),
            data,
        },
    )
    .await
}

/// The case with number `n`, locked for update; with `expected`, a different revision is
/// [`TmError::Stale`].
pub async fn lock_case_by_number(
    conn: &mut PgConnection,
    project_id: Uuid,
    n: i64,
    expected: Option<i64>,
) -> TmResult<CaseRow> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {CASE_COLS} from cases where project_id = $1 and number = $2 for update"
    )))
    .bind(project_id)
    .bind(n)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(TmError::NotFound("case"))?;
    let row = case_row(&r)?;
    if let Some(e) = expected
        && e != row.revision
    {
        return Err(stale(conn, &row).await);
    }
    Ok(row)
}

async fn stale(conn: &mut PgConnection, row: &CaseRow) -> TmError {
    TmError::Stale {
        current_revision: row.revision,
        changed_by: row.updated_by.clone(),
        source: last_source(conn, row.id).await.ok().flatten(),
    }
}

/// Links the attachments a body references to the case (`case_attachments`), so they live as
/// long as the case; `maxCaseAttachments` counts every attachment the case ever had.
async fn link_attachments(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    case_id: Uuid,
    body: &CaseBody,
) -> TmResult<()> {
    let ids = referenced_attachments(body);
    if ids.is_empty() {
        return Ok(());
    }
    let resolved: Vec<Uuid> = resolve_attachments(conn, ctx.project_id, &ids)
        .await?
        .into_values()
        .collect();
    if resolved.is_empty() {
        return Ok(());
    }
    let added = sqlx::query(
        "insert into case_attachments (case_id, attachment_id, name, content_type, created_by) \
         select $1, a.id, coalesce(a.original_name, 'attachment'), a.content_type, $3 \
         from attachments a where a.project_id = $2 and a.id = any($4) \
         on conflict (case_id, attachment_id) do nothing",
    )
    .bind(case_id)
    .bind(ctx.project_id)
    .bind(no_nul(ctx.by))
    .bind(&resolved)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if added > 0 {
        let n: i64 = sqlx::query_scalar("select count(*) from case_attachments where case_id = $1")
            .bind(case_id)
            .fetch_one(&mut *conn)
            .await?;
        if n as usize > ctx.limits.max_case_attachments {
            return Err(TmError::limit(
                "maxCaseAttachments",
                ctx.limits.max_case_attachments as u64,
            ));
        }
    }
    Ok(())
}

/// Stores new content of a locked case: one version per content change (with the changed
/// keys), `revision + 1`, `case.updated`. Returns the case unchanged when nothing changed
/// (and `force_revision` is off).
pub async fn commit(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    cur: &CaseRow,
    new: CaseSnapshot,
    c: Commit,
) -> TmResult<CaseRow> {
    let changed = changed_keys(&snapshot_of(cur), &new);
    if changed.is_empty() && !c.force_revision {
        return Ok(cur.clone());
    }
    if !changed.is_empty() {
        link_attachments(conn, ctx, cur.id, &new.body).await?;
    }
    let version = if changed.is_empty() {
        cur.version
    } else {
        cur.version + 1
    };
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "update cases set name = $3, status = $4, folder_id = $5, body = $6, fields = $7, tags = $8, \
         search_text = $9, revision = revision + 1, version = $10, updated_by = $11, updated_at = now() \
         where project_id = $1 and id = $2 and revision = $12 returning {CASE_COLS}"
    )))
    .bind(ctx.project_id)
    .bind(cur.id)
    .bind(&new.name)
    .bind(new.status.as_str())
    .bind(new.folder_id)
    .bind(serde_json::to_value(&new.body)?)
    .bind(serde_json::to_value(&new.fields)?)
    .bind(&new.tags)
    .bind(search_text(&new.name, &new.body))
    .bind(version)
    .bind(no_nul(ctx.by))
    .bind(cur.revision)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(r) = r else {
        return Err(stale(conn, cur).await);
    };
    let row = case_row(&r)?;
    if !changed.is_empty() {
        insert_version(
            conn,
            ctx.project_id,
            cur.id,
            version,
            c.source,
            c.launch_id,
            &changed,
            &new,
            ctx.by,
        )
        .await?;
    }
    if cur.name != row.name {
        sqlx::query(
            "update test_cases set display_name = $3 where project_id = $1 and case_id = $2 and kind = 'manual'",
        )
        .bind(ctx.project_id)
        .bind(cur.id)
        .bind(&row.name)
        .execute(&mut *conn)
        .await?;
    }
    case_event(
        conn,
        ctx.project_key,
        "case.updated",
        json!({"number": row.number, "revision": row.revision, "source": c.source.as_str()}),
    )
    .await?;
    Ok(row)
}

async fn check_case_count(
    conn: &mut PgConnection,
    project_id: Uuid,
    seq: i64,
    limits: &TmLimits,
    adding: usize,
) -> TmResult<()> {
    if (seq.max(0) as usize) < limits.max_cases {
        return Ok(());
    }
    let n: i64 = sqlx::query_scalar("select count(*) from cases where project_id = $1")
        .bind(project_id)
        .fetch_one(&mut *conn)
        .await?;
    if n as usize + adding > limits.max_cases {
        return Err(TmError::limit("maxCases", limits.max_cases as u64));
    }
    Ok(())
}

/// Reserves `k` case numbers in a statement of its own (callers that create many cases in a
/// long transaction use it, so the project row is not held). Returns the first number. A
/// transaction that fails afterwards leaves a gap in the numbers.
pub async fn reserve_numbers(
    pool: &sqlx::PgPool,
    project_id: Uuid,
    k: i64,
    limits: &TmLimits,
) -> TmResult<i64> {
    let mut conn = pool.acquire().await?;
    let seq: i64 = sqlx::query_scalar("select case_seq from projects where id = $1")
        .bind(project_id)
        .fetch_one(&mut *conn)
        .await?;
    check_case_count(&mut conn, project_id, seq + k, limits, k as usize).await?;
    let last: i64 = sqlx::query_scalar(
        "update projects set case_seq = case_seq + $2 where id = $1 returning case_seq",
    )
    .bind(project_id)
    .bind(k)
    .fetch_one(&mut *conn)
    .await?;
    Ok(last - k + 1)
}

/// Inserts a validated case with its version 1 (all content keys changed), the `allure_id`
/// key equal to its number when that key is free, the manual identity (unless it comes from
/// code) and `case.updated`.
pub async fn create_case_in(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    snap: CaseSnapshot,
    meta: &NewCaseMeta,
) -> TmResult<CaseRow> {
    let number = match meta.number {
        Some((n, true)) => n,
        Some((n, false)) => {
            if n < 1 {
                return Err(TmError::bad_request("case numbers start at 1"));
            }
            let seq: i64 = sqlx::query_scalar(
                "update projects set case_seq = greatest(case_seq, $2) where id = $1 returning case_seq",
            )
            .bind(ctx.project_id)
            .bind(n)
            .fetch_one(&mut *conn)
            .await?;
            check_case_count(conn, ctx.project_id, seq, ctx.limits, 1).await?;
            n
        }
        None => {
            let seq: i64 = sqlx::query_scalar(
                "update projects set case_seq = case_seq + 1 where id = $1 returning case_seq",
            )
            .bind(ctx.project_id)
            .fetch_one(&mut *conn)
            .await?;
            check_case_count(conn, ctx.project_id, seq, ctx.limits, 1).await?;
            seq
        }
    };
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into cases (id, project_id, number, name, status, automated, origin, ai_unreviewed, folder_id, \
           body, fields, tags, search_text, code_hash, last_trusted_at, created_by, updated_by) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, \
           case when $6 then now() end, $15, $15)",
    )
    .bind(id)
    .bind(ctx.project_id)
    .bind(number)
    .bind(&snap.name)
    .bind(snap.status.as_str())
    .bind(meta.automated)
    .bind(meta.origin.as_str())
    .bind(meta.ai_unreviewed)
    .bind(snap.folder_id)
    .bind(serde_json::to_value(&snap.body)?)
    .bind(serde_json::to_value(&snap.fields)?)
    .bind(&snap.tags)
    .bind(search_text(&snap.name, &snap.body))
    .bind(&meta.code_hash)
    .bind(no_nul(ctx.by))
    .execute(&mut *conn)
    .await
    .map_err(|e| {
        if violated_constraint(&e) == Some(UNIQUE_NUMBER) {
            TmError::conflict("case_number_taken", format!("TC-{number} exists"))
        } else {
            e.into()
        }
    })?;
    link_attachments(conn, ctx, id, &snap.body).await?;
    let all: Vec<String> = CONTENT_KEYS.iter().map(|k| k.to_string()).collect();
    insert_version(
        conn,
        ctx.project_id,
        id,
        1,
        meta.source,
        meta.launch_id,
        &all,
        &snap,
        ctx.by,
    )
    .await?;
    set_case_key(
        conn,
        ctx.project_id,
        id,
        CaseKeyKind::AllureId,
        &number.to_string(),
    )
    .await?;
    if meta.origin != CaseOrigin::Code {
        manual_identity(conn, ctx.project_id, id).await?;
    }
    case_event(
        conn,
        ctx.project_key,
        "case.updated",
        json!({"number": number, "revision": 1, "source": meta.source.as_str()}),
    )
    .await?;
    case_by_id(conn, ctx.project_id, id)
        .await?
        .ok_or_else(|| TmError::Other(anyhow::anyhow!("created case vanished")))
}

/// Applies a merge patch to a snapshot: `folderId` (`null` = root), `status`, `fields` (keys
/// merged, `null` removes), `tags` (replaced).
pub fn apply_patch(snap: &mut CaseSnapshot, p: &CasePatch) {
    if let Some(f) = p.folder_id {
        snap.folder_id = f;
    }
    if let Some(s) = p.status {
        snap.status = s;
    }
    if let Some(fs) = &p.fields {
        for (k, v) in fs {
            if v.is_null() {
                snap.fields.remove(k);
            } else {
                snap.fields.insert(k.clone(), v.clone());
            }
        }
    }
    if let Some(t) = &p.tags {
        snap.tags = t.clone();
    }
}

fn write_of(snap: &CaseSnapshot) -> CaseWrite {
    CaseWrite {
        name: snap.name.clone(),
        folder_id: snap.folder_id,
        status: Some(snap.status),
        body: snap.body.clone(),
        fields: Some(snap.fields.clone()),
        tags: Some(snap.tags.clone()),
    }
}

/// Replaces the content of a locked case from a write (validation, code-owned check, commit).
pub async fn update_case_in(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    cur: &CaseRow,
    w: &CaseWrite,
    source: VersionSource,
) -> TmResult<CaseRow> {
    let defs = field_defs(conn, ctx.project_id, true).await?;
    let snap = prepare_write(conn, ctx, w, Some(cur), &defs).await?;
    if cur.automated {
        let s = case_sync_settings(conn, ctx.project_id).await?;
        let conflicts = code_conflicts(cur, &snap, &owned_keys(&s, &defs));
        if !conflicts.is_empty() {
            return Err(field_from_code(conflicts));
        }
    }
    commit(
        conn,
        ctx,
        cur,
        snap,
        Commit {
            source,
            launch_id: None,
            force_revision: false,
        },
    )
    .await
}

fn decode_cursor(c: &str) -> Option<String> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c)
        .ok()?;
    String::from_utf8(raw).ok()
}

fn encode_cursor(s: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
}

fn time_id_cursor(c: &str) -> Option<(DateTime<Utc>, Uuid)> {
    let s = decode_cursor(c)?;
    let (t, id) = s.split_once('|')?;
    let micros: i64 = t.parse().ok()?;
    Some((DateTime::from_timestamp_micros(micros)?, id.parse().ok()?))
}

fn time_id_encode(t: DateTime<Utc>, id: Uuid) -> String {
    encode_cursor(&format!("{}|{id}", t.timestamp_micros()))
}

/// Order of case lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaseSort {
    /// By number, ascending.
    #[default]
    Number,
    /// By number, descending.
    NumberDesc,
    /// Most recently updated first (`updated_at, id`).
    Updated,
}

impl CaseSort {
    /// `number`, `-number` or `updated` (also `-updated`).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "number" => Some(Self::Number),
            "-number" => Some(Self::NumberDesc),
            "updated" | "-updated" => Some(Self::Updated),
            _ => None,
        }
    }
}

/// A case list request.
#[derive(Clone, Debug, Default)]
pub struct CaseQuery {
    pub filter: CaseFilter,
    /// Automated cases without a trusted result for this many days.
    pub stale_days: Option<u32>,
    pub ai_unreviewed: Option<bool>,
    /// `true` lists archived cases only, `false` the others.
    pub archived: bool,
    pub sort: CaseSort,
    pub before: Option<String>,
    pub limit: i64,
}

/// Checks the sizes of a filter.
pub fn check_filter(f: &CaseFilter) -> TmResult<()> {
    let n = f.folder_ids.as_ref().map_or(0, Vec::len)
        + f.statuses.as_ref().map_or(0, Vec::len)
        + f.tags.as_ref().map_or(0, Vec::len)
        + f.fields
            .as_ref()
            .map_or(0, |m| m.values().map(|v| v.len().max(1)).sum());
    if n > MAX_FILTER_VALUES {
        return Err(TmError::bad_request(format!(
            "filters take at most {MAX_FILTER_VALUES} values"
        )));
    }
    if f.q.as_ref().is_some_and(|q| q.len() > MAX_Q_BYTES) {
        return Err(TmError::bad_request(format!(
            "q has at most {MAX_Q_BYTES} bytes"
        )));
    }
    Ok(())
}

/// 404 when a filter names a folder of another project (or none).
pub async fn check_filter_folders(
    conn: &mut PgConnection,
    project_id: Uuid,
    f: &CaseFilter,
) -> TmResult<()> {
    let Some(ids) = f.folder_ids.as_ref().filter(|v| !v.is_empty()) else {
        return Ok(());
    };
    let mut ids = ids.clone();
    ids.sort();
    ids.dedup();
    let n: i64 = sqlx::query_scalar(
        "select count(*) from case_folders where project_id = $1 and id = any($2)",
    )
    .bind(project_id)
    .bind(&ids)
    .fetch_one(&mut *conn)
    .await?;
    if n as usize != ids.len() {
        return Err(TmError::NotFound("folder"));
    }
    Ok(())
}

/// Appends the conditions of a filter on cases aliased `c` (` and ...`).
pub fn push_filter(qb: &mut QueryBuilder<Postgres>, project_id: Uuid, f: &CaseFilter) {
    if let Some(ids) = f.folder_ids.as_ref().filter(|v| !v.is_empty()) {
        if f.recursive.unwrap_or(false) {
            qb.push(" and c.folder_id in (select d.id from case_folders d where d.project_id = ")
                .push_bind(project_id)
                .push(" and d.path_ids && ")
                .push_bind(ids.clone())
                .push(")");
        } else {
            qb.push(" and c.folder_id = any(")
                .push_bind(ids.clone())
                .push(")");
        }
    }
    if let Some(s) = f.statuses.as_ref().filter(|v| !v.is_empty()) {
        let s: Vec<String> = s.iter().map(|s| s.as_str().to_string()).collect();
        qb.push(" and c.status = any(").push_bind(s).push(")");
    }
    match f.automation {
        Some(ResultSource::Automated) => {
            qb.push(" and c.automated");
        }
        Some(ResultSource::Manual) => {
            qb.push(" and not c.automated");
        }
        None => {}
    }
    if let Some(fields) = &f.fields {
        for (k, values) in fields {
            if values.is_empty() {
                qb.push(" and c.fields ? ").push_bind(no_nul(k));
                continue;
            }
            qb.push(" and (false");
            for v in values {
                let v = no_nul(v);
                qb.push(" or c.fields @> ")
                    .push_bind(json!({ k.as_str(): v }))
                    .push(" or c.fields @> ")
                    .push_bind(json!({ k.as_str(): [v] }));
                if let Ok(n) = v.parse::<f64>()
                    && n.is_finite()
                {
                    qb.push(" or c.fields @> ")
                        .push_bind(json!({ k.as_str(): n }));
                }
            }
            qb.push(")");
        }
    }
    if let Some(t) = f.tags.as_ref().filter(|v| !v.is_empty()) {
        let t: Vec<String> = t.iter().map(|t| no_nul(t.trim())).collect();
        qb.push(" and c.tags @> ").push_bind(t);
    }
    if let Some(q) = f.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let q = no_nul(q);
        let number = q
            .strip_prefix("TC-")
            .or(q.strip_prefix("tc-"))
            .unwrap_or(&q)
            .parse::<i64>()
            .ok();
        qb.push(" and (to_tsvector('simple', c.search_text) @@ plainto_tsquery('simple', ")
            .push_bind(q.clone())
            .push(") or c.name ilike ")
            .push_bind(format!("%{}%", like_escape(&q)));
        if let Some(n) = number {
            qb.push(" or c.number = ").push_bind(n);
        }
        qb.push(")");
    }
}

const SUMMARY_SELECT: &str = "select c.id, c.number, c.name, c.status, c.automated, c.folder_id, c.fields, c.tags, \
    c.updated_at, c.ai_unreviewed, c.archived_at is not null as archived, \
    lr.status as lr_status, lr.at as lr_at, lr.manual as lr_manual, lr.launch_id as lr_launch, lr.id as lr_id \
    from cases c left join lateral (select x.status, x.at, x.manual, x.launch_id, x.id from test_cases t \
      cross join lateral (select r.status, coalesce(r.stopped_at, r.updated_at) as at, r.run_item_id is not null as manual, \
        r.launch_id, r.id, r.launch_created_at from results r where r.test_case_id = t.id \
        order by r.launch_created_at desc, r.updated_at desc limit 1) x \
      where t.case_id = c.id order by x.launch_created_at desc limit 1) lr on true ";

fn summary_row(r: &PgRow) -> sqlx::Result<CaseSummary> {
    let status: String = r.try_get("status")?;
    let fields: Value = r.try_get("fields")?;
    let last = match r.try_get::<Option<String>, _>("lr_status")? {
        Some(s) => Some(LastResult {
            status: s,
            at: r.try_get("lr_at")?,
            source: if r.try_get::<bool, _>("lr_manual")? {
                ResultSource::Manual
            } else {
                ResultSource::Automated
            },
            launch_id: r.try_get("lr_launch")?,
            result_id: r.try_get("lr_id")?,
        }),
        None => None,
    };
    Ok(CaseSummary {
        number: r.try_get("number")?,
        name: r.try_get("name")?,
        status: CaseStatus::parse(&status).unwrap_or_default(),
        automated: r.try_get("automated")?,
        folder_id: r.try_get("folder_id")?,
        fields: serde_json::from_value(fields).unwrap_or_default(),
        tags: r.try_get("tags")?,
        last_result: last,
        updated_at: r.try_get("updated_at")?,
        ai_unreviewed: r.try_get("ai_unreviewed")?,
        archived: r.try_get("archived")?,
    })
}

/// Identities of a case (manual first, then by last seen), with the branches they were seen on.
pub async fn case_identities(
    conn: &mut PgConnection,
    project_id: Uuid,
    case_id: Uuid,
) -> sqlx::Result<Vec<CaseIdentity>> {
    let rows: Vec<(Uuid, String, String, DateTime<Utc>)> = sqlx::query_as(
        "select id, full_name, kind, last_seen_at from test_cases where project_id = $1 and case_id = $2 \
         order by kind desc, last_seen_at desc limit $3",
    )
    .bind(project_id)
    .bind(case_id)
    .bind(MAX_CASE_IDENTITIES)
    .fetch_all(&mut *conn)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    let mut branches = identity_branches(conn, project_id, &ids).await?;
    Ok(rows
        .into_iter()
        .map(|(id, full_name, kind, last_seen_at)| CaseIdentity {
            test_case_id: id,
            full_name,
            kind: IdentityKind::parse(&kind).unwrap_or(IdentityKind::Code),
            last_seen_at,
            branches: branches.remove(&id).unwrap_or_default(),
        })
        .collect())
}

/// Branches each identity was seen on: launches of its newest 200 results and the rolled-up
/// history (`series_daily`), at most [`MAX_BRANCHES`] each.
pub async fn identity_branches(
    conn: &mut PgConnection,
    project_id: Uuid,
    ids: &[Uuid],
) -> sqlx::Result<HashMap<Uuid, Vec<String>>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(Uuid, Vec<String>)> = sqlx::query_as(
        "select x.tc, array_agg(distinct x.branch order by x.branch) from ( \
           select i.id as tc, l.branch from unnest($2::uuid[]) as i(id) \
             cross join lateral (select r.launch_id from results r where r.test_case_id = i.id \
               order by r.launch_created_at desc limit 200) r \
             join launches l on l.id = r.launch_id and l.project_id = $1 \
             where l.branch is not null and l.branch <> '' \
           union \
           select s.test_case_id, d.branch from series s \
             join series_daily d on d.series_id = s.id and d.project_id = $1 \
             where s.project_id = $1 and s.test_case_id = any($2) and d.branch <> '' \
         ) x group by x.tc",
    )
    .bind(project_id)
    .bind(ids)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, mut b)| {
            b.truncate(MAX_BRANCHES);
            (id, b)
        })
        .collect())
}

/// The API view of a case (HTML left empty: the server renders it).
pub async fn case_view(conn: &mut PgConnection, project_id: Uuid, row: &CaseRow) -> TmResult<Case> {
    let folder = match row.folder_id {
        Some(f) => {
            let path = folder_path(conn, project_id, f).await?;
            (!path.is_empty()).then_some(CaseFolderRef { id: f, path })
        }
        None => None,
    };
    let identities = case_identities(conn, project_id, row.id).await?;
    let identities_total = if (identities.len() as i64) < MAX_CASE_IDENTITIES {
        identities.len() as i64
    } else {
        sqlx::query_scalar("select count(*) from test_cases where project_id = $1 and case_id = $2")
            .bind(project_id)
            .bind(row.id)
            .fetch_one(&mut *conn)
            .await?
    };
    let keys = case_keys(conn, row.id).await?;
    let code_owned = if row.automated {
        code_owned_keys(conn, project_id).await?
    } else {
        Vec::new()
    };
    Ok(Case {
        number: row.number,
        id: row.id,
        name: row.name.clone(),
        status: row.status,
        automated: row.automated,
        origin: row.origin,
        ai_unreviewed: row.ai_unreviewed,
        folder,
        body: row.body.clone(),
        html: CaseHtml::default(),
        fields: row.fields.clone(),
        tags: row.tags.clone(),
        code_owned,
        identities,
        identities_total,
        keys,
        revision: row.revision,
        version: row.version,
        created_by: row.created_by.clone(),
        created_at: row.created_at,
        updated_by: row.updated_by.clone(),
        updated_at: row.updated_at,
        archived_at: row.archived_at,
    })
}

/// Numbers of the (not archived, unless `archived`) cases matching a filter, by number, at
/// most `max + 1` (callers compare with `max`).
pub async fn filter_numbers(
    conn: &mut PgConnection,
    project_id: Uuid,
    f: &CaseFilter,
    archived: Option<bool>,
    max: usize,
) -> TmResult<Vec<i64>> {
    check_filter(f)?;
    check_filter_folders(conn, project_id, f).await?;
    let mut qb: QueryBuilder<Postgres> =
        QueryBuilder::new("select c.number from cases c where c.project_id = ");
    qb.push_bind(project_id);
    match archived {
        Some(true) => {
            qb.push(" and c.archived_at is not null");
        }
        Some(false) => {
            qb.push(" and c.archived_at is null");
        }
        None => {}
    }
    push_filter(&mut qb, project_id, f);
    qb.push(" order by c.number limit ")
        .push_bind(max as i64 + 1);
    Ok(qb.build_query_scalar().fetch_all(&mut *conn).await?)
}

/// A history request of a case.
#[derive(Clone, Debug, Default)]
pub struct HistoryQuery {
    pub branch: Option<String>,
    pub environment: Option<String>,
    pub source: Option<ResultSource>,
    pub before: Option<String>,
    pub limit: i64,
}

/// A request for test identities.
#[derive(Clone, Debug, Default)]
pub struct IdentityQuery {
    /// Only identities without a case.
    pub unlinked: bool,
    pub branch: Option<String>,
    pub q: Option<String>,
    pub before: Option<String>,
    pub limit: i64,
}

/// What a bulk value means for an action.
enum BulkOp {
    Move(Option<Uuid>),
    Status(CaseStatus),
    Field(String, FieldValue),
    AddTags(Vec<String>),
    RemoveTags(Vec<String>),
    Archive,
    Restore,
}

fn bulk_op(b: &CaseBulk, limits: &TmLimits) -> TmResult<BulkOp> {
    let v = b.value.clone().unwrap_or(Value::Null);
    let bad = |m: &str| TmError::bad_request(format!("value of {}: {m}", b.action));
    Ok(match b.action {
        CaseBulkAction::Move => BulkOp::Move(match v {
            Value::Null => None,
            Value::String(s) => Some(s.parse().map_err(|_| bad("a folder id or null"))?),
            Value::Object(o) => match o.get("folderId") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.parse().map_err(|_| bad("a folder id"))?),
                _ => return Err(bad("a folder id or null")),
            },
            _ => return Err(bad("a folder id or null")),
        }),
        CaseBulkAction::SetStatus => {
            BulkOp::Status(serde_json::from_value(v).map_err(|_| bad("a case status"))?)
        }
        CaseBulkAction::SetField => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Set {
                key: String,
                #[serde(default)]
                value: FieldValue,
            }
            let s: Set = serde_json::from_value(v).map_err(|_| bad("{key, value}"))?;
            BulkOp::Field(s.key, s.value)
        }
        CaseBulkAction::AddTags | CaseBulkAction::RemoveTags => {
            let t: Vec<String> = serde_json::from_value(v).map_err(|_| bad("a list of tags"))?;
            let mut p = Problems::default();
            let t = normalize_tags(&t, limits, &mut p)?;
            p.into_result()?;
            if b.action == CaseBulkAction::AddTags {
                BulkOp::AddTags(t)
            } else {
                BulkOp::RemoveTags(t)
            }
        }
        CaseBulkAction::Archive => BulkOp::Archive,
        CaseBulkAction::Restore => BulkOp::Restore,
    })
}

fn ident_row(r: &PgRow, branches: &mut HashMap<Uuid, Vec<String>>) -> sqlx::Result<TestIdentity> {
    let id: Uuid = r.try_get("id")?;
    let kind: String = r.try_get("kind")?;
    Ok(TestIdentity {
        test_case_id: id,
        full_name: r.try_get("full_name")?,
        display_name: r.try_get("display_name")?,
        kind: IdentityKind::parse(&kind).unwrap_or(IdentityKind::Code),
        case_number: r.try_get("case_number")?,
        first_seen_at: r.try_get("first_seen_at")?,
        last_seen_at: r.try_get("last_seen_at")?,
        branches: branches.remove(&id).unwrap_or_default(),
    })
}

/// A stored proposal: `name` and `body` with the patch keys beside them.
pub fn proposal_of(v: Value) -> Result<CaseProposal, serde_json::Error> {
    let Value::Object(mut o) = v else {
        return Ok(CaseProposal::default());
    };
    let name = o
        .remove("name")
        .map(serde_json::from_value::<Option<String>>)
        .transpose()?
        .flatten();
    let body = o
        .remove("body")
        .map(serde_json::from_value::<Option<CaseBody>>)
        .transpose()?
        .flatten();
    let patch: CasePatch = serde_json::from_value(Value::Object(o))?;
    Ok(CaseProposal { patch, name, body })
}

fn suggestion_row(r: &PgRow) -> sqlx::Result<CaseSuggestion> {
    let status: String = r.try_get("status")?;
    let proposal: Value = r.try_get("proposal")?;
    Ok(CaseSuggestion {
        id: r.try_get("id")?,
        case_number: r.try_get("number")?,
        base_revision: r.try_get("base_revision")?,
        proposal: proposal_of(proposal).unwrap_or_default(),
        rationale: r.try_get("rationale")?,
        agent: r.try_get("agent")?,
        status: SuggestionStatus::parse(&status).unwrap_or(SuggestionStatus::Pending),
        created_at: r.try_get("created_at")?,
        decided_by: r.try_get("decided_by")?,
        decided_at: r.try_get("decided_at")?,
    })
}

const SUGGESTION_SELECT: &str = "select s.id, c.number, s.base_revision, s.proposal, s.rationale, s.agent, \
    s.status, s.created_at, s.decided_by, s.decided_at from case_suggestions s join cases c on c.id = s.case_id ";

/// Puts the case back into the folder tree when its folder is gone.
async fn existing_folder(
    conn: &mut PgConnection,
    project_id: Uuid,
    f: Option<Uuid>,
) -> sqlx::Result<Option<Uuid>> {
    match f {
        Some(id) => Ok(folder(conn, project_id, id).await?.map(|f| f.id)),
        None => Ok(None),
    }
}

/// A stored attachment body for [`Store::add_case_attachment`].
#[derive(Clone, Debug)]
pub struct CaseAttachmentUpload {
    pub sha256: String,
    pub size: i64,
    pub content_type: String,
    pub name: String,
    pub blob_key: String,
}

impl Store {
    /// Creates a case from the UI or API (`origin = ui`, version 1 with `source = ui`).
    pub async fn create_case(&self, ctx: &CaseCtx<'_>, w: &CaseWrite) -> TmResult<CaseRow> {
        let mut tx = self.pool.begin().await?;
        let defs = field_defs(&mut tx, ctx.project_id, false).await?;
        let snap = prepare_write(&mut tx, ctx, w, None, &defs).await?;
        let row = create_case_in(&mut tx, ctx, snap, &NewCaseMeta::ui()).await?;
        tx.commit().await?;
        Ok(row)
    }

    /// The API view of the case with number `n` (HTML empty).
    pub async fn case_view(&self, project_id: Uuid, n: i64) -> TmResult<Case> {
        let mut conn = self.pool.acquire().await?;
        let row = case_by_number(&mut conn, project_id, n)
            .await?
            .ok_or(TmError::NotFound("case"))?;
        case_view(&mut conn, project_id, &row).await
    }

    /// The API view of a case row.
    pub async fn case_view_of(&self, project_id: Uuid, row: &CaseRow) -> TmResult<Case> {
        let mut conn = self.pool.acquire().await?;
        case_view(&mut conn, project_id, row).await
    }

    /// Replaces the content of the case (`PUT`) when its revision is `expected`. Returns the
    /// case before and after.
    pub async fn update_case(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        expected: i64,
        w: &CaseWrite,
    ) -> TmResult<(CaseRow, CaseRow)> {
        let mut tx = self.pool.begin().await?;
        let cur = lock_case_by_number(&mut tx, ctx.project_id, n, Some(expected)).await?;
        let row = update_case_in(&mut tx, ctx, &cur, w, VersionSource::Ui).await?;
        tx.commit().await?;
        Ok((cur, row))
    }

    /// Merges a patch into the case (`PATCH`) when its revision is `expected`. Returns the
    /// case before and after.
    pub async fn patch_case(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        expected: i64,
        p: &CasePatch,
    ) -> TmResult<(CaseRow, CaseRow)> {
        let mut tx = self.pool.begin().await?;
        let cur = lock_case_by_number(&mut tx, ctx.project_id, n, Some(expected)).await?;
        let mut snap = snapshot_of(&cur);
        apply_patch(&mut snap, p);
        let w = write_of(&snap);
        let row = update_case_in(&mut tx, ctx, &cur, &w, VersionSource::Ui).await?;
        tx.commit().await?;
        Ok((cur, row))
    }

    /// Clears `ai_unreviewed` of a case whose revision is `expected` ("Reviewed"); `revision + 1`,
    /// no version (the content did not change). Returns the case before and after; a case that
    /// is already reviewed is returned unchanged.
    pub async fn mark_case_reviewed(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        expected: i64,
    ) -> TmResult<(CaseRow, CaseRow)> {
        let mut tx = self.pool.begin().await?;
        let cur = lock_case_by_number(&mut tx, ctx.project_id, n, Some(expected)).await?;
        if !cur.ai_unreviewed {
            return Ok((cur.clone(), cur));
        }
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update cases set ai_unreviewed = false, revision = revision + 1, updated_by = $3, \
             updated_at = now() where id = $1 and project_id = $2 returning {CASE_COLS}"
        )))
        .bind(cur.id)
        .bind(ctx.project_id)
        .bind(no_nul(ctx.by))
        .fetch_one(&mut *tx)
        .await?;
        let row = case_row(&r)?;
        case_event(
            &mut tx,
            ctx.project_key,
            "case.updated",
            json!({"number": row.number, "revision": row.revision, "source": "ui"}),
        )
        .await?;
        tx.commit().await?;
        Ok((cur, row))
    }

    /// Archives (`archive = true`) or restores a case; `revision + 1`, no version.
    pub async fn set_case_archived(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        archive: bool,
    ) -> TmResult<CaseRow> {
        let mut tx = self.pool.begin().await?;
        let cur = lock_case_by_number(&mut tx, ctx.project_id, n, None).await?;
        if cur.archived_at.is_some() == archive {
            return Ok(cur);
        }
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update cases set archived_at = case when $3 then now() end, revision = revision + 1, \
             updated_by = $4, updated_at = now() where id = $1 and project_id = $2 returning {CASE_COLS}"
        )))
        .bind(cur.id)
        .bind(ctx.project_id)
        .bind(archive)
        .bind(no_nul(ctx.by))
        .fetch_one(&mut *tx)
        .await?;
        let row = case_row(&r)?;
        if archive {
            case_event(
                &mut tx,
                ctx.project_key,
                "case.archived",
                json!({"number": row.number}),
            )
            .await?;
        } else {
            case_event(
                &mut tx,
                ctx.project_key,
                "case.updated",
                json!({"number": row.number, "revision": row.revision, "source": "ui"}),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(row)
    }

    /// Deletes a case with its versions, keys and attachment links (the attachments are
    /// collected later). Cases executed in manual runs are kept (409 `case_has_runs`): their
    /// run items are evidence.
    pub async fn purge_case(&self, ctx: &CaseCtx<'_>, n: i64) -> TmResult<CaseRow> {
        let mut tx = self.pool.begin().await?;
        let cur = lock_case_by_number(&mut tx, ctx.project_id, n, None).await?;
        let runs: bool =
            sqlx::query_scalar("select exists (select 1 from run_items where case_id = $1)")
                .bind(cur.id)
                .fetch_one(&mut *tx)
                .await?;
        if runs {
            return Err(TmError::conflict(
                "case_has_runs",
                "the case was executed in manual runs: archive it instead",
            ));
        }
        sqlx::query("delete from cases where id = $1 and project_id = $2")
            .bind(cur.id)
            .bind(ctx.project_id)
            .execute(&mut *tx)
            .await?;
        case_event(
            &mut tx,
            ctx.project_key,
            "case.archived",
            json!({"number": cur.number, "purged": true}),
        )
        .await?;
        tx.commit().await?;
        Ok(cur)
    }

    /// A page of cases.
    pub async fn list_cases(&self, project_id: Uuid, q: &CaseQuery) -> TmResult<CaseList> {
        check_filter(&q.filter)?;
        let mut conn = self.pool.acquire().await?;
        check_filter_folders(&mut conn, project_id, &q.filter).await?;
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(SUMMARY_SELECT);
        qb.push("where c.project_id = ").push_bind(project_id);
        qb.push(if q.archived {
            " and c.archived_at is not null"
        } else {
            " and c.archived_at is null"
        });
        push_filter(&mut qb, project_id, &q.filter);
        if let Some(d) = q.stale_days {
            qb.push(" and c.automated and (c.last_trusted_at is null or c.last_trusted_at < now() - make_interval(days => ")
                .push_bind(d.min(36_500) as i32)
                .push("))");
        }
        if let Some(a) = q.ai_unreviewed {
            qb.push(" and c.ai_unreviewed = ").push_bind(a);
        }
        let bad = || TmError::bad_request("invalid cursor");
        match q.sort {
            CaseSort::Number | CaseSort::NumberDesc => {
                if let Some(b) = &q.before {
                    let n: i64 = decode_cursor(b)
                        .and_then(|s| s.strip_prefix("n:").and_then(|n| n.parse().ok()))
                        .ok_or_else(bad)?;
                    qb.push(if q.sort == CaseSort::Number {
                        " and c.number > "
                    } else {
                        " and c.number < "
                    })
                    .push_bind(n);
                }
                qb.push(if q.sort == CaseSort::Number {
                    " order by c.number"
                } else {
                    " order by c.number desc"
                });
            }
            CaseSort::Updated => {
                if let Some(b) = &q.before {
                    let (t, id) = time_id_cursor(b).ok_or_else(bad)?;
                    qb.push(" and (c.updated_at, c.id) < (")
                        .push_bind(t)
                        .push(", ")
                        .push_bind(id)
                        .push(")");
                }
                qb.push(" order by c.updated_at desc, c.id desc");
            }
        }
        qb.push(" limit ").push_bind(q.limit + 1);
        let rows = qb.build().fetch_all(&mut *conn).await?;
        let more = rows.len() as i64 > q.limit;
        let mut items = Vec::with_capacity(rows.len());
        let mut last: Option<(DateTime<Utc>, Uuid)> = None;
        for r in rows.iter().take(q.limit as usize) {
            items.push(summary_row(r)?);
            last = Some((r.try_get("updated_at")?, r.try_get("id")?));
        }
        let next = if more {
            match q.sort {
                CaseSort::Updated => last.map(|(t, id)| time_id_encode(t, id)),
                _ => items
                    .last()
                    .map(|c| encode_cursor(&format!("n:{}", c.number))),
            }
        } else {
            None
        };
        Ok(CaseList { items, next })
    }

    /// One level of the case tree: folders (with subtree counts), behaviors (epic → feature →
    /// story field values) or suites (`suite` field values). Cases without the value of a
    /// level do not appear in behavior and suite trees.
    pub async fn case_tree(
        &self,
        project_id: Uuid,
        group: CaseTreeGroupBy,
        parent: Option<&str>,
    ) -> TmResult<CaseTree> {
        let mut conn = self.pool.acquire().await?;
        if group == CaseTreeGroupBy::Folder {
            let parent = match parent.filter(|p| !p.is_empty()) {
                Some(p) => Some(p.parse::<Uuid>().map_err(|_| TmError::NotFound("folder"))?),
                None => None,
            };
            let nodes = folder_level(&mut conn, project_id, parent)
                .await?
                .into_iter()
                .map(|f| CaseTreeNode {
                    id: f.id.to_string(),
                    name: f.name,
                    counts: f.counts,
                    has_children: f.has_children,
                })
                .collect();
            return Ok(CaseTree { nodes });
        }
        let levels: &[&str] = match group {
            CaseTreeGroupBy::Behavior => &["epic", "feature", "story"],
            _ => &["suite"],
        };
        let path: Vec<String> = match parent.filter(|p| !p.is_empty()) {
            Some(p) => p.split(TREE_ID_SEPARATOR).map(str::to_string).collect(),
            None => Vec::new(),
        };
        if path.len() >= levels.len() || path.iter().any(|s| s.len() > 1000) {
            return Ok(CaseTree { nodes: Vec::new() });
        }
        let key = levels[path.len()];
        let next = levels.get(path.len() + 1).copied();
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("select c.fields ->> ");
        qb.push_bind(key)
            .push(" as v, count(*) as total, count(*) filter (where c.automated) as automated, ");
        match next {
            Some(n) => {
                qb.push("coalesce(bool_or(jsonb_typeof(c.fields -> ")
                    .push_bind(n)
                    .push(") = 'string'), false) as more ");
            }
            None => {
                qb.push("false as more ");
            }
        }
        qb.push("from cases c where c.project_id = ")
            .push_bind(project_id)
            .push(" and c.archived_at is null and jsonb_typeof(c.fields -> ")
            .push_bind(key)
            .push(") = 'string'");
        for (i, v) in path.iter().enumerate() {
            qb.push(" and c.fields ->> ")
                .push_bind(levels[i])
                .push(" = ")
                .push_bind(no_nul(v));
        }
        qb.push(" group by 1 order by 1 limit ")
            .push_bind(MAX_TREE_NODES);
        let rows: Vec<(String, i64, i64, bool)> = qb.build_query_as().fetch_all(&mut *conn).await?;
        let sep = TREE_ID_SEPARATOR.to_string();
        let nodes = rows
            .into_iter()
            .map(|(v, total, automated, more)| {
                let mut id = path.clone();
                id.push(v.clone());
                CaseTreeNode {
                    id: id.join(&sep),
                    name: v,
                    counts: FolderCounts {
                        total,
                        manual: total - automated,
                        automated,
                    },
                    has_children: more,
                }
            })
            .collect();
        Ok(CaseTree { nodes })
    }

    /// Applies a bulk action to the cases named by number or matched by a filter (at most
    /// `maxBulk`). Each changed case gets one version (`source = bulk`, archive and restore
    /// change no content and only bump the revision); cases left alone are listed with a
    /// code (`not_found`, `unchanged`, `field_from_code`, `invalid_case`, `archived`).
    pub async fn bulk_cases(&self, ctx: &CaseCtx<'_>, b: &CaseBulk) -> TmResult<CaseBulkResult> {
        let l = ctx.limits;
        let op = bulk_op(b, l)?;
        let mut tx = self.pool.begin().await?;
        let numbers: Vec<i64> = match (&b.numbers, &b.filter) {
            (Some(n), None) => {
                if n.len() > l.max_bulk {
                    return Err(TmError::limit("maxBulk", l.max_bulk as u64));
                }
                let mut n = n.clone();
                n.sort_unstable();
                n.dedup();
                n
            }
            (None, Some(f)) => {
                let archived = match op {
                    BulkOp::Restore => Some(true),
                    _ => Some(false),
                };
                let n = filter_numbers(&mut tx, ctx.project_id, f, archived, l.max_bulk).await?;
                if n.len() > l.max_bulk {
                    return Err(TmError::limit("maxBulk", l.max_bulk as u64));
                }
                n
            }
            _ => {
                return Err(TmError::bad_request("give either numbers or a filter"));
            }
        };
        if let BulkOp::Move(Some(f)) = &op
            && folder(&mut tx, ctx.project_id, *f).await?.is_none()
        {
            return Err(TmError::NotFound("folder"));
        }
        let defs = field_defs(&mut tx, ctx.project_id, false).await?;
        if let BulkOp::Field(k, v) = &op {
            let def = defs
                .iter()
                .find(|d| d.key == *k && !d.archived)
                .ok_or_else(|| invalid_case(&format!("value.{k}"), "unknown field"))?;
            check_value(def, v, l).map_err(|m| invalid_case(&format!("value.{k}"), m))?;
        }
        let settings = case_sync_settings(&mut tx, ctx.project_id).await?;
        let owned = owned_keys(&settings, &defs);
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {CASE_COLS} from cases where project_id = $1 and number = any($2) order by id for update"
        )))
        .bind(ctx.project_id)
        .bind(&numbers)
        .fetch_all(&mut *tx)
        .await?;
        let mut by_number: HashMap<i64, CaseRow> = HashMap::with_capacity(rows.len());
        for r in &rows {
            let c = case_row(r)?;
            by_number.insert(c.number, c);
        }
        let mut out = CaseBulkResult::default();
        let skip = |out: &mut CaseBulkResult, n: i64, code: &str| {
            out.skipped.push(CaseBulkSkip {
                number: n,
                code: code.to_string(),
            })
        };
        let mut content: Vec<(CaseRow, CaseSnapshot, Vec<String>)> = Vec::new();
        let mut archive_ids: Vec<Uuid> = Vec::new();
        for n in &numbers {
            let Some(cur) = by_number.get(n) else {
                skip(&mut out, *n, "not_found");
                continue;
            };
            match &op {
                BulkOp::Archive | BulkOp::Restore => {
                    let archive = matches!(op, BulkOp::Archive);
                    if cur.archived_at.is_some() == archive {
                        skip(&mut out, *n, "unchanged");
                    } else {
                        archive_ids.push(cur.id);
                    }
                    continue;
                }
                _ => {}
            }
            if cur.archived_at.is_some() {
                skip(&mut out, *n, "archived");
                continue;
            }
            let mut snap = snapshot_of(cur);
            match &op {
                BulkOp::Move(f) => snap.folder_id = *f,
                BulkOp::Status(s) => snap.status = *s,
                BulkOp::Field(k, v) => {
                    if v.is_null() {
                        snap.fields.remove(k);
                    } else {
                        snap.fields.insert(k.clone(), v.clone());
                    }
                }
                BulkOp::AddTags(t) => {
                    for tag in t {
                        if !snap.tags.contains(tag) {
                            snap.tags.push(tag.clone());
                        }
                    }
                }
                BulkOp::RemoveTags(t) => snap.tags.retain(|x| !t.contains(x)),
                BulkOp::Archive | BulkOp::Restore => unreachable!(),
            }
            if snap.tags.len() > l.max_tags {
                skip(&mut out, *n, "limit_reached");
                continue;
            }
            if snap.status != CaseStatus::Draft
                && defs.iter().any(|d| {
                    !d.archived
                        && d.required
                        && !snap.fields.contains_key(&d.key)
                        && (cur.status == CaseStatus::Draft || cur.fields.contains_key(&d.key))
                })
            {
                skip(&mut out, *n, "invalid_case");
                continue;
            }
            if !code_conflicts(cur, &snap, &owned).is_empty() {
                skip(&mut out, *n, "field_from_code");
                continue;
            }
            let changed = changed_keys(&snapshot_of(cur), &snap);
            if changed.is_empty() {
                skip(&mut out, *n, "unchanged");
                continue;
            }
            content.push((cur.clone(), snap, changed));
        }
        let mut notify_items: Vec<Value> = Vec::new();
        for chunk in content.chunks(1000) {
            let mut ids = Vec::with_capacity(chunk.len());
            let mut folders = Vec::with_capacity(chunk.len());
            let mut statuses = Vec::with_capacity(chunk.len());
            let mut fields = Vec::with_capacity(chunk.len());
            let mut tags = Vec::with_capacity(chunk.len());
            let mut versions = Vec::with_capacity(chunk.len());
            let mut changed = Vec::with_capacity(chunk.len());
            let mut snaps = Vec::with_capacity(chunk.len());
            for (cur, snap, ch) in chunk {
                ids.push(cur.id);
                folders.push(snap.folder_id);
                statuses.push(snap.status.as_str().to_string());
                fields.push(serde_json::to_value(&snap.fields)?);
                tags.push(serde_json::to_value(&snap.tags)?);
                versions.push(cur.version + 1);
                changed.push(serde_json::to_value(ch)?);
                snaps.push(crate::codec::encode(snap)?);
            }
            let updated: Vec<(i64, i64)> = sqlx::query_as(
                "update cases c set folder_id = t.f, status = t.s, fields = t.fl, \
                   tags = array(select jsonb_array_elements_text(t.tg)), revision = c.revision + 1, \
                   version = t.v, updated_by = $2, updated_at = now() \
                 from unnest($3::uuid[], $4::uuid[], $5::text[], $6::jsonb[], $7::jsonb[], $8::int[]) \
                   as t(id, f, s, fl, tg, v) \
                 where c.project_id = $1 and c.id = t.id returning c.number, c.revision",
            )
            .bind(ctx.project_id)
            .bind(no_nul(ctx.by))
            .bind(&ids)
            .bind(&folders)
            .bind(&statuses)
            .bind(&fields)
            .bind(&tags)
            .bind(&versions)
            .fetch_all(&mut *tx)
            .await?;
            sqlx::query(
                "insert into case_versions (case_id, version, project_id, source, changed, snapshot_z, created_by) \
                 select t.id, t.v, $1, 'bulk', array(select jsonb_array_elements_text(t.ch)), t.z, $2 \
                 from unnest($3::uuid[], $4::int[], $5::jsonb[], $6::bytea[]) as t(id, v, ch, z)",
            )
            .bind(ctx.project_id)
            .bind(no_nul(ctx.by))
            .bind(&ids)
            .bind(&versions)
            .bind(&changed)
            .bind(&snaps)
            .execute(&mut *tx)
            .await?;
            out.updated += updated.len() as i64;
            notify_items.extend(
                updated
                    .into_iter()
                    .map(|(n, r)| json!({"number": n, "revision": r, "source": "bulk"})),
            );
        }
        if !archive_ids.is_empty() {
            let archive = matches!(op, BulkOp::Archive);
            let updated: Vec<(i64, i64)> = sqlx::query_as(
                "update cases set archived_at = case when $3 then now() end, revision = revision + 1, \
                 updated_by = $4, updated_at = now() where project_id = $1 and id = any($2) returning number, revision",
            )
            .bind(ctx.project_id)
            .bind(&archive_ids)
            .bind(archive)
            .bind(no_nul(ctx.by))
            .fetch_all(&mut *tx)
            .await?;
            out.updated += updated.len() as i64;
            notify_items.extend(updated.into_iter().map(|(n, r)| {
                if archive {
                    json!({"number": n})
                } else {
                    json!({"number": n, "revision": r, "source": "bulk"})
                }
            }));
        }
        let kind = if matches!(op, BulkOp::Archive) {
            "case.archived"
        } else {
            "case.updated"
        };
        crate::events::emit_many(&mut tx, kind, Some(ctx.project_key), notify_items.clone())
            .await?;
        if notify_items.len() <= MAX_BULK_NOTIFY {
            for d in notify_items {
                notify(
                    &mut tx,
                    &LiveEvent {
                        project: ctx.project_key.to_string(),
                        kind: kind.to_string(),
                        data: d,
                    },
                )
                .await?;
            }
        } else {
            notify(
                &mut tx,
                &LiveEvent {
                    project: ctx.project_key.to_string(),
                    kind: "case.updated".to_string(),
                    data: json!({"source": "bulk", "count": notify_items.len()}),
                },
            )
            .await?;
        }
        tx.commit().await?;
        Ok(out)
    }

    /// Links a code identity of the project to the case. When it was the last code identity
    /// of another case, that case is archived with a link to this one (a rename or merge).
    /// The case becomes automated. Returns the case and the archived duplicate's number.
    pub async fn link_case_identity(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        test_case_id: Uuid,
    ) -> TmResult<(CaseRow, Option<i64>)> {
        let mut tx = self.pool.begin().await?;
        let ident: Option<(String, Option<Uuid>)> = sqlx::query_as(
            "select kind, case_id from test_cases where project_id = $1 and id = $2 for update",
        )
        .bind(ctx.project_id)
        .bind(test_case_id)
        .fetch_optional(&mut *tx)
        .await?;
        let (kind, old_case) = ident.ok_or(TmError::NotFound("identity"))?;
        if kind != "code" {
            return Err(TmError::bad_request(
                "manual identities belong to their case",
            ));
        }
        let target = lock_case_by_number(&mut tx, ctx.project_id, n, None).await?;
        if old_case == Some(target.id) {
            return Ok((target, None));
        }
        let mut dup_number = None;
        if let Some(old) = old_case {
            let old_row = sqlx::query(sqlx::AssertSqlSafe(format!(
                "select {CASE_COLS} from cases where project_id = $1 and id = $2 for update"
            )))
            .bind(ctx.project_id)
            .bind(old)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(r) = old_row {
                let old_row = case_row(&r)?;
                sqlx::query("update test_cases set case_id = null where id = $1")
                    .bind(test_case_id)
                    .execute(&mut *tx)
                    .await?;
                let others: bool = sqlx::query_scalar(
                    "select exists (select 1 from test_cases where case_id = $1 and kind = 'code')",
                )
                .bind(old_row.id)
                .fetch_one(&mut *tx)
                .await?;
                if others {
                    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
                        "update cases set revision = revision + 1, updated_at = now(), updated_by = $2 \
                         where id = $1 returning {CASE_COLS}"
                    )))
                    .bind(old_row.id)
                    .bind(no_nul(ctx.by))
                    .fetch_one(&mut *tx)
                    .await?;
                    let row = case_row(&r)?;
                    case_event(
                        &mut tx,
                        ctx.project_key,
                        "case.updated",
                        json!({"number": row.number, "revision": row.revision, "source": "ui"}),
                    )
                    .await?;
                } else {
                    let mut snap = snapshot_of(&old_row);
                    let link = CaseLink {
                        name: format!("Merged into TC-{}", target.number),
                        url: format!("/p/{}/cases/{}", ctx.project_key, target.number),
                        link_type: super::CaseLinkType::Link,
                    };
                    if snap.body.links.len() < MAX_LINKS && !snap.body.links.contains(&link) {
                        snap.body.links.push(link);
                    }
                    let row = commit(
                        &mut tx,
                        ctx,
                        &old_row,
                        snap,
                        Commit {
                            source: VersionSource::Ui,
                            launch_id: None,
                            force_revision: true,
                        },
                    )
                    .await?;
                    sqlx::query(
                        "update cases set automated = false, archived_at = coalesce(archived_at, now()), \
                         revision = revision + 1 where id = $1",
                    )
                    .bind(row.id)
                    .execute(&mut *tx)
                    .await?;
                    case_event(
                        &mut tx,
                        ctx.project_key,
                        "case.archived",
                        json!({"number": row.number}),
                    )
                    .await?;
                    dup_number = Some(row.number);
                }
            }
        }
        sqlx::query("update test_cases set case_id = $2 where id = $1")
            .bind(test_case_id)
            .bind(target.id)
            .execute(&mut *tx)
            .await?;
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update cases set automated = true, revision = revision + 1, updated_at = now(), updated_by = $2 \
             where id = $1 returning {CASE_COLS}"
        )))
        .bind(target.id)
        .bind(no_nul(ctx.by))
        .fetch_one(&mut *tx)
        .await?;
        let row = case_row(&r)?;
        case_event(
            &mut tx,
            ctx.project_key,
            "case.updated",
            json!({"number": row.number, "revision": row.revision, "source": "ui"}),
        )
        .await?;
        tx.commit().await?;
        Ok((row, dup_number))
    }

    /// Unlinks a code identity from the case; the case stays automated while it has another
    /// code identity.
    pub async fn unlink_case_identity(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        test_case_id: Uuid,
    ) -> TmResult<CaseRow> {
        let mut tx = self.pool.begin().await?;
        let ident: Option<(String, Option<Uuid>)> = sqlx::query_as(
            "select kind, case_id from test_cases where project_id = $1 and id = $2 for update",
        )
        .bind(ctx.project_id)
        .bind(test_case_id)
        .fetch_optional(&mut *tx)
        .await?;
        let cur = lock_case_by_number(&mut tx, ctx.project_id, n, None).await?;
        let Some((kind, Some(linked))) = ident else {
            return Err(TmError::NotFound("identity"));
        };
        if linked != cur.id {
            return Err(TmError::NotFound("identity"));
        }
        if kind != "code" {
            return Err(TmError::bad_request(
                "the manual identity of a case cannot be unlinked",
            ));
        }
        sqlx::query("update test_cases set case_id = null where id = $1")
            .bind(test_case_id)
            .execute(&mut *tx)
            .await?;
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update cases set automated = exists (select 1 from test_cases t where t.case_id = $1 and t.kind = 'code'), \
             revision = revision + 1, updated_at = now(), updated_by = $2 where id = $1 returning {CASE_COLS}"
        )))
        .bind(cur.id)
        .bind(no_nul(ctx.by))
        .fetch_one(&mut *tx)
        .await?;
        let row = case_row(&r)?;
        case_event(
            &mut tx,
            ctx.project_key,
            "case.updated",
            json!({"number": row.number, "revision": row.revision, "source": "ui"}),
        )
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    /// Code identities of the project, newest seen first; `unlinked` keeps those without a
    /// case (feature branches, untrusted launches, new tests).
    pub async fn test_identities(
        &self,
        project_id: Uuid,
        q: &IdentityQuery,
    ) -> TmResult<(Vec<TestIdentity>, Option<String>)> {
        let mut conn = self.pool.acquire().await?;
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "select t.id, t.full_name, t.display_name, t.kind, t.first_seen_at, t.last_seen_at, \
             c.number as case_number from test_cases t left join cases c on c.id = t.case_id \
             where t.kind = 'code' and t.project_id = ",
        );
        qb.push_bind(project_id);
        if q.unlinked {
            qb.push(" and t.case_id is null");
        }
        if let Some(s) = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            if s.len() > MAX_Q_BYTES {
                return Err(TmError::bad_request(format!(
                    "q has at most {MAX_Q_BYTES} bytes"
                )));
            }
            let like = format!("%{}%", like_escape(&no_nul(s)));
            qb.push(" and (t.full_name ilike ")
                .push_bind(like.clone())
                .push(" or t.display_name ilike ")
                .push_bind(like)
                .push(")");
        }
        if let Some(b) = q.branch.as_deref().filter(|b| !b.is_empty()) {
            let b = no_nul(b);
            qb.push(
                " and (exists (select 1 from results r join launches l on l.id = r.launch_id \
                 where r.test_case_id = t.id and l.project_id = ",
            )
            .push_bind(project_id)
            .push(" and l.branch = ")
            .push_bind(b.clone())
            .push(
                ") or exists (select 1 from series s join series_daily d on d.series_id = s.id \
                 where s.test_case_id = t.id and d.branch = ",
            )
            .push_bind(b)
            .push("))");
        }
        if let Some(c) = &q.before {
            let (t, id) =
                time_id_cursor(c).ok_or_else(|| TmError::bad_request("invalid cursor"))?;
            qb.push(" and (t.last_seen_at, t.id) < (")
                .push_bind(t)
                .push(", ")
                .push_bind(id)
                .push(")");
        }
        qb.push(" order by t.last_seen_at desc, t.id desc limit ")
            .push_bind(q.limit + 1);
        let rows = qb.build().fetch_all(&mut *conn).await?;
        let more = rows.len() as i64 > q.limit;
        let rows: Vec<&PgRow> = rows.iter().take(q.limit as usize).collect();
        let ids: Vec<Uuid> = rows
            .iter()
            .map(|r| r.try_get("id"))
            .collect::<sqlx::Result<_>>()?;
        let mut branches = identity_branches(&mut conn, project_id, &ids).await?;
        let items: Vec<TestIdentity> = rows
            .into_iter()
            .map(|r| ident_row(r, &mut branches))
            .collect::<sqlx::Result<_>>()?;
        let next = more
            .then(|| {
                items
                    .last()
                    .map(|i| time_id_encode(i.last_seen_at, i.test_case_id))
            })
            .flatten();
        Ok((items, next))
    }

    /// Results of the case over all its identities (manual and automated), newest first.
    pub async fn case_history(
        &self,
        project_id: Uuid,
        n: i64,
        q: &HistoryQuery,
    ) -> TmResult<(Vec<CaseHistoryEntry>, Option<String>)> {
        let mut conn = self.pool.acquire().await?;
        let case = case_by_number(&mut conn, project_id, n)
            .await?
            .ok_or(TmError::NotFound("case"))?;
        let ids: Vec<Uuid> =
            sqlx::query_scalar("select id from test_cases where project_id = $1 and case_id = $2")
                .bind(project_id)
                .bind(case.id)
                .fetch_all(&mut *conn)
                .await?;
        if ids.is_empty() {
            return Ok((Vec::new(), None));
        }
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "select r.id, r.launch_id, r.launch_created_at, l.number, l.name, r.status, r.run_item_id is not null as manual, \
             coalesce(r.stopped_at, r.updated_at) as at, l.branch, l.env_key, \
             (select e ->> 'value' from jsonb_array_elements(r.labels) e where e ->> 'name' = 'executedBy' limit 1) as executed_by, \
             (select e ->> 'value' from jsonb_array_elements(r.labels) e where e ->> 'name' = 'manualStatus' limit 1) as manual_status \
             from results r join launches l on l.id = r.launch_id where r.project_id = ",
        );
        qb.push_bind(project_id)
            .push(" and r.test_case_id = any(")
            .push_bind(ids)
            .push(")");
        if let Some(b) = q.branch.as_deref().filter(|b| !b.is_empty()) {
            qb.push(" and l.branch = ").push_bind(no_nul(b));
        }
        if let Some(e) = q.environment.as_deref().filter(|e| !e.is_empty()) {
            qb.push(" and l.env_key = ").push_bind(no_nul(e));
        }
        match q.source {
            Some(ResultSource::Manual) => {
                qb.push(" and r.run_item_id is not null");
            }
            Some(ResultSource::Automated) => {
                qb.push(" and r.run_item_id is null");
            }
            None => {}
        }
        if let Some(c) = &q.before {
            let (t, id) =
                time_id_cursor(c).ok_or_else(|| TmError::bad_request("invalid cursor"))?;
            qb.push(" and (r.launch_created_at, r.id) < (")
                .push_bind(t)
                .push(", ")
                .push_bind(id)
                .push(")");
        }
        qb.push(" order by r.launch_created_at desc, r.id desc limit ")
            .push_bind(q.limit + 1);
        let rows = qb.build().fetch_all(&mut *conn).await?;
        let more = rows.len() as i64 > q.limit;
        let mut items = Vec::new();
        let mut last = None;
        for r in rows.iter().take(q.limit as usize) {
            let manual: bool = r.try_get("manual")?;
            let status: String = r.try_get("status")?;
            let manual_status: Option<String> = r.try_get("manual_status")?;
            last = Some((
                r.try_get::<DateTime<Utc>, _>("launch_created_at")?,
                r.try_get::<Uuid, _>("id")?,
            ));
            items.push(CaseHistoryEntry {
                launch_id: r.try_get("launch_id")?,
                launch_number: r.try_get("number")?,
                launch_name: r.try_get("name")?,
                result_id: r.try_get("id")?,
                status: if manual {
                    manual_status.unwrap_or(status)
                } else {
                    status
                },
                source: if manual {
                    ResultSource::Manual
                } else {
                    ResultSource::Automated
                },
                at: r.try_get("at")?,
                branch: r.try_get("branch")?,
                environment: r.try_get("env_key")?,
                executed_by: r.try_get("executed_by")?,
            });
        }
        let next = if more {
            last.map(|(t, id)| time_id_encode(t, id))
        } else {
            None
        };
        Ok((items, next))
    }

    /// Stores a case attachment whose body is already in blob storage at `blob_key`
    /// (content-addressed per project): the `attachments` row (shared by hash), the
    /// `case_attachments` link, `maxCaseAttachments`.
    pub async fn add_case_attachment(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        a: &CaseAttachmentUpload,
    ) -> TmResult<AttachmentRef> {
        let mut tx = self.pool.begin().await?;
        let cur = lock_case_by_number(&mut tx, ctx.project_id, n, None).await?;
        let count: i64 =
            sqlx::query_scalar("select count(*) from case_attachments where case_id = $1")
                .bind(cur.id)
                .fetch_one(&mut *tx)
                .await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into attachments (id, project_id, sha256, size, content_type, original_name, blob_key) \
             values ($1, $2, $3, $4, $5, $6, $7) \
             on conflict (project_id, sha256) do update set last_seen_at = now() returning id",
        )
        .bind(Uuid::now_v7())
        .bind(ctx.project_id)
        .bind(&a.sha256)
        .bind(a.size)
        .bind(no_nul(&a.content_type))
        .bind(no_nul(&a.name))
        .bind(&a.blob_key)
        .fetch_one(&mut *tx)
        .await?;
        let added = sqlx::query(
            "insert into case_attachments (case_id, attachment_id, name, content_type, created_by) \
             values ($1, $2, $3, $4, $5) on conflict (case_id, attachment_id) do nothing",
        )
        .bind(cur.id)
        .bind(id)
        .bind(no_nul(&a.name))
        .bind(no_nul(&a.content_type))
        .bind(no_nul(ctx.by))
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if added > 0 && count as usize >= ctx.limits.max_case_attachments {
            return Err(TmError::limit(
                "maxCaseAttachments",
                ctx.limits.max_case_attachments as u64,
            ));
        }
        tx.commit().await?;
        Ok(AttachmentRef {
            id,
            sha256: a.sha256.clone(),
            size: a.size,
            content_type: a.content_type.clone(),
            name: a.name.clone(),
        })
    }

    /// The attachments of a case.
    pub async fn case_attachments(&self, project_id: Uuid, n: i64) -> TmResult<Vec<AttachmentRef>> {
        let mut conn = self.pool.acquire().await?;
        let case = case_by_number(&mut conn, project_id, n)
            .await?
            .ok_or(TmError::NotFound("case"))?;
        let rows: Vec<(Uuid, String, i64, String, String)> = sqlx::query_as(
            "select a.id, a.sha256, a.size, c.content_type, c.name from case_attachments c \
             join attachments a on a.id = c.attachment_id where c.case_id = $1 order by c.created_at limit 10000",
        )
        .bind(case.id)
        .fetch_all(&mut *conn)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, sha256, size, content_type, name)| AttachmentRef {
                id,
                sha256,
                size,
                content_type,
                name,
            })
            .collect())
    }

    /// Suggestions of a case, newest first.
    pub async fn case_suggestions(
        &self,
        project_id: Uuid,
        n: i64,
        status: Option<SuggestionStatus>,
        limit: i64,
    ) -> TmResult<Vec<CaseSuggestion>> {
        let mut conn = self.pool.acquire().await?;
        let case = case_by_number(&mut conn, project_id, n)
            .await?
            .ok_or(TmError::NotFound("case"))?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{SUGGESTION_SELECT} where s.case_id = $1 and s.project_id = $2 \
             and ($3::text is null or s.status = $3) order by s.created_at desc limit $4"
        )))
        .bind(case.id)
        .bind(project_id)
        .bind(status.map(|s| s.as_str()))
        .bind(limit)
        .fetch_all(&mut *conn)
        .await?;
        Ok(rows
            .iter()
            .map(suggestion_row)
            .collect::<sqlx::Result<_>>()?)
    }

    /// Accepts a pending suggestion: the case must still be at the suggestion's base revision
    /// (else it turns `stale`, 409 `suggestion_stale`) and at `expected` (412). The proposal is
    /// validated like a write (code-owned values included) and stored as a version with
    /// `source = ai`.
    pub async fn accept_case_suggestion(
        &self,
        ctx: &CaseCtx<'_>,
        id: Uuid,
        expected: i64,
    ) -> TmResult<(CaseSuggestion, CaseRow)> {
        let mut tx = self.pool.begin().await?;
        let s: Option<(Uuid, i64, Value, String)> = sqlx::query_as(
            "select case_id, base_revision, proposal, status from case_suggestions \
             where id = $1 and project_id = $2 for update",
        )
        .bind(id)
        .bind(ctx.project_id)
        .fetch_optional(&mut *tx)
        .await?;
        let (case_id, base, proposal, status) = s.ok_or(TmError::NotFound("suggestion"))?;
        let cur = case_by_id(&mut tx, ctx.project_id, case_id)
            .await?
            .ok_or(TmError::NotFound("suggestion"))?;
        let cur = lock_case_by_number(&mut tx, ctx.project_id, cur.number, Some(expected)).await?;
        if status != "pending" {
            return Err(TmError::conflict(
                "suggestion_decided",
                format!("the suggestion is {status}"),
            ));
        }
        if base != cur.revision {
            sqlx::query(
                "update case_suggestions set status = 'stale', decided_at = now(), decided_by = $2 where id = $1",
            )
            .bind(id)
            .bind(no_nul(ctx.by))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Err(TmError::conflict(
                "suggestion_stale",
                "the case changed after the suggestion was made",
            ));
        }
        let proposal: CaseProposal = proposal_of(proposal)
            .map_err(|e| TmError::bad_request(format!("stored proposal is invalid: {e}")))?;
        let mut snap = snapshot_of(&cur);
        apply_patch(&mut snap, &proposal.patch);
        if let Some(n) = &proposal.name {
            snap.name = n.clone();
        }
        if let Some(b) = &proposal.body {
            snap.body = b.clone();
        }
        snap.folder_id = existing_folder(&mut tx, ctx.project_id, snap.folder_id).await?;
        let row = update_case_in(&mut tx, ctx, &cur, &write_of(&snap), VersionSource::Ai).await?;
        let r = sqlx::query(
            "update case_suggestions s set status = 'accepted', decided_by = $2, decided_at = now() \
             from cases c where s.id = $1 and c.id = s.case_id returning s.id, c.number, s.base_revision, \
             s.proposal, s.rationale, s.agent, s.status, s.created_at, s.decided_by, s.decided_at"
        )
        .bind(id)
        .bind(no_nul(ctx.by))
        .fetch_one(&mut *tx)
        .await?;
        let sug = suggestion_row(&r)?;
        tx.commit().await?;
        Ok((sug, row))
    }

    /// Rejects a pending suggestion.
    pub async fn reject_case_suggestion(
        &self,
        ctx: &CaseCtx<'_>,
        id: Uuid,
    ) -> TmResult<CaseSuggestion> {
        let mut tx = self.pool.begin().await?;
        let status: Option<String> = sqlx::query_scalar(
            "select status from case_suggestions where id = $1 and project_id = $2 for update",
        )
        .bind(id)
        .bind(ctx.project_id)
        .fetch_optional(&mut *tx)
        .await?;
        let status = status.ok_or(TmError::NotFound("suggestion"))?;
        if status != "pending" {
            return Err(TmError::conflict(
                "suggestion_decided",
                format!("the suggestion is {status}"),
            ));
        }
        let r = sqlx::query(
            "update case_suggestions s set status = 'rejected', decided_by = $2, decided_at = now() \
             from cases c where s.id = $1 and c.id = s.case_id returning s.id, c.number, s.base_revision, \
             s.proposal, s.rationale, s.agent, s.status, s.created_at, s.decided_by, s.decided_at",
        )
        .bind(id)
        .bind(no_nul(ctx.by))
        .fetch_one(&mut *tx)
        .await?;
        let sug = suggestion_row(&r)?;
        tx.commit().await?;
        Ok(sug)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_ids_and_search_text() {
        assert!(valid_step_id(&new_step_id()));
        assert!(valid_step_id("s_a-b_C9"));
        assert!(!valid_step_id("s_"));
        assert!(!valid_step_id("x_abc"));
        assert!(!valid_step_id("s_a b"));
        assert!(!valid_step_id(&format!("s_{}", "a".repeat(63))));
        let body = CaseBody {
            description: "**Bold** _it_ `code`".into(),
            preconditions: "Logged in".into(),
            steps: vec![CaseStep {
                action: "Open [cart](https://example.com)".into(),
                ..Default::default()
            }],
            links: vec![],
        };
        assert_eq!(
            search_text("Pay", &body),
            "Pay Bold it code Logged in Open cart https://example.com"
        );
        let big = CaseBody {
            description: "é".repeat(MAX_SEARCH_TEXT),
            ..Default::default()
        };
        let t = search_text("x", &big);
        assert!(t.len() <= MAX_SEARCH_TEXT);
    }

    #[test]
    fn links_and_attachment_references() {
        assert!(valid_link_url("https://example.com/a"));
        assert!(valid_link_url("mailto:qa@example.com"));
        assert!(valid_link_url("/p/shop/cases/4"));
        assert!(!valid_link_url("javascript:alert(1)"));
        assert!(!valid_link_url("//evil.example.com"));
        assert!(!valid_link_url("/p//evil.example.com"));
        assert!(!valid_link_url("https://exa mple.com"));
        let id = Uuid::now_v7();
        let other = Uuid::now_v7();
        let body = CaseBody {
            description: format!("![x](attachment:{id}) and attachment:{id}"),
            steps: vec![CaseStep {
                attachments: vec![other],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(referenced_attachments(&body), vec![other, id]);
    }

    #[test]
    fn code_owned_conflicts_and_keep() {
        let mut s = CaseSyncSettings::default();
        let defs = vec![CaseFieldDef {
            id: Uuid::nil(),
            key: "severity".into(),
            name: "Severity".into(),
            field_type: super::super::FieldType::Text,
            options: vec![],
            required: false,
            default_value: FieldValue::Null,
            source_label: Some("severity".into()),
            built_in: true,
            position: 0,
            archived: false,
        }];
        assert_eq!(
            owned_keys(&s, &defs),
            vec!["name", "description", "links", "tags", "severity"]
        );
        s.fields.name = FieldPolicy::Case;
        s.fields.severity = FieldPolicy::Case;
        assert_eq!(owned_keys(&s, &defs), vec!["description", "links", "tags"]);
    }
}
