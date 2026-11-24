//! Shared steps: CRUD, versions, usages, inline and extract (`74-tm-next-interfaces.md` §7.1).
//!
//! A case references a shared step by id without a version; runs pin the runnable version
//! (`expand.rs`). Each save of a shared step is a version (zstd snapshot, like `versions.rs`)
//! and never touches case versions. `shared_step_usages` lists the cases whose head or
//! runnable version references a shared step.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use base64::Engine;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::cases::{
    CaseCtx, MAX_Q_BYTES, case_event, invalid_case, lock_case_by_number, new_step_id, search_text,
    update_case_in, valid_step_id,
};
use super::lookup::{
    CASE_COLS, SHARED_STEP_COLS, case_by_id, case_row, case_version_snapshot, shared_step_by_id,
    shared_step_by_number, shared_step_row,
};
use super::{
    CaseBody, CaseReviewSettings, CaseRow, CaseStep, CaseWrite, ExtractSharedStep, ImportReport,
    ReviewMode, SharedStep, SharedStepBrief, SharedStepChange, SharedStepChangeList,
    SharedStepList, SharedStepOrigin, SharedStepRef, SharedStepRow, SharedStepSnapshot,
    SharedStepSummary, SharedStepUsage, SharedStepUsageCount, SharedStepUsageList,
    SharedStepVersion, SharedStepVersionList, SharedStepVersionSummary, SharedStepWrite, TmError,
    TmJob, TmJobKind, TmJobProgress, TmJobStatus, TmLimits, TmResult, VersionSource, check_name,
};
use crate::launches::like_escape;
use crate::{Store, no_nul};

/// A shared step with more usages than this is inlined by a job (`shared_inline`).
pub const INLINE_SYNC_MAX: i64 = 1000;
/// Cases one inline batch of the job takes at a time.
const INLINE_BATCH: i64 = 200;

/// Content keys of a shared step version, in display order.
pub const KEYS: &[&str] = &["name", "description", "steps"];

/// Filters of [`Store::list_shared_steps`].
#[derive(Clone, Debug, Default)]
pub struct SharedStepQuery {
    /// A number (`SS-12` or `12`), or words of the name, description or steps.
    pub q: Option<String>,
    /// `true` lists the archived ones.
    pub archived: bool,
    /// The cursor of the previous page (`next`).
    pub before: Option<String>,
    pub limit: i64,
}

/// What `POST /shared-steps/{n}/inline` did.
#[derive(Clone, Debug)]
pub enum InlineOutcome {
    /// Done in the request: cases changed and cases left as they were (refused or conflicting).
    Done { updated: i64, skipped: i64 },
    /// More than [`INLINE_SYNC_MAX`] usages: a queued `shared_inline` job.
    Job(Box<TmJob>),
}

/// A validated write of a shared step.
#[derive(Clone, Debug)]
pub struct CheckedShared {
    pub name: String,
    pub description: String,
    pub steps: Vec<CaseStep>,
}

fn attachment_ref_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"attachment:[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
            .expect("attachment reference pattern")
    })
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
    if attachment_ref_re().is_match(s) {
        return Err(invalid_case(
            path,
            "attachments are not supported in shared steps",
        ));
    }
    Ok(())
}

struct Walk<'a> {
    limits: &'a TmLimits,
    count: usize,
    ids: HashSet<String>,
}

impl Walk<'_> {
    fn walk(&mut self, steps: &mut [CaseStep], prefix: &str, depth: usize) -> TmResult<()> {
        for (i, s) in steps.iter_mut().enumerate() {
            let path = format!("{prefix}[{i}]");
            if depth > 2 {
                return Err(invalid_case(&path, "shared steps nest at most 2 levels"));
            }
            self.count += 1;
            if self.count > self.limits.max_shared_step_steps {
                return Err(TmError::limit(
                    "maxSharedStepSteps",
                    self.limits.max_shared_step_steps as u64,
                ));
            }
            if s.shared.is_some() {
                return Err(invalid_case(
                    &format!("{path}.shared"),
                    "a shared step cannot contain another shared step",
                ));
            }
            if !s.attachments.is_empty() {
                return Err(invalid_case(
                    &format!("{path}.attachments"),
                    "attachments are not supported in shared steps",
                ));
            }
            markdown(&format!("{path}.action"), &mut s.action, self.limits)?;
            markdown(&format!("{path}.expected"), &mut s.expected, self.limits)?;
            markdown(&format!("{path}.data"), &mut s.data, self.limits)?;
            if s.id.is_empty() {
                s.id = new_step_id();
            } else if !valid_step_id(&s.id) {
                return Err(invalid_case(
                    &format!("{path}.id"),
                    "step ids are s_ and up to 62 letters, digits, _ or -",
                ));
            }
            if !self.ids.insert(s.id.clone()) {
                return Err(invalid_case(&format!("{path}.id"), "duplicate step id"));
            }
            self.walk(&mut s.steps, &format!("{path}.steps"), depth + 1)?;
        }
        Ok(())
    }
}

/// Validates a write: name, Markdown sizes, no `shared` steps and no attachments, depth at
/// most 2, `maxSharedStepSteps` steps and `maxSharedStepBytes` of steps JSON. Step ids are
/// assigned (kept when given).
pub fn check_write(limits: &TmLimits, w: &SharedStepWrite) -> TmResult<CheckedShared> {
    let name = check_name("name", &w.name, limits.max_name_bytes).map_err(|e| match e {
        TmError::Invalid { message, .. } => invalid_case("name", message),
        e => e,
    })?;
    let mut description = w.description.clone().unwrap_or_default();
    markdown("description", &mut description, limits)?;
    if w.steps.is_empty() {
        return Err(invalid_case(
            "steps",
            "a shared step needs at least one step",
        ));
    }
    let mut steps = w.steps.clone();
    let mut walk = Walk {
        limits,
        count: 0,
        ids: HashSet::new(),
    };
    walk.walk(&mut steps, "steps", 1)?;
    if serde_json::to_vec(&steps)?.len() > limits.max_shared_step_bytes {
        return Err(TmError::limit(
            "maxSharedStepBytes",
            limits.max_shared_step_bytes as u64,
        ));
    }
    Ok(CheckedShared {
        name,
        description,
        steps,
    })
}

fn snapshot_of_row(r: &SharedStepRow) -> SharedStepSnapshot {
    SharedStepSnapshot {
        name: r.name.clone(),
        description: r.description.clone(),
        steps: r.steps.clone(),
    }
}

/// The content keys whose values differ, in [`KEYS`] order.
pub fn changed_keys(old: &SharedStepSnapshot, new: &SharedStepSnapshot) -> Vec<String> {
    let diff = [
        old.name != new.name,
        old.description != new.description,
        old.steps != new.steps,
    ];
    KEYS.iter()
        .zip(diff)
        .filter(|(_, d)| *d)
        .map(|(k, _)| k.to_string())
        .collect()
}

fn source_of(origin: SharedStepOrigin) -> VersionSource {
    match origin {
        SharedStepOrigin::Ui => VersionSource::Ui,
        SharedStepOrigin::Import => VersionSource::Import,
        SharedStepOrigin::Ai => VersionSource::Ai,
    }
}

/// The version of a shared step that runs: its head, or under review (`mode` on and the
/// shared step was edited under review) the approved version.
pub fn runnable_of(r: &SharedStepRow, policy: &CaseReviewSettings) -> i32 {
    if policy.mode == ReviewMode::Off || r.review_state.is_none() {
        return r.version;
    }
    r.approved_version.unwrap_or(r.version)
}

/// Shared step ids the top-level steps reference, in order, once each.
pub fn ref_ids(steps: &[CaseStep]) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    steps
        .iter()
        .filter_map(|s| s.shared.as_ref().map(|r| r.id))
        .filter(|id| seen.insert(*id))
        .collect()
}

fn invalid_cursor() -> TmError {
    TmError::bad_request("invalid cursor")
}

#[allow(clippy::too_many_arguments)]
async fn insert_version(
    conn: &mut PgConnection,
    project_id: Uuid,
    shared_step_id: Uuid,
    version: i32,
    source: VersionSource,
    changed: &[String],
    snapshot: &SharedStepSnapshot,
    by: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "insert into shared_step_versions (shared_step_id, version, project_id, source, changed, snapshot_z, created_by) \
         values ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(shared_step_id)
    .bind(version)
    .bind(project_id)
    .bind(source.as_str())
    .bind(changed)
    .bind(crate::codec::encode(snapshot)?)
    .bind(no_nul(by))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn reread(conn: &mut PgConnection, project_id: Uuid, id: Uuid) -> TmResult<SharedStepRow> {
    shared_step_by_id(conn, project_id, id)
        .await?
        .ok_or_else(|| TmError::Other(anyhow::anyhow!("shared step vanished")))
}

async fn lock_by_number(
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
        let source: Option<String> = sqlx::query_scalar(
            "select source from shared_step_versions where shared_step_id = $1 order by version desc limit 1",
        )
        .bind(row.id)
        .fetch_optional(&mut *conn)
        .await?;
        return Err(TmError::Stale {
            current_revision: row.revision,
            changed_by: row.updated_by.clone(),
            source,
        });
    }
    Ok(row)
}

fn archived_error(number: i64) -> TmError {
    TmError::conflict(
        "shared_step_archived",
        format!("SS-{number} is archived; restore it first"),
    )
}

async fn event(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    kind: &str,
    data: Value,
) -> TmResult<()> {
    case_event(conn, ctx.project_key, kind, data).await?;
    Ok(())
}

async fn create_in(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    c: CheckedShared,
    origin: SharedStepOrigin,
) -> TmResult<SharedStepRow> {
    let seq: i64 = sqlx::query_scalar(
        "update projects set shared_step_seq = shared_step_seq + 1 where id = $1 returning shared_step_seq",
    )
    .bind(ctx.project_id)
    .fetch_one(&mut *conn)
    .await?;
    if seq as usize > ctx.limits.max_shared_steps {
        let n: i64 = sqlx::query_scalar("select count(*) from shared_steps where project_id = $1")
            .bind(ctx.project_id)
            .fetch_one(&mut *conn)
            .await?;
        if n as usize >= ctx.limits.max_shared_steps {
            return Err(TmError::limit(
                "maxSharedSteps",
                ctx.limits.max_shared_steps as u64,
            ));
        }
    }
    let id = Uuid::now_v7();
    let search = search_text(
        &c.name,
        &CaseBody {
            description: c.description.clone(),
            steps: c.steps.clone(),
            ..Default::default()
        },
    );
    sqlx::query(
        "insert into shared_steps (id, project_id, number, name, description, steps, search_text, origin, \
           ai_unreviewed, created_by, updated_by) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)",
    )
    .bind(id)
    .bind(ctx.project_id)
    .bind(seq)
    .bind(&c.name)
    .bind(&c.description)
    .bind(serde_json::to_value(&c.steps)?)
    .bind(search)
    .bind(origin.as_str())
    .bind(origin == SharedStepOrigin::Ai)
    .bind(no_nul(ctx.by))
    .execute(&mut *conn)
    .await?;
    let snap = SharedStepSnapshot {
        name: c.name,
        description: c.description,
        steps: c.steps,
    };
    let all: Vec<String> = KEYS.iter().map(|k| k.to_string()).collect();
    let source = source_of(origin);
    insert_version(conn, ctx.project_id, id, 1, source, &all, &snap, ctx.by).await?;
    let row = reread(conn, ctx.project_id, id).await?;
    super::review::on_shared_step_save(conn, ctx, None, &row, &all, source).await?;
    event(
        conn,
        ctx,
        "sharedStep.updated",
        json!({"number": row.number, "version": 1, "revision": 1}),
    )
    .await?;
    reread(conn, ctx.project_id, id).await
}

/// Stores new content of a locked shared step: a version per content change, `revision + 1`,
/// `sharedStep.updated`. A person other than the creator saving an unreviewed AI draft (also
/// without a change) clears `aiUnreviewed`.
async fn write_content(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    cur: &SharedStepRow,
    c: CheckedShared,
    source: VersionSource,
) -> TmResult<SharedStepRow> {
    let old = snapshot_of_row(cur);
    let new = SharedStepSnapshot {
        name: c.name,
        description: c.description,
        steps: c.steps,
    };
    let changed = changed_keys(&old, &new);
    let ack = cur.ai_unreviewed && cur.created_by.as_deref() != Some(ctx.by);
    if changed.is_empty() && !ack {
        return Ok(cur.clone());
    }
    let version = if changed.is_empty() {
        cur.version
    } else {
        cur.version + 1
    };
    let search = search_text(
        &new.name,
        &CaseBody {
            description: new.description.clone(),
            steps: new.steps.clone(),
            ..Default::default()
        },
    );
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "update shared_steps set name = $3, description = $4, steps = $5, search_text = $6, version = $7, \
         revision = revision + 1, updated_by = $8, updated_at = now(), \
         ai_unreviewed = case when $9 then false else ai_unreviewed end \
         where project_id = $1 and id = $2 and revision = $10 returning {SHARED_STEP_COLS}"
    )))
    .bind(ctx.project_id)
    .bind(cur.id)
    .bind(&new.name)
    .bind(&new.description)
    .bind(serde_json::to_value(&new.steps)?)
    .bind(search)
    .bind(version)
    .bind(no_nul(ctx.by))
    .bind(ack)
    .bind(cur.revision)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(r) = r else {
        return Err(TmError::Stale {
            current_revision: cur.revision,
            changed_by: cur.updated_by.clone(),
            source: None,
        });
    };
    let mut row = shared_step_row(&r)?;
    if !changed.is_empty() {
        insert_version(
            conn,
            ctx.project_id,
            cur.id,
            version,
            source,
            &changed,
            &new,
            ctx.by,
        )
        .await?;
        super::review::on_shared_step_save(conn, ctx, Some(cur), &row, &changed, source).await?;
        row = reread(conn, ctx.project_id, cur.id).await?;
    }
    event(
        conn,
        ctx,
        "sharedStep.updated",
        json!({"number": row.number, "version": row.version, "revision": row.revision}),
    )
    .await?;
    Ok(row)
}

type Contents = HashMap<(Uuid, i32), (String, Vec<CaseStep>)>;

/// The shared steps of the project by id (other ids are left out).
pub(crate) async fn rows_by_id(
    conn: &mut PgConnection,
    project_id: Uuid,
    ids: &[Uuid],
) -> TmResult<HashMap<Uuid, SharedStepRow>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {SHARED_STEP_COLS} from shared_steps where project_id = $1 and id = any($2)"
    )))
    .bind(project_id)
    .bind(ids)
    .fetch_all(&mut *conn)
    .await?;
    let mut by_id = HashMap::new();
    for r in &rows {
        let row = shared_step_row(r)?;
        by_id.insert(row.id, row);
    }
    Ok(by_id)
}

/// The shared steps of the project by id and the name and steps of each wanted
/// `(id, version)`: from the row when it is the head, else from the snapshot.
pub(crate) async fn load(
    conn: &mut PgConnection,
    project_id: Uuid,
    wanted: &[(Uuid, i32)],
) -> TmResult<(HashMap<Uuid, SharedStepRow>, Contents)> {
    let ids: Vec<Uuid> = wanted
        .iter()
        .map(|w| w.0)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let by_id = rows_by_id(conn, project_id, &ids).await?;
    let mut contents: Contents = HashMap::new();
    let mut old: Vec<(Uuid, i32)> = Vec::new();
    for (id, v) in wanted {
        match by_id.get(id) {
            Some(r) if r.version == *v => {
                contents.insert((*id, *v), (r.name.clone(), r.steps.clone()));
            }
            Some(_) => old.push((*id, *v)),
            None => {}
        }
    }
    if !old.is_empty() {
        let (sid, ver): (Vec<Uuid>, Vec<i32>) = old.into_iter().unzip();
        let rows = sqlx::query(
            "select v.shared_step_id, v.version, v.snapshot_z from shared_step_versions v \
             join unnest($1::uuid[], $2::int[]) as t(id, v) on t.id = v.shared_step_id and t.v = v.version",
        )
        .bind(&sid)
        .bind(&ver)
        .fetch_all(&mut *conn)
        .await?;
        for r in rows {
            let z: Vec<u8> = r.try_get("snapshot_z")?;
            let snap: SharedStepSnapshot = crate::codec::decode(&z)?;
            contents.insert(
                (r.try_get("shared_step_id")?, r.try_get("version")?),
                (snap.name, snap.steps),
            );
        }
    }
    Ok((by_id, contents))
}

/// Checks the `shared` steps of a case body: top level only with no content of their own, at
/// most `maxSharedRefs`, shared steps of the project (another project's id is 404), new
/// references not to an archived shared step (409 `shared_step_archived`) or an unreviewed AI
/// draft. A body that gains its first `shared` step is a gated write.
pub async fn validate_refs(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    body: &CaseBody,
    current: Option<&CaseRow>,
) -> TmResult<()> {
    fn nested(steps: &[CaseStep], prefix: &str) -> TmResult<()> {
        for (i, s) in steps.iter().enumerate() {
            let path = format!("{prefix}[{i}]");
            if s.shared.is_some() {
                return Err(invalid_case(
                    &format!("{path}.shared"),
                    "shared steps are top level only",
                ));
            }
            nested(&s.steps, &format!("{path}.steps"))?;
        }
        Ok(())
    }
    let mut refs: Vec<(usize, &SharedStepRef)> = Vec::new();
    for (i, s) in body.steps.iter().enumerate() {
        let path = format!("body.steps[{i}]");
        nested(&s.steps, &format!("{path}.steps"))?;
        let Some(r) = &s.shared else {
            continue;
        };
        if !s.action.is_empty()
            || !s.expected.is_empty()
            || !s.data.is_empty()
            || !s.attachments.is_empty()
            || !s.steps.is_empty()
        {
            return Err(invalid_case(
                &path,
                "a shared step reference has no content of its own",
            ));
        }
        if r.version.is_some() {
            return Err(invalid_case(
                &format!("{path}.shared.version"),
                "a case references a shared step without a version",
            ));
        }
        refs.push((i, r));
    }
    if refs.is_empty() {
        return Ok(());
    }
    if refs.len() > ctx.limits.max_shared_refs {
        return Err(TmError::limit(
            "maxSharedRefs",
            ctx.limits.max_shared_refs as u64,
        ));
    }
    let known: HashSet<Uuid> = current
        .map(|c| ref_ids(&c.body.steps))
        .unwrap_or_default()
        .into_iter()
        .collect();
    if known.is_empty() {
        super::gate::require(conn, ctx.limits, "shared steps").await?;
    }
    let ids: Vec<Uuid> = refs
        .iter()
        .map(|r| r.1.id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let rows: Vec<(Uuid, i64, bool, bool)> = sqlx::query_as(
        "select id, number, archived_at is not null, ai_unreviewed from shared_steps \
         where project_id = $1 and id = any($2) order by id for share",
    )
    .bind(ctx.project_id)
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let found: HashMap<Uuid, (i64, bool, bool)> = rows
        .into_iter()
        .map(|(id, n, a, u)| (id, (n, a, u)))
        .collect();
    for (i, r) in refs {
        let path = format!("body.steps[{i}].shared");
        let Some((number, archived, unreviewed)) = found.get(&r.id) else {
            return Err(TmError::NotFound("shared step"));
        };
        if *number != r.number {
            return Err(invalid_case(
                &format!("{path}.number"),
                "does not match the shared step",
            ));
        }
        if known.contains(&r.id) {
            continue;
        }
        if *archived {
            return Err(archived_error(*number));
        }
        if *unreviewed {
            return Err(invalid_case(
                &path,
                format!("SS-{number} is an unreviewed AI draft; a person reviews it first"),
            ));
        }
    }
    Ok(())
}

/// Records which shared steps a case uses (`shared_step_usages`): those its head body
/// references and those of its approved version when that differs (the runnable one under
/// review).
pub async fn sync_usages(
    conn: &mut PgConnection,
    project_id: Uuid,
    case_id: Uuid,
    body: &CaseBody,
) -> TmResult<()> {
    let mut ids: BTreeSet<Uuid> = ref_ids(&body.steps).into_iter().collect();
    let r: Option<(i32, Option<i32>)> =
        sqlx::query_as("select version, approved_version from cases where id = $1")
            .bind(case_id)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some((v, Some(a))) = r
        && a != v
        && let Some(snap) = case_version_snapshot(conn, case_id, a).await?
    {
        ids.extend(ref_ids(&snap.body.steps));
    }
    let ids: Vec<Uuid> = ids.into_iter().collect();
    sqlx::query("delete from shared_step_usages where case_id = $1 and shared_step_id <> all($2)")
        .bind(case_id)
        .bind(&ids)
        .execute(&mut *conn)
        .await?;
    if !ids.is_empty() {
        sqlx::query(
            "insert into shared_step_usages (shared_step_id, case_id, project_id) \
             select s.id, $2, $3 from shared_steps s where s.project_id = $3 and s.id = any($1) \
             on conflict do nothing",
        )
        .bind(&ids)
        .bind(case_id)
        .bind(project_id)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Recomputes the usages of a case from its current head and approved version (the review
/// code calls it after the approved version moved).
pub async fn resync_case_usages(
    conn: &mut PgConnection,
    project_id: Uuid,
    case_id: Uuid,
) -> TmResult<()> {
    let Some(c) = case_by_id(conn, project_id, case_id).await? else {
        return Ok(());
    };
    sync_usages(conn, project_id, case_id, &c.body).await
}

/// The shared steps a case body references, at their runnable version.
pub async fn briefs_for_case(
    conn: &mut PgConnection,
    project_id: Uuid,
    body: &CaseBody,
    review: &CaseReviewSettings,
) -> TmResult<Vec<SharedStepBrief>> {
    let ids = ref_ids(&body.steps);
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = rows_by_id(conn, project_id, &ids).await?;
    let wanted: Vec<(Uuid, i32)> = ids
        .iter()
        .filter_map(|i| rows.get(i).map(|r| (*i, runnable_of(r, review))))
        .collect();
    let (rows, contents) = load(conn, project_id, &wanted).await?;
    Ok(wanted
        .into_iter()
        .filter_map(|(id, v)| {
            let r = rows.get(&id)?;
            let (name, steps) = contents.get(&(id, v))?;
            Some(SharedStepBrief {
                id,
                number: r.number,
                name: name.clone(),
                version: v,
                archived: r.archived_at.is_some(),
                steps: steps.clone(),
                steps_html: Vec::new(),
            })
        })
        .collect())
}

fn summary_of(r: &sqlx::postgres::PgRow) -> sqlx::Result<SharedStepSummary> {
    Ok(SharedStepSummary {
        number: r.try_get("number")?,
        name: r.try_get("name")?,
        version: r.try_get("version")?,
        approved_version: r.try_get("approved_version")?,
        review_state: r
            .try_get::<Option<String>, _>("review_state")?
            .and_then(|s| super::ReviewState::parse(&s)),
        step_count: r.try_get("step_count")?,
        cases: r.try_get("cases")?,
        ai_unreviewed: r.try_get("ai_unreviewed")?,
        archived: r.try_get("archived")?,
        updated_at: r.try_get("updated_at")?,
    })
}

fn number_query(q: &str) -> Option<i64> {
    let t = q.trim();
    let digits = t
        .strip_prefix("SS-")
        .or_else(|| t.strip_prefix("ss-"))
        .unwrap_or(t);
    if digits.is_empty() || digits.len() > 18 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|n| *n > 0)
}

/// Cases that use a shared step, and open manual runs that pin it.
pub async fn usage_count(
    conn: &mut PgConnection,
    project_id: Uuid,
    id: Uuid,
) -> sqlx::Result<SharedStepUsageCount> {
    let cases: i64 =
        sqlx::query_scalar("select count(*) from shared_step_usages where shared_step_id = $1")
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
    let open_runs: i64 = sqlx::query_scalar(
        "select count(distinct l.id) from launches l join run_items ri on ri.launch_id = l.id \
         where l.project_id = $1 and l.manual_state = 'open' and jsonb_exists(ri.shared_pins, $2)",
    )
    .bind(project_id)
    .bind(id.to_string())
    .fetch_one(&mut *conn)
    .await?;
    Ok(SharedStepUsageCount { cases, open_runs })
}

fn view_of(row: &SharedStepRow, usage: SharedStepUsageCount) -> SharedStep {
    SharedStep {
        id: row.id,
        number: row.number,
        name: row.name.clone(),
        description: row.description.clone(),
        steps: row.steps.clone(),
        html: Default::default(),
        version: row.version,
        approved_version: row.approved_version,
        review_state: row.review_state,
        origin: row.origin,
        ai_unreviewed: row.ai_unreviewed,
        usage,
        revision: row.revision,
        created_by: row.created_by.clone(),
        created_at: row.created_at,
        updated_by: row.updated_by.clone(),
        updated_at: row.updated_at,
        archived_at: row.archived_at,
    }
}

fn version_summary(r: &sqlx::postgres::PgRow) -> sqlx::Result<SharedStepVersionSummary> {
    let source: String = r.try_get("source")?;
    Ok(SharedStepVersionSummary {
        version: r.try_get("version")?,
        source: VersionSource::parse(&source).unwrap_or(VersionSource::Ui),
        changed: r.try_get("changed")?,
        created_by: r.try_get("created_by")?,
        created_at: r.try_get::<DateTime<Utc>, _>("created_at")?,
    })
}

fn number_cursor(before: Option<&str>) -> TmResult<Option<i64>> {
    match before {
        None => Ok(None),
        Some(b) => Ok(Some(
            b.parse::<i64>()
                .ok()
                .filter(|v| *v >= 0)
                .ok_or_else(invalid_cursor)?,
        )),
    }
}

fn change_cursor(c: &str) -> Option<(DateTime<Utc>, Uuid, i32)> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c)
        .ok()?;
    let s = String::from_utf8(raw).ok()?;
    let mut p = s.split('|');
    let micros: i64 = p.next()?.parse().ok()?;
    let id = Uuid::parse_str(p.next()?).ok()?;
    let v: i32 = p.next()?.parse().ok()?;
    Some((DateTime::from_timestamp_micros(micros)?, id, v))
}

fn change_cursor_of(at: DateTime<Utc>, id: Uuid, v: i32) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(format!("{}|{id}|{v}", at.timestamp_micros()))
}

fn replace_refs(steps: &[CaseStep], target: Uuid, with: &[CaseStep]) -> Vec<CaseStep> {
    fn fresh(s: &CaseStep) -> CaseStep {
        CaseStep {
            id: new_step_id(),
            steps: s.steps.iter().map(fresh).collect(),
            ..s.clone()
        }
    }
    let mut out = Vec::with_capacity(steps.len());
    for s in steps {
        if s.shared.as_ref().is_some_and(|r| r.id == target) {
            out.extend(with.iter().map(fresh));
        } else {
            out.push(s.clone());
        }
    }
    out
}

impl Store {
    /// A page of shared steps by number.
    pub async fn list_shared_steps(
        &self,
        project_id: Uuid,
        q: &SharedStepQuery,
    ) -> TmResult<SharedStepList> {
        let after = number_cursor(q.before.as_deref())?;
        if q.q.as_ref().is_some_and(|t| t.len() > MAX_Q_BYTES) {
            return Err(TmError::bad_request(format!(
                "q has at most {MAX_Q_BYTES} bytes"
            )));
        }
        let text =
            q.q.as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string);
        let number = text.as_deref().and_then(number_query);
        let like = text.as_deref().map(|t| format!("%{}%", like_escape(t)));
        let mut conn = self.pool.acquire().await?;
        let rows = sqlx::query(
            "select s.number, s.name, s.version, s.approved_version, s.review_state, s.ai_unreviewed, \
               s.updated_at, s.archived_at is not null as archived, \
               (jsonb_array_length(s.steps) + coalesce((select sum(jsonb_array_length(x -> 'steps')) \
                 from jsonb_array_elements(s.steps) x), 0))::bigint as step_count, \
               (select count(*) from shared_step_usages u where u.shared_step_id = s.id) as cases \
             from shared_steps s \
             where s.project_id = $1 and (s.archived_at is not null) = $2 and ($3::bigint is null or s.number > $3) \
               and ($4::text is null or s.number = $5 or s.name ilike $4 escape '\\' \
                 or to_tsvector('simple', s.search_text) @@ plainto_tsquery('simple', $6)) \
             order by s.number limit $7",
        )
        .bind(project_id)
        .bind(q.archived)
        .bind(after)
        .bind(like)
        .bind(number)
        .bind(text.as_deref().unwrap_or(""))
        .bind(q.limit + 1)
        .fetch_all(&mut *conn)
        .await?;
        let mut items: Vec<SharedStepSummary> =
            rows.iter().map(summary_of).collect::<sqlx::Result<_>>()?;
        let more = items.len() as i64 > q.limit;
        items.truncate(q.limit as usize);
        let next = more
            .then(|| items.last().map(|i| i.number.to_string()))
            .flatten();
        Ok(SharedStepList { items, next })
    }

    /// The shared step with number `n` (archived ones included).
    pub async fn shared_step_row(&self, project_id: Uuid, n: i64) -> TmResult<SharedStepRow> {
        let mut conn = self.pool.acquire().await?;
        shared_step_by_number(&mut conn, project_id, n)
            .await?
            .ok_or(TmError::NotFound("shared step"))
    }

    /// The API view of the shared step with number `n` (HTML empty: the server renders it).
    pub async fn shared_step_view(&self, project_id: Uuid, n: i64) -> TmResult<SharedStep> {
        let row = self.shared_step_row(project_id, n).await?;
        self.shared_step_view_of(project_id, &row).await
    }

    /// The API view of a shared step row.
    pub async fn shared_step_view_of(
        &self,
        project_id: Uuid,
        row: &SharedStepRow,
    ) -> TmResult<SharedStep> {
        let mut conn = self.pool.acquire().await?;
        let usage = usage_count(&mut conn, project_id, row.id).await?;
        Ok(view_of(row, usage))
    }

    /// Creates a shared step (version 1, `sharedStep.updated`). An AI draft is `aiUnreviewed`
    /// until a person other than its creator saves it; cases cannot reference it before.
    pub async fn create_shared_step(
        &self,
        ctx: &CaseCtx<'_>,
        w: &SharedStepWrite,
        origin: SharedStepOrigin,
    ) -> TmResult<SharedStepRow> {
        let c = check_write(ctx.limits, w)?;
        let mut tx = self.pool.begin().await?;
        let row = create_in(&mut tx, ctx, c, origin).await?;
        tx.commit().await?;
        Ok(row)
    }

    /// Replaces the content of a shared step when its revision is `expected`; returns the
    /// row before and after. An archived shared step answers 409 `shared_step_archived`.
    pub async fn update_shared_step(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        expected: i64,
        w: &SharedStepWrite,
    ) -> TmResult<(SharedStepRow, SharedStepRow)> {
        let c = check_write(ctx.limits, w)?;
        let mut tx = self.pool.begin().await?;
        let cur = lock_by_number(&mut tx, ctx.project_id, n, Some(expected)).await?;
        if cur.archived_at.is_some() {
            return Err(archived_error(cur.number));
        }
        let row = write_content(&mut tx, ctx, &cur, c, VersionSource::Ui).await?;
        tx.commit().await?;
        Ok((cur, row))
    }

    /// Archives or restores a shared step (allowed while used).
    pub async fn set_shared_step_archived(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        archive: bool,
    ) -> TmResult<SharedStepRow> {
        let mut tx = self.pool.begin().await?;
        let cur = lock_by_number(&mut tx, ctx.project_id, n, None).await?;
        if cur.archived_at.is_some() == archive {
            return Ok(cur);
        }
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update shared_steps set archived_at = case when $3 then now() end, revision = revision + 1, \
             updated_by = $4, updated_at = now() where project_id = $1 and id = $2 returning {SHARED_STEP_COLS}"
        )))
        .bind(ctx.project_id)
        .bind(cur.id)
        .bind(archive)
        .bind(no_nul(ctx.by))
        .fetch_one(&mut *tx)
        .await?;
        let row = shared_step_row(&r)?;
        if archive {
            event(
                &mut tx,
                ctx,
                "sharedStep.archived",
                json!({"number": row.number}),
            )
            .await?;
        } else {
            event(
                &mut tx,
                ctx,
                "sharedStep.updated",
                json!({"number": row.number, "version": row.version, "revision": row.revision}),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(row)
    }

    /// Deletes a shared step with its versions and keys. Refused with 409 `shared_step_in_use
    /// {cases, openRuns}` while any case uses it or an open manual run pins it.
    pub async fn purge_shared_step(&self, ctx: &CaseCtx<'_>, n: i64) -> TmResult<SharedStepRow> {
        let mut tx = self.pool.begin().await?;
        let cur = lock_by_number(&mut tx, ctx.project_id, n, None).await?;
        let usage = usage_count(&mut tx, ctx.project_id, cur.id).await?;
        if usage.cases > 0 || usage.open_runs > 0 {
            return Err(TmError::Conflict {
                code: "shared_step_in_use",
                message: format!("SS-{} is used by cases or open runs", cur.number),
                details: Some(json!({"cases": usage.cases, "openRuns": usage.open_runs})),
            });
        }
        sqlx::query(
            "delete from review_votes where project_id = $1 and kind = 'sharedStep' and object_id = $2",
        )
        .bind(ctx.project_id)
        .bind(cur.id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("delete from shared_steps where project_id = $1 and id = $2")
            .bind(ctx.project_id)
            .bind(cur.id)
            .execute(&mut *tx)
            .await?;
        event(
            &mut tx,
            ctx,
            "sharedStep.archived",
            json!({"number": cur.number, "purged": true}),
        )
        .await?;
        tx.commit().await?;
        Ok(cur)
    }

    /// Versions of the shared step with number `n`, newest first, below the cursor.
    pub async fn shared_step_versions(
        &self,
        project_id: Uuid,
        n: i64,
        before: Option<&str>,
        limit: i64,
    ) -> TmResult<SharedStepVersionList> {
        let before = match before {
            None => None,
            Some(b) => Some(
                b.parse::<i32>()
                    .ok()
                    .filter(|v| *v > 0)
                    .ok_or_else(invalid_cursor)?,
            ),
        };
        let row = self.shared_step_row(project_id, n).await?;
        let rows = sqlx::query(
            "select version, source, changed, created_by, created_at from shared_step_versions \
             where shared_step_id = $1 and ($2::int is null or version < $2) order by version desc limit $3",
        )
        .bind(row.id)
        .bind(before)
        .bind(limit + 1)
        .fetch_all(&self.pool)
        .await?;
        let mut items: Vec<SharedStepVersionSummary> = rows
            .iter()
            .map(version_summary)
            .collect::<sqlx::Result<_>>()?;
        let more = items.len() as i64 > limit;
        items.truncate(limit as usize);
        let next = more
            .then(|| items.last().map(|v| v.version.to_string()))
            .flatten();
        Ok(SharedStepVersionList { items, next })
    }

    /// Version `v` of the shared step with number `n`.
    pub async fn shared_step_version(
        &self,
        project_id: Uuid,
        n: i64,
        v: i32,
    ) -> TmResult<SharedStepVersion> {
        let row = self.shared_step_row(project_id, n).await?;
        let r = sqlx::query(
            "select version, source, changed, created_by, created_at, snapshot_z from shared_step_versions \
             where shared_step_id = $1 and version = $2",
        )
        .bind(row.id)
        .bind(v)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(TmError::NotFound("version"))?;
        let z: Vec<u8> = r.try_get("snapshot_z")?;
        Ok(SharedStepVersion {
            summary: version_summary(&r)?,
            snapshot: crate::codec::decode(&z)?,
        })
    }

    /// Makes the content of version `v` current as a new version (`source = restore`).
    pub async fn restore_shared_step_version(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        v: i32,
        expected: i64,
    ) -> TmResult<(SharedStepRow, SharedStepRow)> {
        let mut tx = self.pool.begin().await?;
        let cur = lock_by_number(&mut tx, ctx.project_id, n, Some(expected)).await?;
        if cur.archived_at.is_some() {
            return Err(archived_error(cur.number));
        }
        let z: Vec<u8> = sqlx::query_scalar(
            "select snapshot_z from shared_step_versions where shared_step_id = $1 and version = $2",
        )
        .bind(cur.id)
        .bind(v)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(TmError::NotFound("version"))?;
        let snap: SharedStepSnapshot = crate::codec::decode(&z)?;
        let c = check_write(
            ctx.limits,
            &SharedStepWrite {
                name: snap.name,
                description: Some(snap.description),
                steps: snap.steps,
            },
        )?;
        let row = write_content(&mut tx, ctx, &cur, c, VersionSource::Restore).await?;
        tx.commit().await?;
        Ok((cur, row))
    }

    /// A page of the cases that use a shared step, by case number, with the open manual runs
    /// that pin it.
    pub async fn shared_step_usages(
        &self,
        project_id: Uuid,
        n: i64,
        before: Option<&str>,
        limit: i64,
    ) -> TmResult<SharedStepUsageList> {
        let after = number_cursor(before)?;
        let row = self.shared_step_row(project_id, n).await?;
        let mut conn = self.pool.acquire().await?;
        let rows = sqlx::query(
            "select c.number, c.name, c.status, c.archived_at is not null as archived \
             from shared_step_usages u join cases c on c.id = u.case_id \
             where u.shared_step_id = $1 and ($2::bigint is null or c.number > $2) \
             order by c.number limit $3",
        )
        .bind(row.id)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(&mut *conn)
        .await?;
        let mut items = Vec::with_capacity(rows.len());
        for r in &rows {
            let status: String = r.try_get("status")?;
            items.push(SharedStepUsage {
                case_number: r.try_get("number")?,
                name: r.try_get("name")?,
                status: super::CaseStatus::parse(&status).unwrap_or_default(),
                archived: r.try_get("archived")?,
            });
        }
        let more = items.len() as i64 > limit;
        items.truncate(limit as usize);
        let next = more
            .then(|| items.last().map(|i| i.case_number.to_string()))
            .flatten();
        let usage = usage_count(&mut conn, project_id, row.id).await?;
        Ok(SharedStepUsageList {
            items,
            next,
            open_runs: usage.open_runs,
        })
    }

    /// The versions written to the shared steps a case uses, newest first (the stream of the
    /// case's "Versions" tab; creations are not changes).
    pub async fn case_shared_step_changes(
        &self,
        project_id: Uuid,
        n: i64,
        before: Option<&str>,
        limit: i64,
    ) -> TmResult<SharedStepChangeList> {
        let cursor = match before {
            None => None,
            Some(b) => Some(change_cursor(b).ok_or_else(invalid_cursor)?),
        };
        let case = self
            .case_by_number(project_id, n)
            .await?
            .ok_or(TmError::NotFound("case"))?;
        let rows = sqlx::query(
            "select s.number, s.name, v.shared_step_id, v.version, v.source, v.changed, v.created_by, v.created_at \
             from shared_step_usages u \
             join shared_step_versions v on v.shared_step_id = u.shared_step_id and v.version > 1 \
             join shared_steps s on s.id = u.shared_step_id \
             where u.case_id = $1 \
               and ($2::timestamptz is null or (v.created_at, v.shared_step_id, v.version) < ($2, $3, $4)) \
             order by v.created_at desc, v.shared_step_id desc, v.version desc limit $5",
        )
        .bind(case.id)
        .bind(cursor.map(|c| c.0))
        .bind(cursor.map(|c| c.1))
        .bind(cursor.map(|c| c.2))
        .bind(limit + 1)
        .fetch_all(&self.pool)
        .await?;
        let mut items = Vec::with_capacity(rows.len());
        let mut last = None;
        for r in rows.iter().take(limit as usize) {
            let source: String = r.try_get("source")?;
            let at: DateTime<Utc> = r.try_get("created_at")?;
            let id: Uuid = r.try_get("shared_step_id")?;
            let version: i32 = r.try_get("version")?;
            last = Some(change_cursor_of(at, id, version));
            items.push(SharedStepChange {
                shared_step_number: r.try_get("number")?,
                name: r.try_get("name")?,
                version,
                source: VersionSource::parse(&source).unwrap_or(VersionSource::Ui),
                changed: r.try_get("changed")?,
                created_by: r.try_get("created_by")?,
                created_at: at,
            });
        }
        let next = (rows.len() as i64 > limit).then_some(last).flatten();
        Ok(SharedStepChangeList { items, next })
    }

    /// Moves adjacent top-level steps of a case into a new shared step and replaces them with a
    /// reference, in one transaction with the case's `If-Match` revision. Returns the shared
    /// step and the case after.
    pub async fn extract_shared_step(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        expected: i64,
        w: &ExtractSharedStep,
    ) -> TmResult<(SharedStepRow, CaseRow)> {
        let l = ctx.limits;
        if w.step_ids.is_empty() {
            return Err(invalid_case("stepIds", "choose at least one step"));
        }
        if w.step_ids.len() > l.max_shared_step_steps {
            return Err(TmError::limit(
                "maxSharedStepSteps",
                l.max_shared_step_steps as u64,
            ));
        }
        let wanted: HashSet<&str> = w.step_ids.iter().map(String::as_str).collect();
        let mut tx = self.pool.begin().await?;
        let cur = lock_case_by_number(&mut tx, ctx.project_id, n, Some(expected)).await?;
        let positions: Vec<usize> = cur
            .body
            .steps
            .iter()
            .enumerate()
            .filter(|(_, s)| wanted.contains(s.id.as_str()))
            .map(|(i, _)| i)
            .collect();
        if positions.len() != wanted.len() {
            return Err(invalid_case(
                "stepIds",
                "every id must be a top-level step of the case",
            ));
        }
        let (first, last) = (positions[0], positions[positions.len() - 1]);
        if last - first + 1 != positions.len() {
            return Err(invalid_case("stepIds", "the steps must be adjacent"));
        }
        let selected = cur.body.steps[first..=last].to_vec();
        if selected.iter().any(|s| s.shared.is_some()) {
            return Err(invalid_case(
                "stepIds",
                "a shared step reference cannot be extracted again",
            ));
        }
        let c = check_write(
            l,
            &SharedStepWrite {
                name: w.name.clone(),
                description: w.description.clone(),
                steps: selected,
            },
        )?;
        let created = create_in(&mut tx, ctx, c, SharedStepOrigin::Ui).await?;
        let mut body = cur.body.clone();
        body.steps.splice(
            first..=last,
            [CaseStep {
                id: new_step_id(),
                shared: Some(SharedStepRef {
                    id: created.id,
                    number: created.number,
                    version: None,
                }),
                ..Default::default()
            }],
        );
        let write = CaseWrite {
            name: cur.name.clone(),
            folder_id: cur.folder_id,
            status: Some(cur.status),
            body: Some(body),
            fields: Some(cur.fields.clone()),
            tags: Some(cur.tags.clone()),
        };
        let row = update_case_in(&mut tx, ctx, &cur, &write, VersionSource::Ui).await?;
        tx.commit().await?;
        Ok((created, row))
    }

    /// Replaces the references to a shared step in every using case by the steps of its
    /// runnable version (case versions with `source = bulk`). Up to [`INLINE_SYNC_MAX`] usages
    /// run in the request; more queue a `shared_inline` job.
    pub async fn inline_shared_step(&self, ctx: &CaseCtx<'_>, n: i64) -> TmResult<InlineOutcome> {
        let row = self.shared_step_row(ctx.project_id, n).await?;
        let usages: i64 =
            sqlx::query_scalar("select count(*) from shared_step_usages where shared_step_id = $1")
                .bind(row.id)
                .fetch_one(&self.pool)
                .await?;
        if usages > INLINE_SYNC_MAX {
            let busy: bool = sqlx::query_scalar(
                "select exists (select 1 from tm_jobs where project_id = $1 and kind = 'shared_inline' \
                 and status in ('queued', 'running') and params ->> 'sharedStepId' = $2)",
            )
            .bind(ctx.project_id)
            .bind(row.id.to_string())
            .fetch_one(&self.pool)
            .await?;
            if busy {
                return Err(TmError::conflict(
                    "conflict",
                    format!("SS-{n} is already being inlined"),
                ));
            }
            let job = self
                .create_tm_job(
                    Uuid::now_v7(),
                    ctx.project_id,
                    ctx.project_key,
                    TmJobKind::SharedInline,
                    &json!({"sharedStepId": row.id, "number": row.number}),
                    ctx.by,
                )
                .await?;
            return Ok(InlineOutcome::Job(Box::new(job)));
        }
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "select case_id from shared_step_usages where shared_step_id = $1 order by case_id",
        )
        .bind(row.id)
        .fetch_all(&self.pool)
        .await?;
        let (updated, skipped) = self.inline_cases(ctx, &row, &ids).await?;
        Ok(InlineOutcome::Done { updated, skipped })
    }

    /// Inlines a shared step into the given cases, each in a transaction of its own; cases that
    /// refuse the change (conflicts, limits, a revision that moved) are counted as skipped.
    async fn inline_cases(
        &self,
        ctx: &CaseCtx<'_>,
        shared: &SharedStepRow,
        case_ids: &[Uuid],
    ) -> TmResult<(i64, i64)> {
        let review = {
            let mut conn = self.pool.acquire().await?;
            super::review::review_settings(&mut conn, ctx.project_id).await?
        };
        let v = runnable_of(shared, &review);
        let steps = {
            let mut conn = self.pool.acquire().await?;
            let (_, contents) = load(&mut conn, ctx.project_id, &[(shared.id, v)]).await?;
            contents
                .get(&(shared.id, v))
                .map(|c| c.1.clone())
                .ok_or_else(|| TmError::Other(anyhow::anyhow!("shared step version missing")))?
        };
        let (mut updated, mut skipped) = (0i64, 0i64);
        for case_id in case_ids {
            let mut tx = self.pool.begin().await?;
            let r = sqlx::query(sqlx::AssertSqlSafe(format!(
                "select {CASE_COLS} from cases where project_id = $1 and id = $2 for update"
            )))
            .bind(ctx.project_id)
            .bind(case_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(r) = r else {
                continue;
            };
            let cur = case_row(&r)?;
            if !ref_ids(&cur.body.steps).contains(&shared.id) {
                sync_usages(&mut tx, ctx.project_id, cur.id, &cur.body).await?;
                tx.commit().await?;
                continue;
            }
            let mut body = cur.body.clone();
            body.steps = replace_refs(&cur.body.steps, shared.id, &steps);
            let write = CaseWrite {
                name: cur.name.clone(),
                folder_id: cur.folder_id,
                status: Some(cur.status),
                body: Some(body),
                fields: Some(cur.fields.clone()),
                tags: Some(cur.tags.clone()),
            };
            match update_case_in(&mut tx, ctx, &cur, &write, VersionSource::Bulk).await {
                Ok(_) => {
                    tx.commit().await?;
                    updated += 1;
                }
                Err(TmError::Other(e)) => return Err(TmError::Other(e)),
                Err(_) => {
                    drop(tx);
                    skipped += 1;
                }
            }
        }
        Ok((updated, skipped))
    }

    /// The `shared_inline` job: replaces the references to a shared step in every using case.
    pub async fn run_shared_inline_job(
        &self,
        job_id: Uuid,
        owner: &str,
        limits: &TmLimits,
    ) -> TmResult<()> {
        let r = sqlx::query(
            "select j.project_id, p.key, j.params, j.created_by from tm_jobs j \
             join projects p on p.id = j.project_id where j.id = $1",
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(TmError::NotFound("job"))?;
        let project_id: Uuid = r.try_get("project_id")?;
        let key: String = r.try_get("key")?;
        let params: Value = r.try_get("params")?;
        let by: Option<String> = r.try_get("created_by")?;
        let by = by.unwrap_or_else(|| "system".to_string());
        let shared_id = params
            .get("sharedStepId")
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| TmError::bad_request("the job has no shared step"))?;
        let shared = {
            let mut conn = self.pool.acquire().await?;
            shared_step_by_id(&mut conn, project_id, shared_id)
                .await?
                .ok_or(TmError::NotFound("shared step"))?
        };
        let ctx = CaseCtx {
            project_id,
            project_key: &key,
            by: &by,
            limits,
            can_review: false,
        };
        let total: i64 =
            sqlx::query_scalar("select count(*) from shared_step_usages where shared_step_id = $1")
                .bind(shared_id)
                .fetch_one(&self.pool)
                .await?;
        let (mut updated, mut skipped, mut done) = (0i64, 0i64, 0i64);
        let mut after = Uuid::nil();
        loop {
            let ids: Vec<Uuid> = sqlx::query_scalar(
                "select case_id from shared_step_usages where shared_step_id = $1 and case_id > $2 \
                 order by case_id limit $3",
            )
            .bind(shared_id)
            .bind(after)
            .bind(INLINE_BATCH)
            .fetch_all(&self.pool)
            .await?;
            let Some(last) = ids.last() else {
                break;
            };
            after = *last;
            let (u, s) = self.inline_cases(&ctx, &shared, &ids).await?;
            updated += u;
            skipped += s;
            done += ids.len() as i64;
            let progress = TmJobProgress {
                done,
                total: Some(total),
            };
            if !self.heartbeat_tm_job(job_id, owner, progress).await? {
                return Ok(());
            }
        }
        let report = ImportReport {
            update: updated,
            skip: skipped,
            ..Default::default()
        };
        self.finish_tm_job(
            job_id,
            owner,
            &key,
            TmJobStatus::Done,
            TmJobProgress {
                done,
                total: Some(total),
            },
            Some(&report),
            None,
        )
        .await?;
        Ok(())
    }

    /// Pins a run item that has not started again to the runnable versions of its shared
    /// steps. 409 `run_item_started` when the item has a status other than `untested` or step
    /// results, 409 `manual_run_closed` when the run is closed. Returns the launch and the new
    /// revision of the item.
    pub async fn refresh_run_item(
        &self,
        project_id: Uuid,
        project_key: &str,
        item_id: Uuid,
    ) -> TmResult<(Uuid, i64)> {
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query(
            "select ri.launch_id, ri.status, ri.execution_no, l.manual_state \
             from run_items ri join launches l on l.id = ri.launch_id \
             where ri.project_id = $1 and ri.id = $2 for update of ri",
        )
        .bind(project_id)
        .bind(item_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(TmError::NotFound("run item"))?;
        let launch_id: Uuid = r.try_get("launch_id")?;
        let status: String = r.try_get("status")?;
        let execution_no: i32 = r.try_get("execution_no")?;
        let state: Option<String> = r.try_get("manual_state")?;
        if state.as_deref() == Some("closed") {
            return Err(TmError::conflict(
                "manual_run_closed",
                "the manual run is closed; reopen it first",
            ));
        }
        let started: bool = sqlx::query_scalar(
            "select exists (select 1 from run_step_results where run_item_id = $1 and execution_no = $2)",
        )
        .bind(item_id)
        .bind(execution_no)
        .fetch_one(&mut *tx)
        .await?;
        if status != "untested" || started {
            return Err(TmError::conflict(
                "run_item_started",
                "the item has results; start a new execution instead",
            ));
        }
        let review = super::review::review_settings(&mut tx, project_id).await?;
        super::expand::repin_item(&mut tx, project_id, item_id, &review).await?;
        let revision: i64 = sqlx::query_scalar(
            "update run_items set revision = revision + 1, updated_at = now() where id = $1 returning revision",
        )
        .bind(item_id)
        .fetch_one(&mut *tx)
        .await?;
        super::runs::item_changed(
            &mut tx,
            project_key,
            launch_id,
            item_id,
            super::RunStatus::Untested,
            revision,
        )
        .await?;
        tx.commit().await?;
        Ok((launch_id, revision))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: &str, action: &str) -> CaseStep {
        CaseStep {
            id: id.into(),
            action: action.into(),
            ..Default::default()
        }
    }

    #[test]
    fn write_rules() {
        let l = TmLimits::default();
        let ok = check_write(
            &l,
            &SharedStepWrite {
                name: " Log in ".into(),
                description: None,
                steps: vec![CaseStep {
                    steps: vec![step("", "child")],
                    ..step("", "open")
                }],
            },
        )
        .unwrap();
        assert_eq!(ok.name, "Log in");
        assert!(ok.steps[0].id.starts_with("s_"));
        assert!(ok.steps[0].steps[0].id.starts_with("s_"));
        let deep = SharedStepWrite {
            name: "x".into(),
            description: None,
            steps: vec![CaseStep {
                steps: vec![CaseStep {
                    steps: vec![step("", "deep")],
                    ..step("", "mid")
                }],
                ..step("", "top")
            }],
        };
        assert!(check_write(&l, &deep).is_err());
        let nested_shared = SharedStepWrite {
            name: "x".into(),
            description: None,
            steps: vec![CaseStep {
                shared: Some(SharedStepRef {
                    id: Uuid::nil(),
                    number: 1,
                    version: None,
                }),
                ..Default::default()
            }],
        };
        assert!(check_write(&l, &nested_shared).is_err());
        let dup = SharedStepWrite {
            name: "x".into(),
            description: None,
            steps: vec![step("s_a", "1"), step("s_a", "2")],
        };
        assert!(check_write(&l, &dup).is_err());
        let empty = SharedStepWrite {
            name: "x".into(),
            description: None,
            steps: Vec::new(),
        };
        assert!(check_write(&l, &empty).is_err());
        let img = SharedStepWrite {
            name: "x".into(),
            description: None,
            steps: vec![step(
                "",
                "![a](attachment:0193a1b2-0000-7000-8000-000000000001)",
            )],
        };
        assert!(check_write(&l, &img).is_err());
        let many = SharedStepWrite {
            name: "x".into(),
            description: None,
            steps: (0..=l.max_shared_step_steps)
                .map(|i| step("", &format!("step {i}")))
                .collect(),
        };
        assert!(matches!(
            check_write(&l, &many),
            Err(TmError::Conflict {
                code: "limit_reached",
                ..
            })
        ));
    }

    #[test]
    fn numbers_and_replacement() {
        assert_eq!(number_query("SS-12"), Some(12));
        assert_eq!(number_query(" 7 "), Some(7));
        assert_eq!(number_query("login"), None);
        assert_eq!(number_query("SS-"), None);
        let id = Uuid::now_v7();
        let r = CaseStep {
            id: "s_ref".into(),
            shared: Some(SharedStepRef {
                id,
                number: 1,
                version: None,
            }),
            ..Default::default()
        };
        let steps = vec![step("s_a", "a"), r.clone(), step("s_b", "b"), r];
        let with = vec![CaseStep {
            steps: vec![step("s_c", "c")],
            ..step("s_x", "x")
        }];
        let out = replace_refs(&steps, id, &with);
        assert_eq!(out.len(), 4);
        assert_eq!(out[1].action, "x");
        assert_ne!(out[1].id, "s_x");
        assert_ne!(out[1].steps[0].id, "s_c");
        assert_ne!(out[1].id, out[3].id);
        assert!(out.iter().all(|s| s.shared.is_none()));
    }
}
