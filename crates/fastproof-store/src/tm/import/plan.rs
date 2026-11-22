//! The database side of an import: the same planner computes the dry-run report and applies
//! it. A case matches an existing one through its external keys (`case_keys`): `createOnly`
//! skips matches, `upsert` updates them with a version `source = import` (unchanged content
//! is skipped). Folder paths are created under the target folder, field values are mapped to
//! the field definitions (unknown fields are reported and left out; select values that are
//! not options are created, skipped or fail the row as the options say), numbers are kept for
//! TestOps and FastProof sources in a project without cases, and automated TestOps cases with
//! a full name get their code identity at once.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use fastproof_tm_import::{ImportedCase, ImportedFieldDef, ImportedStep, ImportedValue, RowError};
use serde_json::json;
use sqlx::{Acquire, PgConnection};
use uuid::Uuid;

use super::super::cases::draft_when_required_missing;
use super::super::cases::{
    CaseCtx, NewCaseMeta, create_case_in, lock_case_by_number, prepare_write, reserve_numbers,
    update_case_in, valid_link_url, valid_step_id,
};
use super::super::fields::{
    ImportColumn, MAX_IMPORT_COLUMNS, field_defs, import_field_name, valid_date,
};
use super::super::folders::ensure_folder_path;
use super::super::keys::{case_by_key, set_case_key};
use super::super::versions::{changed_keys, snapshot_of};
use super::super::{
    CaseBody, CaseFieldDef, CaseFieldPatch, CaseFieldWrite, CaseKeyKind, CaseLink, CaseLinkType,
    CaseOrigin, CaseRow, CaseStatus, CaseStep, CaseWrite, FieldOption, FieldType, FieldValue,
    ImportMode, ImportReport, ImportRowError, TmError, TmResult, UnknownFieldValues, VersionSource,
};
use super::MAX_REPORT_ERRORS;
use crate::Store;
use crate::tm::lookup::case_by_id;

/// What the planner needs to know about the import.
#[derive(Clone, Debug)]
pub struct PlanOptions {
    pub mode: ImportMode,
    pub unknown_values: UnknownFieldValues,
    pub target_folder: Option<Uuid>,
    /// Source numbers may be kept (TestOps, FastProof NDJSON).
    pub numbered_source: bool,
    /// Code identities from full names (TestOps).
    pub identities: bool,
    /// Status of created cases (`draft`/`active`); `None`: the source's, else `active`.
    pub status: Option<super::super::ImportCaseStatus>,
}

/// One import run (dry or applying): field mapping, folder cache, report.
pub struct Planner {
    pub opts: PlanOptions,
    pub(crate) defs: Vec<CaseFieldDef>,
    by_name: HashMap<String, usize>,
    pub(crate) folders: HashMap<(Option<Uuid>, String), Uuid>,
    virtual_folders: HashMap<(Option<Uuid>, String), Uuid>,
    seen: HashMap<(String, String), bool>,
    pub(crate) used_numbers: HashSet<i64>,
    unknown_fields: BTreeSet<String>,
    unknown_values: BTreeMap<String, BTreeSet<String>>,
    /// Unknown columns with their values (by normalized name), for `unknownFieldValues: create`.
    pub unknown_columns: BTreeMap<String, ImportColumn>,
    pub report: ImportReport,
    /// Errors beyond the kept ones.
    pub errors_total: u64,
    /// Keep source numbers (decided at the start: the project has no cases).
    pub keep_numbers: bool,
    pub max_number: Option<i64>,
    existing_cases: i64,
    pub(crate) target_depth: usize,
}

const NIL: Uuid = Uuid::nil();

fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

fn err_of(row: u64, e: TmError) -> Result<RowError, TmError> {
    match e {
        TmError::Invalid { code, message, .. } | TmError::Conflict { code, message, .. } => {
            Ok(RowError::new(row, code, message))
        }
        TmError::NotFound(what) => Ok(RowError::new(row, "not_found", format!("{what} not found"))),
        TmError::Stale { .. } => Ok(RowError::new(
            row,
            "precondition_failed",
            "the case changed meanwhile",
        )),
        other => Err(other),
    }
}

fn steps_of(
    steps: &[ImportedStep],
    attachments: &HashMap<String, Uuid>,
    cur: Option<&[CaseStep]>,
) -> Vec<CaseStep> {
    steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let prev = cur.and_then(|c| c.get(i));
            let id = if valid_step_id(&s.id) {
                s.id.clone()
            } else {
                prev.map(|p| p.id.clone()).unwrap_or_default()
            };
            CaseStep {
                id,
                action: s.action.clone(),
                expected: s.expected.clone(),
                data: s.data.clone(),
                attachments: s
                    .attachments
                    .iter()
                    .filter_map(|a| attachments.get(a).copied())
                    .collect(),
                steps: steps_of(&s.steps, attachments, prev.map(|p| p.steps.as_slice())),
            }
        })
        .collect()
}

/// Rewrites Markdown references to attachments of the import (`(name)`, `(old-id)` and
/// `attachment:<old-id>`) to `attachment:<new id>`.
fn rewrite(md: &str, c: &ImportedCase, attachments: &HashMap<String, Uuid>) -> String {
    if c.attachments.is_empty() || !md.contains("](") {
        return md.to_string();
    }
    let mut out = md.to_string();
    for a in &c.attachments {
        let Some(id) = attachments.get(&a.source) else {
            continue;
        };
        let new = format!("](attachment:{id})");
        let mut from = vec![format!("]({})", a.source), format!("]({})", a.name)];
        if let Some(old) = &a.old_id {
            from.push(format!("](attachment:{old})"));
        }
        for f in from {
            if f.len() > 3 && out.contains(&f) {
                out = out.replace(&f, &new);
            }
        }
    }
    out
}

impl Planner {
    /// Starts a run: reads the field definitions and the folders of the project.
    pub async fn new(
        conn: &mut PgConnection,
        project_id: Uuid,
        opts: PlanOptions,
    ) -> TmResult<Self> {
        let defs = field_defs(conn, project_id, false).await?;
        let rows: Vec<(Uuid, Option<Uuid>, String)> =
            sqlx::query_as("select id, parent_id, name from case_folders where project_id = $1")
                .bind(project_id)
                .fetch_all(&mut *conn)
                .await?;
        let folders = rows
            .into_iter()
            .map(|(id, p, n)| ((p, norm(&n)), id))
            .collect();
        let existing_cases: i64 =
            sqlx::query_scalar("select count(*) from cases where project_id = $1")
                .bind(project_id)
                .fetch_one(&mut *conn)
                .await?;
        let target_depth: i32 = match opts.target_folder {
            Some(f) => sqlx::query_scalar::<_, i16>(
                "select depth from case_folders where project_id = $1 and id = $2",
            )
            .bind(project_id)
            .bind(f)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or(TmError::NotFound("folder"))? as i32,
            None => 0,
        };
        let mut p = Self {
            keep_numbers: opts.numbered_source && existing_cases == 0,
            opts,
            defs: Vec::new(),
            by_name: HashMap::new(),
            folders,
            virtual_folders: HashMap::new(),
            seen: HashMap::new(),
            used_numbers: HashSet::new(),
            unknown_fields: BTreeSet::new(),
            unknown_values: BTreeMap::new(),
            unknown_columns: BTreeMap::new(),
            report: ImportReport::default(),
            errors_total: 0,
            max_number: None,
            existing_cases,
            target_depth: target_depth.max(0) as usize,
        };
        p.set_defs(defs);
        Ok(p)
    }

    fn set_defs(&mut self, defs: Vec<CaseFieldDef>) {
        self.by_name.clear();
        for (i, d) in defs.iter().enumerate() {
            self.by_name.entry(norm(&d.key)).or_insert(i);
        }
        for (i, d) in defs.iter().enumerate() {
            self.by_name.entry(norm(&d.name)).or_insert(i);
            if let Some(l) = &d.source_label {
                self.by_name.entry(norm(l)).or_insert(i);
            }
        }
        self.defs = defs;
    }

    pub async fn reload_defs(&mut self, conn: &mut PgConnection, project_id: Uuid) -> TmResult<()> {
        let defs = field_defs(conn, project_id, false).await?;
        self.set_defs(defs);
        Ok(())
    }

    /// Records a row error (the first [`MAX_REPORT_ERRORS`] are kept).
    pub fn error(&mut self, e: RowError) {
        self.errors_total += 1;
        if self.report.errors.len() < MAX_REPORT_ERRORS {
            self.report.errors.push(ImportRowError {
                row: e.row as i64,
                code: e.code,
                message: e.message.chars().take(500).collect(),
            });
        }
    }

    /// Records a row warning (the first [`MAX_REPORT_ERRORS`] are kept).
    pub fn warn(&mut self, row: u64, code: &str, message: String) {
        if self.report.warnings.len() < MAX_REPORT_ERRORS {
            self.report.warnings.push(ImportRowError {
                row: row as i64,
                code: code.to_string(),
                message: message.chars().take(500).collect(),
            });
        }
    }

    /// The report with the unknown fields and values. With `unknownFieldValues: create` the
    /// unknown columns are listed as fields the import creates, not as unknown fields.
    pub fn finish_report(&mut self) -> ImportReport {
        let mut r = self.report.clone();
        r.unknown_fields = self.unknown_fields.iter().cloned().collect();
        if self.opts.unknown_values == UnknownFieldValues::Create {
            for c in self.unknown_columns.values() {
                r.unknown_fields.retain(|f| f.trim() != c.name.trim());
                let name = import_field_name(&c.name);
                if r.created_fields.len() < MAX_IMPORT_COLUMNS
                    && !r.created_fields.iter().any(|f| f == &name)
                {
                    r.created_fields.push(name);
                }
            }
        }
        r.unknown_values = self
            .unknown_values
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
            .collect();
        r
    }

    fn option_of(def: &CaseFieldDef, v: &str) -> Option<String> {
        let t = v.trim();
        def.options
            .iter()
            .find(|o| !o.archived && o.value.eq_ignore_ascii_case(t))
            .map(|o| o.value.clone())
    }

    fn unknown_value(&mut self, def_key: &str, v: &str) {
        let set = self.unknown_values.entry(def_key.to_string()).or_default();
        if set.len() < 100 {
            set.insert(v.chars().take(200).collect());
        }
    }

    /// Field values of a case on the definitions; `Err` fails the row (`unknownFieldValues:
    /// fail`).
    pub(crate) fn fields(
        &mut self,
        c: &ImportedCase,
    ) -> Result<BTreeMap<String, FieldValue>, RowError> {
        let mut out = BTreeMap::new();
        for (name, v) in &c.fields {
            let Some(&i) = self.by_name.get(&norm(name)) else {
                if self.unknown_fields.len() < 1000 {
                    self.unknown_fields.insert(name.chars().take(200).collect());
                }
                if self.opts.unknown_values == UnknownFieldValues::Create {
                    let n = norm(name);
                    if self.unknown_columns.contains_key(&n)
                        || self.unknown_columns.len() < MAX_IMPORT_COLUMNS
                    {
                        let (texts, many, number) = match v {
                            ImportedValue::Text(s) => (vec![s.clone()], false, false),
                            ImportedValue::Many(m) => (m.clone(), true, false),
                            ImportedValue::Number(x) => (vec![x.to_string()], false, true),
                        };
                        self.unknown_columns
                            .entry(n)
                            .or_insert_with(|| ImportColumn {
                                name: name.chars().take(200).collect(),
                                ..Default::default()
                            })
                            .observe(&texts, many, number);
                    }
                }
                continue;
            };
            let def = self.defs[i].clone();
            let texts: Vec<String> = match v {
                ImportedValue::Text(s) => vec![s.clone()],
                ImportedValue::Many(m) => m.clone(),
                ImportedValue::Number(n) => vec![if n.fract() == 0.0 {
                    format!("{}", *n as i64)
                } else {
                    n.to_string()
                }],
            };
            let value = match def.field_type {
                FieldType::Text => FieldValue::Text(texts.join(", ")),
                FieldType::Number => match v {
                    ImportedValue::Number(n) => FieldValue::Number(*n),
                    _ => match texts
                        .first()
                        .and_then(|t| t.trim().parse::<f64>().ok())
                        .filter(|n| n.is_finite())
                    {
                        Some(n) => FieldValue::Number(n),
                        None => {
                            self.unknown_value(&def.key, &texts.join(", "));
                            continue;
                        }
                    },
                },
                FieldType::Date => {
                    let t = texts
                        .first()
                        .map(|t| t.trim().chars().take(10).collect::<String>())
                        .unwrap_or_default();
                    if valid_date(&t) {
                        FieldValue::Text(t)
                    } else {
                        self.unknown_value(&def.key, &texts.join(", "));
                        continue;
                    }
                }
                FieldType::Select | FieldType::Multiselect => {
                    let mut vals: Vec<String> = Vec::new();
                    let parts: Vec<String> = if def.field_type == FieldType::Multiselect {
                        texts
                            .iter()
                            .flat_map(|t| fastproof_tm_import::split_list(t))
                            .collect()
                    } else {
                        texts.iter().take(1).map(|t| t.trim().to_string()).collect()
                    };
                    for p in parts {
                        match Self::option_of(&def, &p) {
                            Some(o) => vals.push(o),
                            None => {
                                self.unknown_value(&def.key, &p);
                                match self.opts.unknown_values {
                                    UnknownFieldValues::Create => {}
                                    UnknownFieldValues::Skip => continue,
                                    UnknownFieldValues::Fail => {
                                        return Err(RowError::new(
                                            c.row,
                                            "unknown_value",
                                            format!("{p:?} is not an option of {}", def.key),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    if vals.is_empty() {
                        continue;
                    }
                    if def.field_type == FieldType::Select {
                        FieldValue::Text(vals.swap_remove(0))
                    } else {
                        FieldValue::Many(vals)
                    }
                }
            };
            out.insert(def.key.clone(), value);
        }
        Ok(out)
    }

    /// The folder of a case in a dry run: `Ok(Some(id))` existing, `Ok(None)` the target or
    /// new folders (counted).
    fn dry_folder(
        &mut self,
        c: &ImportedCase,
        max_depth: usize,
        max_name: usize,
    ) -> Result<Option<Uuid>, RowError> {
        if self.target_depth + c.folder.len() > max_depth {
            return Err(RowError::new(
                c.row,
                "limit_reached",
                format!("folders deeper than {max_depth} levels"),
            ));
        }
        let mut parent = self.opts.target_folder;
        let mut real = true;
        for n in &c.folder {
            if n.trim().is_empty() || n.trim().len() > max_name || n.chars().any(char::is_control) {
                return Err(RowError::new(
                    c.row,
                    "invalid_case",
                    "a folder name is empty, too long or has control characters",
                ));
            }
            let k = (parent, norm(n));
            if let Some(id) = self.folders.get(&k) {
                parent = Some(*id);
                continue;
            }
            real = false;
            let next = match self.virtual_folders.get(&k) {
                Some(id) => *id,
                None => {
                    let id = Uuid::now_v7();
                    self.virtual_folders.insert(k, id);
                    self.report.folders += 1;
                    id
                }
            };
            parent = Some(next);
        }
        Ok(if real { parent } else { None })
    }

    pub(crate) async fn apply_folder(
        &mut self,
        conn: &mut PgConnection,
        ctx: &CaseCtx<'_>,
        c: &ImportedCase,
    ) -> TmResult<Option<Uuid>> {
        let mut parent = self.opts.target_folder;
        for n in &c.folder {
            let k = (parent, norm(n));
            parent = match self.folders.get(&k) {
                Some(id) => Some(*id),
                None => {
                    let id =
                        ensure_folder_path(conn, ctx.project_id, parent, &[n.as_str()], ctx.limits)
                            .await?;
                    if let Some(id) = id {
                        self.folders.insert(k, id);
                        self.report.folders += 1;
                    }
                    id
                }
            };
        }
        Ok(parent)
    }

    async fn existing(
        &mut self,
        conn: &mut PgConnection,
        project_id: Uuid,
        c: &ImportedCase,
    ) -> TmResult<Option<Uuid>> {
        for k in &c.keys {
            let Some(kind) = CaseKeyKind::parse(&k.kind) else {
                continue;
            };
            if kind == CaseKeyKind::AllureId {
                continue;
            }
            if let Some(id) = case_by_key(conn, project_id, kind, &k.value).await? {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Whether an earlier row of this import had one of the keys (`true` = it was created).
    fn seen_before(&mut self, c: &ImportedCase) -> Option<bool> {
        let mut found = None;
        for k in &c.keys {
            if let Some(v) = self.seen.get(&(k.kind.clone(), k.value.clone())) {
                found = Some(*v);
            }
        }
        for k in &c.keys {
            self.seen
                .entry((k.kind.clone(), k.value.clone()))
                .or_insert(true);
        }
        found
    }

    pub(crate) fn write_of(
        &self,
        c: &ImportedCase,
        folder: Option<Uuid>,
        fields: BTreeMap<String, FieldValue>,
        attachments: &HashMap<String, Uuid>,
        cur: Option<&CaseRow>,
    ) -> CaseWrite {
        let folder = if c.folder.is_empty() && self.opts.target_folder.is_none() {
            cur.and_then(|r| r.folder_id)
        } else {
            folder
        };
        let links = c
            .links
            .iter()
            .filter(|l| valid_link_url(l.url.trim()))
            .take(100)
            .map(|l| CaseLink {
                name: l.name.chars().take(500).collect(),
                url: l.url.trim().to_string(),
                link_type: CaseLinkType::parse(&l.kind).unwrap_or(CaseLinkType::Link),
            })
            .collect();
        let mut merged = cur.map(|r| r.fields.clone()).unwrap_or_default();
        merged.extend(fields);
        let tags: Vec<String> = c
            .tags
            .iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        let status = match (self.opts.status, cur) {
            (Some(s), None) => CaseStatus::parse(s.as_str()),
            _ => c.status.as_deref().and_then(CaseStatus::parse),
        };
        CaseWrite {
            name: c.name.clone(),
            folder_id: folder,
            status,
            body: CaseBody {
                description: rewrite(&c.description, c, attachments),
                preconditions: rewrite(&c.preconditions, c, attachments),
                steps: steps_of(&c.steps, attachments, cur.map(|r| r.body.steps.as_slice())),
                links,
            },
            fields: Some(merged),
            tags: if tags.is_empty() && cur.is_some() {
                None
            } else {
                Some(tags)
            },
        }
    }

    /// A case whose required fields have no value is imported as `draft` with a warning.
    fn draft_if_required_missing(
        &mut self,
        c: &ImportedCase,
        w: &mut CaseWrite,
        cur: Option<&CaseRow>,
    ) {
        let missing = draft_when_required_missing(w, cur, &self.defs);
        if !missing.is_empty() {
            self.warn(
                c.row,
                "required_fields_missing",
                format!("imported as draft: no value for {}", missing.join(", ")),
            );
        }
    }

    /// Dry run of one case: counts it as create, update or skip, or records its row error.
    pub async fn dry_case(
        &mut self,
        conn: &mut PgConnection,
        ctx: &CaseCtx<'_>,
        c: &ImportedCase,
    ) -> TmResult<()> {
        if let (true, Some(n)) = (self.keep_numbers, c.number) {
            self.max_number = Some(self.max_number.map_or(n, |m| m.max(n)));
        }
        let fields = match self.fields(c) {
            Ok(f) => f,
            Err(e) => {
                self.error(e);
                return Ok(());
            }
        };
        let folder =
            match self.dry_folder(c, ctx.limits.max_folder_depth, ctx.limits.max_name_bytes) {
                Ok(f) => f,
                Err(e) => {
                    self.error(e);
                    return Ok(());
                }
            };
        let earlier = self.seen_before(c);
        let existing = match earlier {
            Some(_) => None,
            None => self.existing(conn, ctx.project_id, c).await?,
        };
        let cur = match existing {
            Some(id) => case_by_id(conn, ctx.project_id, id).await?,
            None => None,
        };
        let mut w = self.write_of(c, folder, fields, &HashMap::new(), cur.as_ref());
        self.draft_if_required_missing(c, &mut w, cur.as_ref());
        let snap = match prepare_write(conn, ctx, &w, cur.as_ref(), &self.defs).await {
            Ok(s) => s,
            Err(e) => {
                let e = err_of(c.row, e)?;
                self.error(e);
                return Ok(());
            }
        };
        match (earlier, cur, self.opts.mode) {
            (Some(_), _, ImportMode::CreateOnly) | (None, Some(_), ImportMode::CreateOnly) => {
                self.report.skip += 1
            }
            (Some(_), _, ImportMode::Upsert) => self.report.update += 1,
            (None, Some(cur), ImportMode::Upsert) => {
                let mut s = snap;
                if w.folder_id.is_none() && !c.folder.is_empty() {
                    s.folder_id = Some(NIL);
                }
                if changed_keys(&snapshot_of(&cur), &s).is_empty() {
                    self.report.skip += 1;
                } else {
                    self.report.update += 1;
                }
            }
            (None, None, _) => {
                self.report.create += 1;
                if self.existing_cases + self.report.create > ctx.limits.max_cases as i64 {
                    self.report.create -= 1;
                    self.error(RowError::new(
                        c.row,
                        "limit_reached",
                        format!("maxCases ({}) reached", ctx.limits.max_cases),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Applies one case inside the caller's transaction (a savepoint per case, so a failed
    /// row leaves the batch intact). `numbers` hands out reserved numbers.
    pub async fn apply_case(
        &mut self,
        tx: &mut PgConnection,
        ctx: &CaseCtx<'_>,
        c: &ImportedCase,
        attachments: &HashMap<String, Uuid>,
        numbers: &mut std::ops::Range<i64>,
    ) -> TmResult<()> {
        let fields = match self.fields(c) {
            Ok(f) => f,
            Err(e) => {
                self.error(e);
                return Ok(());
            }
        };
        if self.target_depth + c.folder.len() > ctx.limits.max_folder_depth {
            self.error(RowError::new(
                c.row,
                "limit_reached",
                format!("folders deeper than {} levels", ctx.limits.max_folder_depth),
            ));
            return Ok(());
        }
        let mut sp = tx.begin().await?;
        let r = self
            .apply_inner(&mut sp, ctx, c, fields, attachments, numbers)
            .await;
        match r {
            Ok(()) => {
                sp.commit().await?;
                Ok(())
            }
            Err(e) => {
                sp.rollback().await?;
                let e = err_of(c.row, e)?;
                self.error(e);
                Ok(())
            }
        }
    }

    async fn apply_inner(
        &mut self,
        conn: &mut PgConnection,
        ctx: &CaseCtx<'_>,
        c: &ImportedCase,
        fields: BTreeMap<String, FieldValue>,
        attachments: &HashMap<String, Uuid>,
        numbers: &mut std::ops::Range<i64>,
    ) -> TmResult<()> {
        let folders_before = self.report.folders;
        let folder_cache = self.folders.clone();
        let r = self
            .apply_rows(conn, ctx, c, fields, attachments, numbers)
            .await;
        if r.is_err() {
            self.report.folders = folders_before;
            self.folders = folder_cache;
        }
        r
    }

    async fn apply_rows(
        &mut self,
        conn: &mut PgConnection,
        ctx: &CaseCtx<'_>,
        c: &ImportedCase,
        fields: BTreeMap<String, FieldValue>,
        attachments: &HashMap<String, Uuid>,
        numbers: &mut std::ops::Range<i64>,
    ) -> TmResult<()> {
        let folder = self.apply_folder(conn, ctx, c).await?;
        let existing = self.existing(conn, ctx.project_id, c).await?;
        let row = match existing {
            Some(id) => {
                if self.opts.mode == ImportMode::CreateOnly {
                    self.report.skip += 1;
                    return Ok(());
                }
                let number: i64 = sqlx::query_scalar("select number from cases where id = $1")
                    .bind(id)
                    .fetch_one(&mut *conn)
                    .await?;
                let cur = lock_case_by_number(conn, ctx.project_id, number, None).await?;
                let mut w = self.write_of(c, folder, fields, attachments, Some(&cur));
                self.draft_if_required_missing(c, &mut w, Some(&cur));
                let after = update_case_in(conn, ctx, &cur, &w, VersionSource::Import).await?;
                if after.revision == cur.revision {
                    self.report.skip += 1;
                } else {
                    self.report.update += 1;
                }
                after
            }
            None => {
                let mut w = self.write_of(c, folder, fields, attachments, None);
                self.draft_if_required_missing(c, &mut w, None);
                let snap = prepare_write(conn, ctx, &w, None, &self.defs).await?;
                let number = match c.number {
                    Some(n) if self.keep_numbers && n > 0 && self.used_numbers.insert(n) => {
                        Some((n, false))
                    }
                    _ => numbers.next().map(|n| (n, true)),
                };
                let automated = self.opts.identities && c.automated && !c.full_names.is_empty();
                let meta = NewCaseMeta {
                    origin: CaseOrigin::Import,
                    source: VersionSource::Import,
                    launch_id: None,
                    ai_unreviewed: false,
                    automated,
                    code_hash: None,
                    number,
                };
                let row = create_case_in(conn, ctx, snap, &meta).await?;
                self.report.create += 1;
                if automated {
                    for f in c.full_names.iter().take(10) {
                        code_identity(conn, ctx.project_id, row.id, f, &row.name).await?;
                    }
                }
                row
            }
        };
        for k in &c.keys {
            if let Some(kind) = CaseKeyKind::parse(&k.kind)
                && k.value.len() <= super::super::keys::MAX_KEY_BYTES
                && !k.value.chars().any(char::is_control)
            {
                set_case_key(conn, ctx.project_id, row.id, kind, &k.value).await?;
            }
        }
        if self.opts.identities
            && let Some(n) = c.number
        {
            set_case_key(
                conn,
                ctx.project_id,
                row.id,
                CaseKeyKind::AllureId,
                &n.to_string(),
            )
            .await?;
        }
        let ids: Vec<Uuid> = c
            .attachments
            .iter()
            .filter_map(|a| attachments.get(&a.source).copied())
            .collect();
        if !ids.is_empty() {
            link_case_attachments(conn, ctx, row.id, &ids).await?;
        }
        Ok(())
    }

    /// Adds the select options the dry run reported as unknown (`unknownFieldValues: create`).
    pub async fn create_options(
        &mut self,
        store: &Store,
        ctx: &CaseCtx<'_>,
        unknown: &BTreeMap<String, Vec<String>>,
    ) -> TmResult<()> {
        for (key, values) in unknown {
            let Some(def) = self.defs.iter().find(|d| d.key == *key).cloned() else {
                continue;
            };
            if !matches!(def.field_type, FieldType::Select | FieldType::Multiselect) {
                continue;
            }
            let mut opts = def.options.clone();
            for v in values {
                let v = v.trim();
                if v.is_empty()
                    || v.len() > super::super::fields::MAX_OPTION_BYTES
                    || opts.iter().any(|o| o.value.eq_ignore_ascii_case(v))
                {
                    continue;
                }
                opts.push(FieldOption {
                    value: v.to_string(),
                    color: None,
                    archived: false,
                });
            }
            if opts.len() != def.options.len() {
                store
                    .update_case_field(
                        ctx.project_id,
                        def.id,
                        &CaseFieldPatch {
                            options: Some(opts),
                            ..Default::default()
                        },
                        ctx.limits,
                    )
                    .await?;
            }
        }
        let mut conn = store.pool.acquire().await?;
        self.reload_defs(&mut conn, ctx.project_id).await
    }

    /// Creates the field definitions an NDJSON header carries that the project lacks; in a
    /// dry run reports them as unknown fields.
    pub async fn field_defs_from_source(
        &mut self,
        store: &Store,
        ctx: &CaseCtx<'_>,
        defs: &[ImportedFieldDef],
        apply: bool,
    ) -> TmResult<()> {
        for d in defs {
            if self.defs.iter().any(|x| x.key == d.key) {
                continue;
            }
            let Some(t) = FieldType::parse(&d.field_type) else {
                continue;
            };
            if !apply || self.opts.unknown_values != UnknownFieldValues::Create {
                self.unknown_fields.insert(d.key.clone());
                continue;
            }
            let w = CaseFieldWrite {
                key: d.key.clone(),
                name: d.name.clone(),
                field_type: t,
                options: Some(
                    d.options
                        .iter()
                        .map(|o| FieldOption {
                            value: o.clone(),
                            color: None,
                            archived: false,
                        })
                        .collect(),
                ),
                required: None,
                default_value: None,
                source_label: d.source_label.clone().map(Some),
                position: None,
            };
            store
                .create_case_field(ctx.project_id, &w, ctx.limits)
                .await?;
        }
        let mut conn = store.pool.acquire().await?;
        self.reload_defs(&mut conn, ctx.project_id).await
    }

    /// Raises `case_seq` to the largest kept number before cases with reserved numbers are
    /// created, so reserved numbers never meet a kept one later in the file.
    pub async fn raise_seq(&self, store: &Store, project_id: Uuid) -> TmResult<()> {
        if let (true, Some(m)) = (self.keep_numbers, self.max_number) {
            sqlx::query("update projects set case_seq = greatest(case_seq, $2) where id = $1")
                .bind(project_id)
                .bind(m)
                .execute(&store.pool)
                .await?;
        }
        Ok(())
    }

    /// Reserves `k` numbers in a statement of their own.
    pub async fn reserve(
        &self,
        store: &Store,
        ctx: &CaseCtx<'_>,
        k: i64,
    ) -> TmResult<std::ops::Range<i64>> {
        if k <= 0 {
            return Ok(0..0);
        }
        let first = reserve_numbers(&store.pool, ctx.project_id, k, ctx.limits).await?;
        Ok(first..first + k)
    }
}

/// The code identity of an automated case from its full name (`md5(fullName)`, as Allure
/// hashes a result without `testCaseId`), linked to the case unless another case holds it.
pub async fn code_identity(
    conn: &mut PgConnection,
    project_id: Uuid,
    case_id: Uuid,
    full_name: &str,
    name: &str,
) -> anyhow::Result<()> {
    let hash = fastproof_model::identity::md5_hex(full_name);
    sqlx::query(
        "insert into test_cases (id, project_id, test_case_hash, full_name, display_name, kind, case_id, \
           first_seen_at, last_seen_at) values ($1, $2, $3, $4, $5, 'code', $6, now(), now()) \
         on conflict (project_id, test_case_hash) do update set case_id = coalesce(test_cases.case_id, excluded.case_id)",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(&hash)
    .bind(crate::no_nul(full_name))
    .bind(crate::no_nul(name))
    .bind(case_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn link_case_attachments(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    case_id: Uuid,
    ids: &[Uuid],
) -> TmResult<()> {
    sqlx::query(
        "insert into case_attachments (case_id, attachment_id, name, content_type, created_by) \
         select $1, a.id, coalesce(a.original_name, 'attachment'), a.content_type, $3 \
         from attachments a where a.project_id = $2 and a.id = any($4) \
         on conflict (case_id, attachment_id) do nothing",
    )
    .bind(case_id)
    .bind(ctx.project_id)
    .bind(crate::no_nul(ctx.by))
    .bind(ids)
    .execute(&mut *conn)
    .await?;
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
    Ok(())
}

impl Store {
    /// Stores the row of an imported attachment (content-addressed per project) whose blob the
    /// caller already wrote; returns its id.
    pub async fn import_attachment(
        &self,
        project_id: Uuid,
        sha256: &str,
        size: i64,
        content_type: &str,
        name: &str,
        blob_key: &str,
    ) -> anyhow::Result<Uuid> {
        Ok(sqlx::query_scalar(
            "insert into attachments (id, project_id, sha256, size, content_type, original_name, blob_key) \
             values ($1, $2, $3, $4, $5, $6, $7) \
             on conflict (project_id, sha256) do update set last_seen_at = now() returning id",
        )
        .bind(Uuid::now_v7())
        .bind(project_id)
        .bind(sha256)
        .bind(size)
        .bind(crate::no_nul(content_type))
        .bind(crate::no_nul(name))
        .bind(blob_key)
        .fetch_one(&self.pool)
        .await?)
    }

    /// Applies a batch of cases in one transaction with the job's progress, so a resumed
    /// job continues after the last committed batch. New cases are inserted together (one
    /// statement per table); when that fails, they are applied one by one so the failing rows
    /// become row errors. Returns `false` when the job is no longer the worker's (cancelled).
    #[allow(clippy::too_many_arguments)]
    pub async fn apply_import_batch(
        &self,
        planner: &mut Planner,
        ctx: &CaseCtx<'_>,
        job: Uuid,
        owner: &str,
        cases: &[ImportedCase],
        attachments: &HashMap<String, Uuid>,
        done: i64,
        total: Option<i64>,
    ) -> TmResult<bool> {
        let mut tx = self.pool.begin().await?;
        let ok: Option<i32> = sqlx::query_scalar(
            "select 1 from tm_jobs where id = $1 and lease_owner = $2 and status = 'running' for update",
        )
        .bind(job)
        .bind(owner)
        .fetch_optional(&mut *tx)
        .await?;
        if ok.is_none() {
            return Ok(false);
        }
        let snapshot = (
            planner.report.clone(),
            planner.errors_total,
            planner.folders.clone(),
            planner.used_numbers.clone(),
        );
        let r = self
            .batch_in(&mut tx, planner, ctx, cases, attachments)
            .await;
        if let Err(e) = r {
            planner.report = snapshot.0;
            planner.errors_total = snapshot.1;
            planner.folders = snapshot.2;
            planner.used_numbers = snapshot.3;
            return Err(e);
        }
        sqlx::query(
            "update tm_jobs set progress = $2, report = $3, lease_until = now() + make_interval(secs => $4) where id = $1",
        )
        .bind(job)
        .bind(json!({"done": done, "total": total}))
        .bind(serde_json::to_value(planner.finish_report())?)
        .bind(super::LEASE_SECONDS as f64)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    async fn batch_in(
        &self,
        tx: &mut PgConnection,
        planner: &mut Planner,
        ctx: &CaseCtx<'_>,
        cases: &[ImportedCase],
        attachments: &HashMap<String, Uuid>,
    ) -> TmResult<()> {
        let known = known_keys(tx, ctx.project_id, cases).await?;
        let mut pending: Vec<(&ImportedCase, super::super::CaseSnapshot)> = Vec::new();
        let mut pending_keys: HashSet<(String, String)> = HashSet::new();
        for c in cases {
            let ks: Vec<(String, String)> = c
                .keys
                .iter()
                .map(|k| (k.kind.clone(), k.value.clone()))
                .collect();
            let has_known = ks.iter().any(|k| known.contains(k));
            let dup = ks.iter().any(|k| pending_keys.contains(k));
            if has_known || dup {
                if dup {
                    self.flush_creates(tx, planner, ctx, &mut pending, attachments)
                        .await?;
                    pending_keys.clear();
                }
                let mut none = 0..0;
                planner
                    .apply_case(tx, ctx, c, attachments, &mut none)
                    .await?;
                continue;
            }
            let fields = match planner.fields(c) {
                Ok(f) => f,
                Err(e) => {
                    planner.error(e);
                    continue;
                }
            };
            if planner.target_depth + c.folder.len() > ctx.limits.max_folder_depth {
                planner.error(RowError::new(
                    c.row,
                    "limit_reached",
                    format!("folders deeper than {} levels", ctx.limits.max_folder_depth),
                ));
                continue;
            }
            if c.folder
                .iter()
                .any(|n| check_folder_name(n, ctx.limits.max_name_bytes).is_err())
            {
                planner.error(RowError::new(
                    c.row,
                    "invalid_case",
                    "a folder name is empty, too long or has control characters",
                ));
                continue;
            }
            let folder = planner.apply_folder(tx, ctx, c).await?;
            let mut w = planner.write_of(c, folder, fields, attachments, None);
            planner.draft_if_required_missing(c, &mut w, None);
            match prepare_write(tx, ctx, &w, None, &planner.defs).await {
                Ok(snap) => {
                    pending_keys.extend(ks);
                    pending.push((c, snap));
                }
                Err(e) => {
                    let e = err_of(c.row, e)?;
                    planner.error(e);
                }
            }
        }
        self.flush_creates(tx, planner, ctx, &mut pending, attachments)
            .await
    }

    async fn flush_creates(
        &self,
        tx: &mut PgConnection,
        planner: &mut Planner,
        ctx: &CaseCtx<'_>,
        pending: &mut Vec<(&ImportedCase, super::super::CaseSnapshot)>,
        attachments: &HashMap<String, Uuid>,
    ) -> TmResult<()> {
        if pending.is_empty() {
            return Ok(());
        }
        let items = std::mem::take(pending);
        let mut sp = tx.begin().await?;
        let used = planner.used_numbers.clone();
        match bulk_create(&mut sp, planner, ctx, &items, attachments).await {
            Ok(n) => {
                sp.commit().await?;
                planner.report.create += n;
                Ok(())
            }
            Err(TmError::Other(e)) if !is_unique(&e) => Err(TmError::Other(e)),
            Err(_) => {
                sp.rollback().await?;
                planner.used_numbers = used;
                let k = items.len() as i64;
                let mut numbers = planner.reserve(self, ctx, k).await?;
                for (c, _) in items {
                    planner
                        .apply_case(tx, ctx, c, attachments, &mut numbers)
                        .await?;
                }
                Ok(())
            }
        }
    }
}

fn check_folder_name(n: &str, max: usize) -> Result<(), ()> {
    let t = n.trim();
    if t.is_empty() || t.len() > max || t.chars().any(char::is_control) {
        return Err(());
    }
    Ok(())
}

fn is_unique(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<sqlx::Error>()
            .and_then(|s| match s {
                sqlx::Error::Database(d) => d.code(),
                _ => None,
            })
            .is_some_and(|code| code == "23505")
    })
}

/// The `(kind, value)` keys of the batch that some case of the project already holds.
async fn known_keys(
    conn: &mut PgConnection,
    project_id: Uuid,
    cases: &[ImportedCase],
) -> TmResult<HashSet<(String, String)>> {
    let mut kinds = Vec::new();
    let mut values = Vec::new();
    for c in cases {
        for k in &c.keys {
            if k.kind != "allure_id" {
                kinds.push(k.kind.clone());
                values.push(k.value.clone());
            }
        }
    }
    if kinds.is_empty() {
        return Ok(HashSet::new());
    }
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select k.kind, k.value from case_keys k join unnest($2::text[], $3::text[]) as t(kind, value) \
         on k.kind = t.kind and k.value = t.value where k.project_id = $1",
    )
    .bind(project_id)
    .bind(&kinds)
    .bind(&values)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Inserts new cases together: rows, version 1, keys, manual identities, code identities,
/// attachments and `case.updated` events (one live notification for the batch).
async fn bulk_create(
    conn: &mut PgConnection,
    planner: &mut Planner,
    ctx: &CaseCtx<'_>,
    items: &[(&ImportedCase, super::super::CaseSnapshot)],
    attachments: &HashMap<String, Uuid>,
) -> TmResult<i64> {
    let need = items
        .iter()
        .filter(|(c, _)| !(planner.keep_numbers && c.number.is_some_and(|n| n > 0)))
        .count() as i64;
    let first: i64 = sqlx::query_scalar(
        "update projects set case_seq = case_seq + $2 where id = $1 returning case_seq",
    )
    .bind(ctx.project_id)
    .bind(need)
    .fetch_one(&mut *conn)
    .await?;
    let mut next = first - need + 1;
    let mut ids = Vec::with_capacity(items.len());
    let mut numbers = Vec::with_capacity(items.len());
    let mut names = Vec::with_capacity(items.len());
    let mut statuses = Vec::with_capacity(items.len());
    let mut automated = Vec::with_capacity(items.len());
    let mut folders: Vec<Option<Uuid>> = Vec::with_capacity(items.len());
    let mut bodies = Vec::with_capacity(items.len());
    let mut fields = Vec::with_capacity(items.len());
    let mut tags = Vec::with_capacity(items.len());
    let mut search = Vec::with_capacity(items.len());
    let mut snaps = Vec::with_capacity(items.len());
    let mut max_kept = 0i64;
    for (c, snap) in items {
        let number = match c.number {
            Some(n) if planner.keep_numbers && n > 0 && planner.used_numbers.insert(n) => {
                max_kept = max_kept.max(n);
                n
            }
            Some(n) if planner.keep_numbers && n > 0 => {
                return Err(TmError::conflict(
                    "case_number_taken",
                    format!("TC-{n} twice"),
                ));
            }
            _ => {
                let n = next;
                next += 1;
                n
            }
        };
        ids.push(Uuid::now_v7());
        numbers.push(number);
        names.push(crate::no_nul(&snap.name));
        statuses.push(snap.status.as_str().to_string());
        automated.push(planner.opts.identities && c.automated && !c.full_names.is_empty());
        folders.push(snap.folder_id);
        bodies.push(crate::json_no_nul(serde_json::to_value(&snap.body)?));
        fields.push(crate::json_no_nul(serde_json::to_value(&snap.fields)?));
        tags.push(serde_json::to_value(&snap.tags)?);
        search.push(crate::no_nul(&super::super::cases::search_text(
            &snap.name, &snap.body,
        )));
        snaps.push(crate::codec::encode(snap)?);
    }
    if max_kept > 0 {
        sqlx::query("update projects set case_seq = greatest(case_seq, $2) where id = $1")
            .bind(ctx.project_id)
            .bind(max_kept)
            .execute(&mut *conn)
            .await?;
    }
    let count: i64 = sqlx::query_scalar("select count(*) from cases where project_id = $1")
        .bind(ctx.project_id)
        .fetch_one(&mut *conn)
        .await?;
    if count as usize + items.len() > ctx.limits.max_cases {
        return Err(TmError::limit("maxCases", ctx.limits.max_cases as u64));
    }
    sqlx::query(
        "insert into cases (id, project_id, number, name, status, automated, origin, ai_unreviewed, folder_id, \
           body, fields, tags, search_text, last_trusted_at, created_by, updated_by) \
         select i, $1, n, nm, st, a, 'import', false, f, b, fl, array(select jsonb_array_elements_text(tg)), s, \
           case when a then now() end, $2, $2 \
         from unnest($3::uuid[], $4::bigint[], $5::text[], $6::text[], $7::bool[], $8::uuid[], $9::jsonb[], \
           $10::jsonb[], $11::jsonb[], $12::text[]) as t(i, n, nm, st, a, f, b, fl, tg, s)",
    )
    .bind(ctx.project_id)
    .bind(crate::no_nul(ctx.by))
    .bind(&ids)
    .bind(&numbers)
    .bind(&names)
    .bind(&statuses)
    .bind(&automated)
    .bind(&folders)
    .bind(&bodies)
    .bind(&fields)
    .bind(&tags)
    .bind(&search)
    .execute(&mut *conn)
    .await?;
    let all: Vec<String> = super::super::versions::CONTENT_KEYS
        .iter()
        .map(|k| k.to_string())
        .collect();
    sqlx::query(
        "insert into case_versions (case_id, version, project_id, source, launch_id, changed, snapshot_z, created_by) \
         select c, 1, $1, 'import', null, $2, z, $3 from unnest($4::uuid[], $5::bytea[]) as t(c, z)",
    )
    .bind(ctx.project_id)
    .bind(&all)
    .bind(crate::no_nul(ctx.by))
    .bind(&ids)
    .bind(&snaps)
    .execute(&mut *conn)
    .await?;
    let mut k_case = Vec::new();
    let mut k_kind = Vec::new();
    let mut k_value = Vec::new();
    for (i, (c, _)) in items.iter().enumerate() {
        let mut push = |kind: &str, value: String| {
            if !value.is_empty()
                && value.len() <= super::super::keys::MAX_KEY_BYTES
                && !value.chars().any(char::is_control)
            {
                k_case.push(ids[i]);
                k_kind.push(kind.to_string());
                k_value.push(value);
            }
        };
        push("allure_id", numbers[i].to_string());
        for k in &c.keys {
            if CaseKeyKind::parse(&k.kind).is_some() {
                push(&k.kind, k.value.clone());
            }
        }
        if planner.opts.identities
            && let Some(n) = c.number
            && n != numbers[i]
        {
            push("allure_id", n.to_string());
        }
    }
    sqlx::query(
        "insert into case_keys (project_id, kind, value, case_id) \
         select $1, k, v, c from unnest($2::uuid[], $3::text[], $4::text[]) as t(c, k, v) \
         on conflict (project_id, kind, value) do nothing",
    )
    .bind(ctx.project_id)
    .bind(&k_case)
    .bind(&k_kind)
    .bind(&k_value)
    .execute(&mut *conn)
    .await?;
    let manual: Vec<(Uuid, i64, String)> = (0..ids.len())
        .map(|i| (ids[i], numbers[i], names[i].clone()))
        .collect();
    super::super::runs::manual_identities(conn, ctx.project_id, &manual).await?;
    let mut events = Vec::with_capacity(items.len());
    for (i, (c, snap)) in items.iter().enumerate() {
        if automated[i] {
            for f in c.full_names.iter().take(10) {
                code_identity(conn, ctx.project_id, ids[i], f, &snap.name).await?;
            }
        }
        let mut att: Vec<Uuid> = c
            .attachments
            .iter()
            .filter_map(|a| attachments.get(&a.source).copied())
            .collect();
        att.extend(super::super::cases::referenced_attachments(&snap.body));
        att.sort();
        att.dedup();
        if !att.is_empty() {
            link_case_attachments(conn, ctx, ids[i], &att).await?;
        }
        events.push(json!({"number": numbers[i], "revision": 1, "source": "import"}));
    }
    crate::events::emit_many(conn, "case.updated", Some(ctx.project_key), events).await?;
    crate::events::notify(
        conn,
        &crate::events::LiveEvent {
            project: ctx.project_key.to_string(),
            kind: "case.updated".into(),
            data: json!({"source": "import", "count": items.len()}),
        },
    )
    .await?;
    Ok(items.len() as i64)
}

impl Store {
    /// Starts a planner for an import of the project.
    pub async fn import_planner(&self, project_id: Uuid, opts: PlanOptions) -> TmResult<Planner> {
        let mut conn = self.pool.acquire().await?;
        Planner::new(&mut conn, project_id, opts).await
    }

    /// Dry run of one case (see [`Planner::dry_case`]).
    pub async fn dry_import_case(
        &self,
        planner: &mut Planner,
        ctx: &CaseCtx<'_>,
        c: &ImportedCase,
    ) -> TmResult<()> {
        let mut conn = self.pool.acquire().await?;
        planner.dry_case(&mut conn, ctx, c).await
    }

    /// Secrets of TestOps imports (`tm-import-<job id>`) whose job is over (or never got
    /// created: older than 10 minutes without a job).
    pub async fn stale_import_secrets(&self) -> anyhow::Result<Vec<(Uuid, String)>> {
        Ok(sqlx::query_as(
            "select s.project_id, s.name from project_secrets s where s.name like 'tm-import-%' \
             and s.updated_at < now() - interval '10 minutes' \
             and not exists (select 1 from tm_jobs j where j.project_id = s.project_id \
               and 'tm-import-' || j.id::text = s.name and j.status in ('queued', 'dry_run', 'ready', 'running')) \
             limit 1000",
        )
        .fetch_all(&self.pool)
        .await?)
    }
}
