//! Shared steps, requirements, parameters and requirement links of an import (NDJSON v2 and
//! the foreign formats). Shared steps and requirements are created one by one before the
//! cases that use them (the parsers hand them over first); the maps of what was created are
//! kept in the job's parameters, so a resumed import does not create them again.

use std::collections::{BTreeMap, HashMap, HashSet};

use fastproof_tm_import::{
    ImportedCase, ImportedParameters, ImportedRequirement, ImportedSharedStep, ImportedStep,
    RowError,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::super::cases::{CaseCtx, valid_step_id};
use super::super::shared_steps;
use super::super::{
    CaseParameters, CaseStep, ImportMode, ParameterGenerator, ParameterMode, ParameterRow,
    RequirementStatus, SharedStepOrigin, SharedStepWrite, TmError, TmLimits, TmResult,
};
use super::plan::{Planner, err_of};
use crate::Store;

/// Bytes of a requirement key a case names (`REQ-<n>` or an issue key, which the requirements
/// API bounds at 64); longer ones name no requirement.
const MAX_REQUIREMENT_KEY_BYTES: usize = 64;
/// `kind` of the keys of shared steps the importer stores.
const SHARED_KINDS: &[&str] = &["testops", "qase", "testrail", "external"];
/// The shared step key of a foreign id that was never numbered by FastProof.
const FILE_NUMBER: &str = "number";

/// What a job created, by the row (line) of the record in the file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportMaps {
    pub shared: BTreeMap<u64, (Uuid, i64)>,
    pub requirements: BTreeMap<u64, (Uuid, i64)>,
    /// Whether the job keeps the requirement numbers of the file, decided by its first run
    /// (a resumed run sees the requirements the first one created).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_requirement_numbers: Option<bool>,
}

/// The state of an import for records other than cases.
#[derive(Debug, Default)]
pub struct Records {
    /// Shared steps by `(kind, value)`; `number` keys name the numbers of the file.
    pub shared: HashMap<(String, String), (Uuid, i64)>,
    pub maps: ImportMaps,
    /// Keys of the shared steps a dry run met in the file: they would exist after the apply.
    pub virtual_keys: HashSet<(String, String)>,
    /// The maps changed since the job's parameters last held them.
    pub dirty: bool,
    req_by_number: HashMap<i64, Uuid>,
    req_by_key: HashMap<String, Uuid>,
    unknown_keys: HashSet<String>,
    /// Shared step keys that named no usable shared step of the project (asked once).
    unknown_shared: HashSet<(String, String)>,
    keep_numbers: bool,
    used_numbers: HashSet<i64>,
    gated: bool,
}

impl Records {
    pub fn new(existing_requirements: i64, numbered_source: bool) -> Self {
        Self {
            keep_numbers: numbered_source && existing_requirements == 0,
            ..Default::default()
        }
    }

    /// Restores the maps a previous run of the job saved.
    pub fn load(&mut self, v: Option<&serde_json::Value>) {
        if let Some(m) = v.and_then(|v| serde_json::from_value::<ImportMaps>(v.clone()).ok()) {
            if let Some(k) = m.keep_requirement_numbers {
                self.keep_numbers = k;
            }
            self.maps = m;
        }
    }

    fn remember_shared(&mut self, x: &ImportedSharedStep, id: Uuid, number: i64) {
        for k in valid_keys(x) {
            self.shared.insert(k, (id, number));
        }
        if let Some(n) = x.number {
            self.shared
                .insert((FILE_NUMBER.into(), n.to_string()), (id, number));
        }
    }
}

fn valid_keys(x: &ImportedSharedStep) -> Vec<(String, String)> {
    x.keys
        .iter()
        .filter(|k| {
            SHARED_KINDS.contains(&k.kind.as_str())
                && !k.value.is_empty()
                && k.value.len() <= super::super::keys::MAX_KEY_BYTES
                && !k.value.chars().any(char::is_control)
        })
        .map(|k| (k.kind.clone(), k.value.clone()))
        .collect()
}

fn plain_steps(steps: &[ImportedStep]) -> Vec<CaseStep> {
    steps
        .iter()
        .map(|s| CaseStep {
            id: if valid_step_id(&s.id) {
                s.id.clone()
            } else {
                String::new()
            },
            action: s.action.clone(),
            expected: s.expected.clone(),
            data: s.data.clone(),
            attachments: Vec::new(),
            steps: plain_steps(&s.steps),
            shared: None,
        })
        .collect()
}

fn blank_ids(steps: &mut [CaseStep]) {
    for s in steps {
        s.id.clear();
        blank_ids(&mut s.steps);
    }
}

fn write_of(x: &ImportedSharedStep) -> SharedStepWrite {
    SharedStepWrite {
        name: x.name.clone(),
        description: Some(x.description.clone()).filter(|d| !d.is_empty()),
        steps: plain_steps(&x.steps),
    }
}

/// Converts imported parameters to the case's: row ids are assigned when missing, an unknown
/// generator mode drops the generator.
pub fn case_parameters(p: &ImportedParameters) -> CaseParameters {
    let mut seen = HashSet::new();
    CaseParameters {
        columns: p.columns.clone(),
        rows: p
            .rows
            .iter()
            .map(|r| {
                let valid = r.id.len() > 2
                    && r.id.len() <= 64
                    && r.id.starts_with("r_")
                    && r.id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    && seen.insert(r.id.clone());
                ParameterRow {
                    id: if valid {
                        r.id.clone()
                    } else {
                        loop {
                            let id = format!("r_{}", Uuid::now_v7().simple());
                            if seen.insert(id.clone()) {
                                break id;
                            }
                        }
                    },
                    values: r.values.clone(),
                    enabled: r.enabled,
                }
            })
            .collect(),
        generator: p.generator.as_ref().and_then(|g| {
            Some(ParameterGenerator {
                mode: ParameterMode::parse(&g.mode)?,
                values: g.values.clone(),
            })
        }),
    }
}

/// Fields of a requirement record that passed the checks.
struct ReqFields {
    title: String,
    description: String,
    req_type: String,
    status: RequirementStatus,
    tracker_id: Option<String>,
    issue_key: Option<String>,
    url: Option<String>,
    external_state: Option<String>,
}

fn clean(v: &Option<String>, max: usize) -> Result<Option<String>, &'static str> {
    match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) if s.len() > max || s.chars().any(char::is_control) => {
            Err("an identifier or url is too long or has control characters")
        }
        Some(s) => Ok(Some(s.to_string())),
    }
}

/// A tracker id as the requirements API accepts it: 1 to 32 letters, digits, `_` or `-`.
fn valid_tracker_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// An issue key as the requirements API accepts it: 1 to 64 bytes without spaces or control
/// characters.
fn valid_issue_key(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn check_requirement(l: &TmLimits, x: &ImportedRequirement) -> Result<ReqFields, RowError> {
    let bad = |m: &str| RowError::new(x.row, "invalid_case", m.to_string());
    let title = x.title.trim();
    if title.is_empty() || title.len() > l.max_name_bytes || title.chars().any(char::is_control) {
        return Err(bad(
            "a requirement title is empty, too long or has control characters",
        ));
    }
    if x.description.len() > l.max_markdown_bytes {
        return Err(RowError::new(
            x.row,
            "limit_reached",
            format!("maxMarkdownBytes ({}) exceeded", l.max_markdown_bytes),
        ));
    }
    let req_type = match x.req_type.trim() {
        "" => "requirement".to_string(),
        t if t.len() <= 32
            && t.starts_with(|c: char| c.is_ascii_lowercase())
            && t.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'
            }) =>
        {
            t.to_string()
        }
        _ => return Err(bad("the requirement type is not a lowercase word")),
    };
    let status = match x.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => RequirementStatus::Active,
        Some(s) => RequirementStatus::parse(s).ok_or_else(|| bad("unknown requirement status"))?,
    };
    let url = clean(&x.url, 2048).map_err(bad)?;
    if url.as_deref().is_some_and(|u| {
        let lower = u.to_ascii_lowercase();
        !(lower.starts_with("https://") || lower.starts_with("http://"))
            || u.len() <= 8
            || u.chars().any(char::is_whitespace)
    }) {
        return Err(bad("the requirement url must be http or https"));
    }
    let tracker_id = clean(&x.tracker_id, 32).map_err(bad)?;
    if tracker_id.as_deref().is_some_and(|t| !valid_tracker_id(t)) {
        return Err(bad(
            "the requirement trackerId must have 1 to 32 letters, digits, _ or -",
        ));
    }
    let issue_key = clean(&x.issue_key, 64).map_err(bad)?;
    if issue_key.as_deref().is_some_and(|k| !valid_issue_key(k)) {
        return Err(bad(
            "the requirement issueKey must have 1 to 64 bytes without spaces",
        ));
    }
    if tracker_id.is_some() && issue_key.is_none() {
        return Err(bad("the requirement trackerId needs an issueKey"));
    }
    Ok(ReqFields {
        title: title.to_string(),
        description: x.description.clone(),
        req_type,
        status,
        tracker_id,
        issue_key,
        url,
        external_state: clean(&x.external_state, 200).map_err(bad)?,
    })
}

impl Planner {
    /// Looks up the shared steps `c` names that the file did not define, by their keys.
    pub async fn ensure_shared_refs(
        &mut self,
        conn: &mut PgConnection,
        project_id: Uuid,
        c: &ImportedCase,
    ) -> TmResult<()> {
        let mut kinds = Vec::new();
        let mut values = Vec::new();
        for s in &c.steps {
            let Some(k) = &s.shared else { continue };
            let key = (k.kind.clone(), k.value.clone());
            if k.kind == FILE_NUMBER
                || !SHARED_KINDS.contains(&k.kind.as_str())
                || k.value.len() > super::super::keys::MAX_KEY_BYTES
                || self.rec.shared.contains_key(&key)
                || self.rec.unknown_shared.contains(&key)
                || kinds
                    .iter()
                    .zip(&values)
                    .any(|(a, b)| *a == k.kind && *b == k.value)
            {
                continue;
            }
            kinds.push(k.kind.clone());
            values.push(k.value.clone());
        }
        if kinds.is_empty() {
            return Ok(());
        }
        let rows: Vec<(String, String, Uuid, i64)> = sqlx::query_as(
            "select k.kind, k.value, s.id, s.number from shared_step_keys k \
             join unnest($2::text[], $3::text[]) as t(kind, value) on k.kind = t.kind and k.value = t.value \
             join shared_steps s on s.id = k.shared_step_id \
             where k.project_id = $1 and s.archived_at is null and not s.ai_unreviewed",
        )
        .bind(project_id)
        .bind(&kinds)
        .bind(&values)
        .fetch_all(&mut *conn)
        .await?;
        for (kind, value, id, number) in rows {
            self.rec.shared.insert((kind, value), (id, number));
        }
        for key in kinds.into_iter().zip(values) {
            if !self.rec.shared.contains_key(&key) && self.rec.unknown_shared.len() < 100_000 {
                self.rec.unknown_shared.insert(key);
            }
        }
        Ok(())
    }

    /// Resolves the requirement keys of `items` that the file did not define by their issue
    /// keys, in one query (the answers are kept, up to 100 000 keys).
    async fn resolve_requirement_keys(
        &mut self,
        conn: &mut PgConnection,
        project_id: Uuid,
        items: &[(Uuid, &ImportedCase)],
    ) -> TmResult<()> {
        let mut want: Vec<String> = Vec::new();
        for (_, c) in items {
            for r in &c.requirements {
                let k = r.key.trim();
                if k.is_empty() || k.len() > MAX_REQUIREMENT_KEY_BYTES {
                    continue;
                }
                if !self.rec.req_by_key.contains_key(k)
                    && !self.rec.unknown_keys.contains(k)
                    && !want.iter().any(|w| w == k)
                {
                    want.push(k.to_string());
                }
            }
        }
        if want.is_empty() {
            return Ok(());
        }
        let rows: Vec<(String, Uuid)> = sqlx::query_as(
            "select distinct on (issue_key) issue_key, id from requirements \
             where project_id = $1 and issue_key = any($2) and archived_at is null \
             order by issue_key, number",
        )
        .bind(project_id)
        .bind(&want)
        .fetch_all(&mut *conn)
        .await?;
        for (k, id) in rows {
            if self.rec.req_by_key.len() < 200_000 {
                self.rec.req_by_key.insert(k, id);
            }
        }
        for k in want {
            if !self.rec.req_by_key.contains_key(&k) && self.rec.unknown_keys.len() < 100_000 {
                self.rec.unknown_keys.insert(k);
            }
        }
        Ok(())
    }

    /// Links the cases to the requirements their keys name (the rest is ignored: an issue
    /// link is not a requirement unless one with that key exists). Links beyond
    /// `maxRequirementCases` of a requirement are left out with a warning.
    pub(crate) async fn link_requirements(
        &mut self,
        conn: &mut PgConnection,
        ctx: &CaseCtx<'_>,
        items: &[(Uuid, &ImportedCase)],
    ) -> TmResult<()> {
        let mut req = Vec::new();
        let mut case = Vec::new();
        let mut origin = Vec::new();
        let mut status = Vec::new();
        let mut seen: HashSet<(Uuid, Uuid)> = HashSet::new();
        self.resolve_requirement_keys(conn, ctx.project_id, items)
            .await?;
        for (case_id, c) in items {
            for r in &c.requirements {
                let Some(id) = self.rec.req_by_key.get(r.key.trim()).copied() else {
                    continue;
                };
                if !seen.insert((id, *case_id)) {
                    continue;
                }
                req.push(id);
                case.push(*case_id);
                origin.push(match r.origin.as_deref() {
                    Some(o @ ("ui" | "code" | "import" | "ai")) => o.to_string(),
                    _ => "import".to_string(),
                });
                status.push(match r.status.as_deref() {
                    Some("proposed") => "proposed".to_string(),
                    _ => "active".to_string(),
                });
            }
        }
        if req.is_empty() {
            return Ok(());
        }
        super::super::gate::require(conn, ctx.limits, "import.ndjson2").await?;
        let have: Vec<(Uuid, i64)> = sqlx::query_as(
            "select requirement_id, count(*) from requirement_links where requirement_id = any($1) \
             group by requirement_id",
        )
        .bind(&req)
        .fetch_all(&mut *conn)
        .await?;
        let mut counts: HashMap<Uuid, i64> = have.into_iter().collect();
        let max = ctx.limits.max_requirement_cases as i64;
        let keep: Vec<usize> = (0..req.len())
            .filter(|i| {
                let n = counts.entry(req[*i]).or_insert(0);
                *n += 1;
                *n <= max
            })
            .collect();
        if keep.len() < req.len() {
            self.warn(
                items.first().map_or(0, |(_, c)| c.row),
                "limit_reached",
                format!(
                    "{} links to requirements with {max} cases were left out",
                    req.len() - keep.len()
                ),
            );
            req = keep.iter().map(|i| req[*i]).collect();
            case = keep.iter().map(|i| case[*i]).collect();
            origin = keep.iter().map(|i| origin[*i].clone()).collect();
            status = keep.iter().map(|i| status[*i].clone()).collect();
        }
        sqlx::query(
            "insert into requirement_links (requirement_id, case_id, project_id, origin, status, created_by) \
             select r, c, $1, o, s, $2 from unnest($3::uuid[], $4::uuid[], $5::text[], $6::text[]) as t(r, c, o, s) \
             on conflict (requirement_id, case_id) do nothing",
        )
        .bind(ctx.project_id)
        .bind(crate::no_nul(ctx.by))
        .bind(&req)
        .bind(&case)
        .bind(&origin)
        .bind(&status)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Whether `c` names a shared step that is known (`true`) or not (`false`); `None` when
    /// it names none.
    pub(crate) fn shared_known(&self, c: &ImportedCase) -> Option<bool> {
        let mut any = None;
        for s in &c.steps {
            if let Some(k) = &s.shared {
                let key = (k.kind.clone(), k.value.clone());
                let known =
                    self.rec.shared.contains_key(&key) || self.rec.virtual_keys.contains(&key);
                any = Some(any.unwrap_or(true) && known);
            }
        }
        any
    }
}

impl Planner {
    /// The steps of `c` as they would be after the apply, with references to the shared
    /// steps the file defines (a dry run compares them with the case's). `None` when the
    /// file defines none that `c` uses.
    pub(crate) fn steps_after_apply(
        &self,
        c: &ImportedCase,
        cur: Option<&[CaseStep]>,
    ) -> Option<Vec<CaseStep>> {
        let mut map: HashMap<(String, String), (Uuid, i64)> = HashMap::new();
        let mut any = false;
        for s in &c.steps {
            if let Some(k) = &s.shared {
                let key = (k.kind.clone(), k.value.clone());
                if let Some(v) = self.rec.shared.get(&key) {
                    map.insert(key, *v);
                } else if self.rec.virtual_keys.contains(&key) {
                    map.insert(key, (Uuid::nil(), 0));
                    any = true;
                }
            }
        }
        any.then(|| super::plan::steps_with(&c.steps, cur, &map))
    }
}

fn same_content(row: &super::super::SharedStepRow, w: &SharedStepWrite) -> bool {
    let mut a = row.steps.clone();
    let mut b = w.steps.clone();
    blank_ids(&mut a);
    blank_ids(&mut b);
    row.name == w.name.trim()
        && row.description == w.description.clone().unwrap_or_default()
        && a == b
}

impl Store {
    /// Extends the lease of a running import and saves the maps of the records it created when
    /// they changed, so a long run of records keeps the job and a resumed job does not create
    /// them again. `false` when the job is no longer the worker's (cancelled or taken over).
    pub async fn save_import_records(
        &self,
        planner: &mut Planner,
        job: Uuid,
        owner: &str,
    ) -> anyhow::Result<bool> {
        let maps = planner
            .rec
            .dirty
            .then(|| serde_json::to_value(&planner.rec.maps))
            .transpose()?;
        let n = sqlx::query(
            "update tm_jobs set lease_until = now() + make_interval(secs => $3), \
             params = case when $4::jsonb is null then params else params || jsonb_build_object('importMaps', $4::jsonb) end \
             where id = $1 and lease_owner = $2 and status = 'running'",
        )
        .bind(job)
        .bind(owner)
        .bind(super::LEASE_SECONDS as f64)
        .bind(maps)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if n == 1 {
            planner.rec.dirty = false;
        }
        Ok(n == 1)
    }

    async fn gate_records(&self, planner: &mut Planner, ctx: &CaseCtx<'_>) -> TmResult<()> {
        if planner.rec.gated {
            return Ok(());
        }
        let mut conn = self.pool.acquire().await?;
        super::super::gate::require(&mut conn, ctx.limits, "import.ndjson2").await?;
        planner.rec.gated = true;
        Ok(())
    }

    /// Creates (or, with `upsert`, updates) the shared step of a record and remembers it for
    /// the cases that follow. A shared step with one of the keys is reused. In a dry run the
    /// record is only checked. A closed gate stops the import (409 `upgrade_in_progress`);
    /// any other failure is a row error.
    pub async fn import_shared_step(
        &self,
        planner: &mut Planner,
        ctx: &CaseCtx<'_>,
        x: &ImportedSharedStep,
        apply: bool,
    ) -> TmResult<()> {
        self.gate_records(planner, ctx).await?;
        match self.import_shared_inner(planner, ctx, x, apply).await {
            Ok(()) => Ok(()),
            Err(e) => {
                if matches!(
                    &e,
                    TmError::Conflict {
                        code: "upgrade_in_progress",
                        ..
                    }
                ) {
                    return Err(e);
                }
                let e = err_of(x.row, e)?;
                planner.error(e);
                Ok(())
            }
        }
    }

    async fn import_shared_inner(
        &self,
        planner: &mut Planner,
        ctx: &CaseCtx<'_>,
        x: &ImportedSharedStep,
        apply: bool,
    ) -> TmResult<()> {
        let w = write_of(x);
        shared_steps::check_write(ctx.limits, &w)?;
        if !apply {
            if planner.rec.virtual_keys.len() < 100_000 {
                for k in valid_keys(x) {
                    planner.rec.virtual_keys.insert(k);
                }
                if let Some(n) = x.number {
                    planner
                        .rec
                        .virtual_keys
                        .insert((FILE_NUMBER.into(), n.to_string()));
                }
            }
            return Ok(());
        }
        if let Some((id, n)) = planner.rec.maps.shared.get(&x.row).copied() {
            planner.rec.remember_shared(x, id, n);
            return Ok(());
        }
        let keys = valid_keys(x);
        let mut conn = self.pool.acquire().await?;
        let mut found: Option<Uuid> = None;
        for (kind, value) in &keys {
            found = sqlx::query_scalar(
                "select shared_step_id from shared_step_keys where project_id = $1 and kind = $2 and value = $3",
            )
            .bind(ctx.project_id)
            .bind(kind)
            .bind(value)
            .fetch_optional(&mut *conn)
            .await?;
            if found.is_some() {
                break;
            }
        }
        if found.is_none()
            && let Some(n) = x.number
        {
            found = sqlx::query_scalar(
                "select id from shared_steps where project_id = $1 and number = $2 and name = $3 \
                 and archived_at is null and not ai_unreviewed",
            )
            .bind(ctx.project_id)
            .bind(n)
            .bind(x.name.trim())
            .fetch_optional(&mut *conn)
            .await?;
        }
        drop(conn);
        let (id, number) = match found {
            Some(id) => {
                let mut conn = self.pool.acquire().await?;
                let row = crate::tm::lookup::shared_step_by_id(&mut conn, ctx.project_id, id)
                    .await?
                    .ok_or(TmError::NotFound("shared step"))?;
                drop(conn);
                if row.archived_at.is_some() || row.ai_unreviewed {
                    planner.warn(
                        x.row,
                        if row.ai_unreviewed {
                            "ai_unreviewed"
                        } else {
                            "shared_step_archived"
                        },
                        format!(
                            "shared step SS-{} with this key is {}; the cases keep its steps inline",
                            row.number,
                            if row.ai_unreviewed {
                                "an unreviewed AI draft"
                            } else {
                                "archived"
                            }
                        ),
                    );
                    return Ok(());
                }
                if planner.opts.mode == ImportMode::Upsert && !same_content(&row, &w) {
                    self.update_shared_step(ctx, row.number, row.revision, &w)
                        .await?;
                }
                (row.id, row.number)
            }
            None => {
                let row = self
                    .create_shared_step(ctx, &w, SharedStepOrigin::Import)
                    .await?;
                (row.id, row.number)
            }
        };
        if !keys.is_empty() {
            let (kinds, values): (Vec<_>, Vec<_>) = keys.iter().cloned().unzip();
            sqlx::query(
                "insert into shared_step_keys (project_id, kind, value, shared_step_id) \
                 select $1, k, v, $2 from unnest($3::text[], $4::text[]) as t(k, v) \
                 on conflict (project_id, kind, value) do nothing",
            )
            .bind(ctx.project_id)
            .bind(id)
            .bind(&kinds)
            .bind(&values)
            .execute(&self.pool)
            .await?;
        }
        planner.rec.maps.shared.insert(x.row, (id, number));
        planner.rec.dirty = true;
        planner.rec.remember_shared(x, id, number);
        Ok(())
    }

    /// Creates (or, with `upsert`, updates) the requirement of a record. A requirement with
    /// the same `(trackerId, issueKey)` is reused. Numbers of the file are kept in a project
    /// without requirements. A parent the file does not hold makes the requirement a root
    /// (warning `parent_not_found`).
    pub async fn import_requirement(
        &self,
        planner: &mut Planner,
        ctx: &CaseCtx<'_>,
        x: &ImportedRequirement,
        apply: bool,
    ) -> TmResult<()> {
        self.gate_records(planner, ctx).await?;
        let f = match check_requirement(ctx.limits, x) {
            Ok(f) => f,
            Err(e) => {
                planner.error(e);
                return Ok(());
            }
        };
        if !apply {
            return Ok(());
        }
        match self.import_requirement_inner(planner, ctx, x, f).await {
            Ok(()) => Ok(()),
            Err(TmError::Other(e)) if super::plan::is_unique(&e) => {
                planner.error(RowError::new(
                    x.row,
                    "requirement_number_taken",
                    "the number of the requirement is taken",
                ));
                Ok(())
            }
            Err(e) => {
                let e = err_of(x.row, e)?;
                planner.error(e);
                Ok(())
            }
        }
    }

    async fn import_requirement_inner(
        &self,
        planner: &mut Planner,
        ctx: &CaseCtx<'_>,
        x: &ImportedRequirement,
        f: ReqFields,
    ) -> TmResult<()> {
        let remember = |planner: &mut Planner, id: Uuid, number: i64| {
            if let Some(n) = x.number {
                planner.rec.req_by_number.insert(n, id);
                planner.rec.req_by_key.insert(format!("REQ-{n}"), id);
            }
            planner.rec.req_by_key.insert(format!("REQ-{number}"), id);
            if let Some(k) = &f.issue_key {
                planner.rec.req_by_key.insert(k.clone(), id);
            }
        };
        if let Some((id, n)) = planner.rec.maps.requirements.get(&x.row).copied() {
            remember(planner, id, n);
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        let existing: Option<(Uuid, i64, String, String, String, String, i64)> =
            match &f.issue_key {
                Some(k) => {
                    sqlx::query_as(
                        "select id, number, title, description, type, status, revision from requirements \
                         where project_id = $1 and coalesce(tracker_id, '') = $2 and issue_key = $3",
                    )
                    .bind(ctx.project_id)
                    .bind(f.tracker_id.as_deref().unwrap_or(""))
                    .bind(k)
                    .fetch_optional(&mut *tx)
                    .await?
                }
                None => match x.number {
                    Some(n) => {
                        sqlx::query_as(
                            "select id, number, title, description, type, status, revision from requirements \
                             where project_id = $1 and number = $2 and title = $3 and archived_at is null",
                        )
                        .bind(ctx.project_id)
                        .bind(n)
                        .bind(&f.title)
                        .fetch_optional(&mut *tx)
                        .await?
                    }
                    None => None,
                },
            };
        let (id, number) = if let Some((id, number, title, desc, ty, st, _)) = existing {
            if planner.opts.mode == ImportMode::Upsert {
                let content = title != f.title || desc != f.description;
                let changed = content || ty != f.req_type || st != f.status.as_str();
                if changed {
                    sqlx::query(
                        "update requirements set title = $2, description = $3, type = $4, status = $5, url = $6, \
                           external_state = $7, revision = revision + 1, updated_by = $8, updated_at = now(), \
                           content_changed_at = case when $9 then now() else content_changed_at end \
                         where id = $1",
                    )
                    .bind(id)
                    .bind(crate::no_nul(&f.title))
                    .bind(crate::no_nul(&f.description))
                    .bind(&f.req_type)
                    .bind(f.status.as_str())
                    .bind(&f.url)
                    .bind(&f.external_state)
                    .bind(crate::no_nul(ctx.by))
                    .bind(content)
                    .execute(&mut *tx)
                    .await?;
                }
            }
            (id, number)
        } else {
            let (parent, path, depth) = match x.parent {
                None => (None, Vec::new(), 0i16),
                Some(p) => match planner.rec.req_by_number.get(&p).copied() {
                    Some(pid) => {
                        let (path, depth): (Vec<Uuid>, i16) = sqlx::query_as(
                            "select path_ids, depth from requirements where project_id = $1 and id = $2",
                        )
                        .bind(ctx.project_id)
                        .bind(pid)
                        .fetch_one(&mut *tx)
                        .await?;
                        (Some(pid), path, depth)
                    }
                    None => {
                        planner.warn(
                            x.row,
                            "parent_not_found",
                            format!("the parent {p} is not in the file; imported as a root"),
                        );
                        (None, Vec::new(), 0)
                    }
                },
            };
            let depth = depth + 1;
            if depth as usize > ctx.limits.max_requirement_depth {
                return Err(TmError::limit(
                    "maxRequirementDepth",
                    ctx.limits.max_requirement_depth as u64,
                ));
            }
            if planner.rec.maps.keep_requirement_numbers.is_none() {
                planner.rec.maps.keep_requirement_numbers = Some(planner.rec.keep_numbers);
                planner.rec.dirty = true;
            }
            let kept = match x.number {
                Some(n)
                    if planner.rec.keep_numbers && n > 0 && planner.rec.used_numbers.insert(n) =>
                {
                    Some(n)
                }
                _ => None,
            };
            let number: i64 = match kept {
                Some(n) => {
                    sqlx::query("update projects set requirement_seq = greatest(requirement_seq, $2) where id = $1")
                        .bind(ctx.project_id)
                        .bind(n)
                        .execute(&mut *tx)
                        .await?;
                    n
                }
                None => {
                    let mut n: i64 = sqlx::query_scalar(
                        "update projects set requirement_seq = requirement_seq + 1 where id = $1 returning requirement_seq",
                    )
                    .bind(ctx.project_id)
                    .fetch_one(&mut *tx)
                    .await?;
                    while planner.rec.keep_numbers && planner.rec.used_numbers.contains(&n) {
                        n = sqlx::query_scalar(
                            "update projects set requirement_seq = requirement_seq + 1 where id = $1 returning requirement_seq",
                        )
                        .bind(ctx.project_id)
                        .fetch_one(&mut *tx)
                        .await?;
                    }
                    n
                }
            };
            let seq: i64 = sqlx::query_scalar("select requirement_seq from projects where id = $1")
                .bind(ctx.project_id)
                .fetch_one(&mut *tx)
                .await?;
            if seq as usize > ctx.limits.max_requirements {
                let count: i64 =
                    sqlx::query_scalar("select count(*) from requirements where project_id = $1")
                        .bind(ctx.project_id)
                        .fetch_one(&mut *tx)
                        .await?;
                if count as usize >= ctx.limits.max_requirements {
                    return Err(TmError::limit(
                        "maxRequirements",
                        ctx.limits.max_requirements as u64,
                    ));
                }
            }
            let id = Uuid::now_v7();
            let mut path_ids = path;
            path_ids.push(id);
            sqlx::query(
                "insert into requirements (id, project_id, number, title, description, type, status, parent_id, \
                   path_ids, depth, tracker_id, issue_key, url, external_state, origin, created_by, updated_by) \
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, 'import', $15, $15)",
            )
            .bind(id)
            .bind(ctx.project_id)
            .bind(number)
            .bind(crate::no_nul(&f.title))
            .bind(crate::no_nul(&f.description))
            .bind(&f.req_type)
            .bind(f.status.as_str())
            .bind(parent)
            .bind(&path_ids)
            .bind(depth)
            .bind(&f.tracker_id)
            .bind(&f.issue_key)
            .bind(&f.url)
            .bind(&f.external_state)
            .bind(crate::no_nul(ctx.by))
            .execute(&mut *tx)
            .await?;
            (id, number)
        };
        let revision: i64 = sqlx::query_scalar("select revision from requirements where id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        super::super::cases::case_event(
            &mut tx,
            ctx.project_key,
            "requirement.updated",
            json!({"number": number, "revision": revision}),
        )
        .await?;
        tx.commit().await?;
        planner.rec.maps.requirements.insert(x.row, (id, number));
        planner.rec.dirty = true;
        remember(planner, id, number);
        Ok(())
    }
}
