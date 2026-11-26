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
use sqlx::PgConnection;
use uuid::Uuid;

use super::super::cases::{CaseCtx, valid_step_id};
use super::super::parameters::{allure_parameters, new_row_id, row_values, valid_row_id};
use super::super::requirements::{
    self, Change, Checked, MAX_STATE_BYTES, check_write, valid_issue_key,
};
use super::super::shared_steps;
use super::super::{
    CaseParameters, CaseStep, ImportMode, ParameterGenerator, ParameterMode, ParameterRow,
    RequirementOrigin, RequirementStatus, RequirementWrite, SharedStepOrigin, SharedStepWrite,
    TmError, TmLimits, TmResult, lock_project,
};
use super::plan::{Planner, err_of};
use crate::Store;
use fastproof_model::identity::parameters_hash;

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
    /// Requirements by their number in the file: id and number in the project.
    req_by_number: HashMap<i64, (Uuid, i64)>,
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

/// Converts imported parameters to the case's: row ids that are missing, invalid or repeated
/// get new ones ([`new_row_id`]), an enabled row with the same values (as Allure parameters,
/// one series) as an earlier enabled row is switched off (a foreign tool may list a value
/// twice), and an unknown generator mode drops the generator. The case write then checks them
/// as any other (`validate_parameters`).
pub fn case_parameters(p: &ImportedParameters) -> CaseParameters {
    let mut seen = HashSet::new();
    let mut sets = HashSet::new();
    CaseParameters {
        columns: p.columns.clone(),
        rows: p
            .rows
            .iter()
            .map(|r| {
                let valid = valid_row_id(&r.id) && seen.insert(r.id.clone());
                ParameterRow {
                    id: if valid {
                        r.id.clone()
                    } else {
                        loop {
                            let id = new_row_id();
                            if seen.insert(id.clone()) {
                                break id;
                            }
                        }
                    },
                    values: r.values.clone(),
                    enabled: r.enabled && {
                        let row = ParameterRow {
                            id: String::new(),
                            values: r.values.clone(),
                            enabled: true,
                        };
                        sets.insert(parameters_hash(&allure_parameters(
                            &p.columns,
                            &row_values(&p.columns, &row),
                        )))
                    },
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

/// Fields of a requirement record that passed the checks: the API's ([`check_write`]) and
/// the external state.
struct ReqFields {
    checked: Checked,
    external_state: Option<String>,
}

fn opt(v: &Option<String>) -> Option<Option<String>> {
    Some(v.clone())
}

fn check_requirement(ctx: &CaseCtx<'_>, x: &ImportedRequirement) -> Result<ReqFields, RowError> {
    let bad = |m: String| RowError::new(x.row, "invalid_case", m);
    let l: &TmLimits = ctx.limits;
    if x.description.len() > l.max_markdown_bytes {
        return Err(RowError::new(
            x.row,
            "limit_reached",
            format!("maxMarkdownBytes ({}) exceeded", l.max_markdown_bytes),
        ));
    }
    let status = match x.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => Some(
            RequirementStatus::parse(s).ok_or_else(|| bad("unknown requirement status".into()))?,
        ),
    };
    let w = RequirementWrite {
        title: x.title.clone(),
        description: Some(x.description.clone()),
        req_type: Some(x.req_type.trim().to_string()).filter(|t| !t.is_empty()),
        status,
        parent_number: None,
        tracker_id: opt(&x.tracker_id),
        issue_key: opt(&x.issue_key),
        url: opt(&x.url),
    };
    let checked = check_write(ctx, &w, RequirementOrigin::Import).map_err(|e| match e {
        TmError::Invalid { message, .. } | TmError::Conflict { message, .. } => {
            bad(format!("requirement: {message}"))
        }
        other => bad(format!("requirement: {other}")),
    })?;
    let external_state = match x.external_state.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(s) if s.len() > MAX_STATE_BYTES || s.chars().any(char::is_control) => {
            return Err(bad(format!(
                "the requirement externalState must have at most {MAX_STATE_BYTES} bytes without control characters"
            )));
        }
        Some(s) => Some(s.to_string()),
    };
    Ok(ReqFields {
        checked,
        external_state,
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
                if !valid_issue_key(k) {
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
        let f = match check_requirement(ctx, x) {
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
        // Cases name a requirement of the file by its number there (`REQ-<n>`) or by its issue
        // key; the number it gets in the project is never a key (with numbers not kept, it can
        // be another record's number in the file).
        let issue_key = f.checked.issue_key.clone();
        let remember = |planner: &mut Planner, id: Uuid, number: i64| {
            if let Some(n) = x.number {
                planner.rec.req_by_number.insert(n, (id, number));
                planner.rec.req_by_key.insert(format!("REQ-{n}"), id);
            }
            if let Some(k) = &issue_key {
                planner.rec.req_by_key.insert(k.clone(), id);
            }
        };
        if let Some((id, n)) = planner.rec.maps.requirements.get(&x.row).copied() {
            remember(planner, id, n);
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        // Creates and moves of requirements take this lock first (R's order).
        lock_project(&mut tx, ctx.project_id, "requirements").await?;
        let existing: Option<(Uuid, i64, bool)> = match &f.checked.issue_key {
            Some(k) => {
                sqlx::query_as(
                    "select id, number, archived_at is not null from requirements \
                     where project_id = $1 and coalesce(tracker_id, '') = $2 and issue_key = $3",
                )
                .bind(ctx.project_id)
                .bind(f.checked.tracker_id.as_deref().unwrap_or(""))
                .bind(k)
                .fetch_optional(&mut *tx)
                .await?
            }
            None => match x.number {
                Some(n) => {
                    sqlx::query_as(
                        "select id, number, false from requirements \
                         where project_id = $1 and number = $2 and title = $3 and archived_at is null",
                    )
                    .bind(ctx.project_id)
                    .bind(n)
                    .bind(&f.checked.title)
                    .fetch_optional(&mut *tx)
                    .await?
                }
                None => None,
            },
        };
        let (id, number) = match existing {
            Some((_, number, true)) => {
                planner.warn(
                    x.row,
                    "requirement_archived",
                    format!(
                        "REQ-{number} with this issue key is archived; the record was not imported and cases are not linked to it"
                    ),
                );
                return Ok(());
            }
            Some((id, number, false)) => {
                if planner.opts.mode == ImportMode::Upsert {
                    let cur =
                        requirements::lock_by_number(&mut tx, ctx.project_id, number, None).await?;
                    let c = f.checked;
                    requirements::change_in(
                        &mut tx,
                        ctx,
                        &cur,
                        Change {
                            title: Some(c.title),
                            description: Some(c.description),
                            req_type: Some(c.req_type),
                            status: Some(c.status),
                            parent: None,
                            tracker: Some((cur.tracker_id.clone(), cur.issue_key.clone(), c.url)),
                        },
                    )
                    .await?;
                    set_external_state(&mut tx, id, f.external_state.as_deref()).await?;
                }
                (id, number)
            }
            None => {
                let parent = match x.parent {
                    None => None,
                    Some(p) => match planner.rec.req_by_number.get(&p).copied() {
                        Some((_, parent_number)) => Some(parent_number),
                        None => {
                            planner.warn(
                                x.row,
                                "parent_not_found",
                                format!(
                                    "the parent {p} is not in the file or was not imported; imported as a root"
                                ),
                            );
                            None
                        }
                    },
                };
                if planner.rec.maps.keep_requirement_numbers.is_none() {
                    planner.rec.maps.keep_requirement_numbers = Some(planner.rec.keep_numbers);
                    planner.rec.dirty = true;
                }
                let kept = match x.number {
                    Some(n)
                        if planner.rec.keep_numbers
                            && n > 0
                            && planner.rec.used_numbers.insert(n) =>
                    {
                        Some(n)
                    }
                    _ => None,
                };
                let row = requirements::create_in(
                    &mut tx,
                    ctx,
                    f.checked,
                    parent,
                    kept,
                    RequirementOrigin::Import,
                )
                .await?;
                set_external_state(&mut tx, row.id, f.external_state.as_deref()).await?;
                (row.id, row.number)
            }
        };
        tx.commit().await?;
        planner.rec.maps.requirements.insert(x.row, (id, number));
        planner.rec.dirty = true;
        remember(planner, id, number);
        Ok(())
    }
}

/// The external state of an imported requirement (a tracker's, kept as the file says; it is
/// not a content change and does not bump the revision, like the tracker's state sync).
async fn set_external_state(
    conn: &mut PgConnection,
    id: Uuid,
    state: Option<&str>,
) -> TmResult<()> {
    sqlx::query(
        "update requirements set external_state = $2 where id = $1 and external_state is distinct from $2",
    )
    .bind(id)
    .bind(state)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tm::parameters::valid_column;
    use fastproof_tm_import::params::unique_names;

    /// The foreign formats' parameter names (Qase, TestOps) are made of `[A-Za-z0-9_]`, start
    /// with a letter or `_` and have at most 64 bytes: a subset of P's column rule, so a
    /// sanitized name is always a valid column and its rewritten `${name}` a valid placeholder.
    #[test]
    fn sanitized_parameter_names_are_valid_columns() {
        let names: Vec<String> = [
            "User role",
            "9 lives",
            "",
            "   ",
            "café",
            "a.b-c",
            "${x}",
            "-dash",
            "名前",
            &"x".repeat(200),
            "User role",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let out = unique_names(&names);
        for (n, o) in names.iter().zip(&out) {
            assert!(valid_column(o), "{n:?} became {o:?}");
        }
        let distinct: HashSet<&String> = out.iter().collect();
        assert_eq!(distinct.len(), out.len(), "{out:?}");
    }

    #[test]
    fn imported_rows_get_valid_unique_ids() {
        let p = ImportedParameters {
            columns: vec!["a".into()],
            rows: ["r_keep", "", "bad id", "r_keep", "r_x"]
                .iter()
                .map(|id| fastproof_tm_import::ImportedParameterRow {
                    id: id.to_string(),
                    values: BTreeMap::from([("a".to_string(), id.to_string())]),
                    enabled: true,
                })
                .collect(),
            generator: None,
        };
        let c = case_parameters(&p);
        let ids: Vec<&str> = c.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids[0], "r_keep");
        assert_eq!(ids[4], "r_x");
        assert!(ids.iter().all(|i| valid_row_id(i)), "{ids:?}");
        assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 5, "{ids:?}");
        let enabled: Vec<bool> = c.rows.iter().map(|r| r.enabled).collect();
        assert_eq!(
            enabled,
            vec![true, true, true, false, true],
            "the second row with the values of the first (r_keep) is switched off"
        );
    }
}
