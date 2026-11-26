//! Exports of cases (T3): the cases of a filter (at most [`super::import::MAX_EXPORT_CASES`])
//! read in pages as [`ImportedCase`]s, the shape the FastProof NDJSON format writes and the
//! importer reads back. Attachments are named `<id>-<file name>` (the entry under
//! `attachments/` of a zip export).

use std::collections::HashMap;

use fastproof_tm_import::{
    ImportedAttachment, ImportedCase, ImportedFieldDef, ImportedGenerator, ImportedKey,
    ImportedLink, ImportedParameterRow, ImportedParameters, ImportedRequirement,
    ImportedRequirementRef, ImportedSharedStep, ImportedStep, ImportedValue,
};
use futures::TryStreamExt;
use sqlx::Row;
use uuid::Uuid;

use super::cases::{CaseSelection, filter_numbers, selection_numbers};
use super::fields::field_defs;
use super::lookup::{CASE_COLS, case_row};
use super::{
    CaseFieldDef, CaseFilter, CaseParameters, CaseRow, CaseStep, FieldValue, TmError, TmResult,
};
use crate::Store;

/// Bytes of shared step content one export holds (it keeps them for the CSV expansion).
pub const MAX_EXPORT_SHARED_BYTES: usize = 64 * 1024 * 1024;
/// Requirements read per page of an export.
pub const REQUIREMENT_PAGE: i64 = 1000;

/// An attachment of an exported case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportAttachment {
    pub id: Uuid,
    pub name: String,
    pub blob_key: String,
    pub size: i64,
}

/// What an export reads besides the cases: folder names and field definitions.
#[derive(Clone, Debug, Default)]
pub struct ExportSource {
    pub numbers: Vec<i64>,
    pub defs: Vec<CaseFieldDef>,
    /// The export covers every case of the project: all shared steps and requirements go with
    /// it. Otherwise only the shared steps the cases reference and the requirements they
    /// cover (with their ancestors).
    pub all: bool,
    /// The requirements of a partial export (`None`: all).
    pub requirement_ids: Option<Vec<Uuid>>,
    folders: HashMap<Uuid, (Option<Uuid>, String)>,
}

impl ExportSource {
    fn path(&self, mut id: Option<Uuid>) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(f) = id {
            let Some((parent, name)) = self.folders.get(&f) else {
                break;
            };
            out.push(name.clone());
            id = *parent;
            if out.len() > 100 {
                break;
            }
        }
        out.reverse();
        out
    }

    /// The field definitions as the NDJSON header carries them.
    pub fn field_defs(&self) -> Vec<ImportedFieldDef> {
        self.defs
            .iter()
            .map(|d| ImportedFieldDef {
                key: d.key.clone(),
                name: d.name.clone(),
                field_type: d.field_type.as_str().to_string(),
                options: d
                    .options
                    .iter()
                    .filter(|o| !o.archived)
                    .map(|o| o.value.clone())
                    .collect(),
                source_label: d.source_label.clone(),
            })
            .collect()
    }

    /// Field keys in their order (CSV columns).
    pub fn field_keys(&self) -> Vec<String> {
        self.defs.iter().map(|d| d.key.clone()).collect()
    }
}

/// The file name part of an attachment entry (`<id>-<name>`, path characters replaced).
pub fn attachment_entry(id: Uuid, name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .take(150)
        .collect();
    let clean = clean.trim_start_matches('.').to_string();
    format!(
        "{id}-{}",
        if clean.is_empty() {
            "attachment".into()
        } else {
            clean
        }
    )
}

/// Parameters of a case body in the shape of the NDJSON records.
pub fn imported_parameters(p: &CaseParameters) -> ImportedParameters {
    ImportedParameters {
        columns: p.columns.clone(),
        rows: p
            .rows
            .iter()
            .map(|r| ImportedParameterRow {
                id: r.id.clone(),
                values: r.values.clone(),
                enabled: r.enabled,
            })
            .collect(),
        generator: p.generator.as_ref().map(|g| ImportedGenerator {
            mode: g.mode.as_str().to_string(),
            values: g.values.clone(),
        }),
    }
}

fn steps_of(steps: &[CaseStep], names: &HashMap<Uuid, String>) -> Vec<ImportedStep> {
    steps
        .iter()
        .map(|s| ImportedStep {
            id: s.id.clone(),
            shared: s.shared.as_ref().map(|r| ImportedKey {
                kind: "number".into(),
                value: r.number.to_string(),
            }),
            action: s.action.clone(),
            expected: s.expected.clone(),
            data: s.data.clone(),
            attachments: s
                .attachments
                .iter()
                .filter_map(|a| names.get(a).cloned())
                .collect(),
            steps: steps_of(&s.steps, names),
        })
        .collect()
}

fn value_of(v: &FieldValue) -> Option<ImportedValue> {
    match v {
        FieldValue::Text(s) => Some(ImportedValue::Text(s.clone())),
        FieldValue::Many(m) => Some(ImportedValue::Many(m.clone())),
        FieldValue::Number(n) => Some(ImportedValue::Number(*n)),
        FieldValue::Null => None,
    }
}

impl Store {
    /// The numbers of the cases a filter exports (409 `limit_reached` over `max`), with the
    /// folder names and field definitions.
    pub async fn export_source(
        &self,
        project_id: Uuid,
        filter: &CaseFilter,
        max: usize,
    ) -> TmResult<ExportSource> {
        self.export_source_query(
            project_id,
            filter,
            None,
            &super::query::QueryCtx::default(),
            max,
        )
        .await
    }

    /// The cases of a filter or, when given, of an FPQL query (exclusive; `who` is the caller
    /// for `me()` and personal saved queries), not archived even when the query names
    /// `archived`, as [`Self::export_source`].
    pub async fn export_source_query(
        &self,
        project_id: Uuid,
        filter: &CaseFilter,
        query: Option<&str>,
        who: &super::query::QueryCtx,
        max: usize,
    ) -> TmResult<ExportSource> {
        let mut conn = self.pool.begin().await?;
        let numbers = match query.filter(|q| !q.trim().is_empty()) {
            Some(src) => {
                if *filter != CaseFilter::default() {
                    return Err(TmError::bad_request("give either a filter or a query"));
                }
                super::query::set_timeout(&mut conn).await?;
                let q = super::query::prepare(
                    &mut conn,
                    project_id,
                    super::QueryEntity::Cases,
                    src,
                    who,
                )
                .await?;
                selection_numbers(
                    &mut conn,
                    project_id,
                    CaseSelection::Query(&q),
                    Some(false),
                    max,
                )
                .await?
            }
            None => filter_numbers(&mut conn, project_id, filter, Some(false), max).await?,
        };
        if numbers.len() > max {
            return Err(TmError::limit("maxExportCases", max as u64));
        }
        let rows: Vec<(Uuid, Option<Uuid>, String)> =
            sqlx::query_as("select id, parent_id, name from case_folders where project_id = $1")
                .bind(project_id)
                .fetch_all(&mut *conn)
                .await?;
        let defs = field_defs(&mut conn, project_id, false).await?;
        let all = query.is_none_or(|q| q.trim().is_empty()) && *filter == CaseFilter::default();
        let requirement_ids = if all {
            None
        } else {
            Some(
                sqlx::query_scalar(
                    "select distinct a from requirement_links l \
                     join requirements r on r.id = l.requirement_id \
                     join cases c on c.id = l.case_id \
                     cross join lateral unnest(r.path_ids) as a \
                     where c.project_id = $1 and c.number = any($2) and r.archived_at is null",
                )
                .bind(project_id)
                .bind(&numbers)
                .fetch_all(&mut *conn)
                .await?,
            )
        };
        conn.commit().await?;
        Ok(ExportSource {
            numbers,
            defs,
            all,
            requirement_ids,
            folders: rows.into_iter().map(|(id, p, n)| (id, (p, n))).collect(),
        })
    }

    /// One page of exported cases (by number) with their attachments.
    pub async fn export_page(
        &self,
        project_id: Uuid,
        src: &ExportSource,
        numbers: &[i64],
    ) -> TmResult<Vec<(ImportedCase, Vec<ExportAttachment>)>> {
        let mut conn = self.pool.acquire().await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {CASE_COLS} from cases where project_id = $1 and number = any($2) order by number"
        )))
        .bind(project_id)
        .bind(numbers)
        .fetch_all(&mut *conn)
        .await?;
        let cases: Vec<CaseRow> = rows.iter().map(case_row).collect::<sqlx::Result<_>>()?;
        let ids: Vec<Uuid> = cases.iter().map(|c| c.id).collect();
        let keys: Vec<(Uuid, String, String)> = sqlx::query_as(
            "select case_id, kind, value from case_keys where case_id = any($1) order by kind, value",
        )
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?;
        let mut keys_of: HashMap<Uuid, Vec<ImportedKey>> = HashMap::new();
        for (c, kind, value) in keys {
            keys_of
                .entry(c)
                .or_default()
                .push(ImportedKey { kind, value });
        }
        let full: Vec<(Uuid, String)> = sqlx::query_as(
            "select case_id, full_name from test_cases where case_id = any($1) and kind = 'code' order by full_name",
        )
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?;
        let mut full_of: HashMap<Uuid, Vec<String>> = HashMap::new();
        for (c, f) in full {
            full_of.entry(c).or_default().push(f);
        }
        let att = sqlx::query(
            "select ca.case_id, a.id, ca.name, a.blob_key, a.size from case_attachments ca \
             join attachments a on a.id = ca.attachment_id where ca.case_id = any($1) order by ca.created_at",
        )
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?;
        let reqs: Vec<(Uuid, i64, String, String)> = sqlx::query_as(
            "select l.case_id, r.number, l.origin, l.status from requirement_links l \
             join requirements r on r.id = l.requirement_id \
             where l.case_id = any($1) and r.archived_at is null order by r.number",
        )
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?;
        let mut reqs_of: HashMap<Uuid, Vec<ImportedRequirementRef>> = HashMap::new();
        for (c, n, origin, status) in reqs {
            reqs_of.entry(c).or_default().push(ImportedRequirementRef {
                key: format!("REQ-{n}"),
                origin: Some(origin),
                status: Some(status),
            });
        }
        let mut att_of: HashMap<Uuid, Vec<ExportAttachment>> = HashMap::new();
        for r in att {
            let blob: Option<String> = r.try_get("blob_key")?;
            att_of
                .entry(r.try_get("case_id")?)
                .or_default()
                .push(ExportAttachment {
                    id: r.try_get("id")?,
                    name: r.try_get("name")?,
                    blob_key: blob.unwrap_or_default(),
                    size: r.try_get("size")?,
                });
        }
        let mut out = Vec::with_capacity(cases.len());
        for c in cases {
            let atts = att_of.remove(&c.id).unwrap_or_default();
            let names: HashMap<Uuid, String> = atts
                .iter()
                .map(|a| (a.id, attachment_entry(a.id, &a.name)))
                .collect();
            let full_names = full_of.remove(&c.id).unwrap_or_default();
            let ic = ImportedCase {
                row: 0,
                keys: keys_of.remove(&c.id).unwrap_or_default(),
                number: Some(c.number),
                name: c.name.clone(),
                folder: src.path(c.folder_id),
                status: Some(c.status.as_str().to_string()),
                description: c.body.description.clone(),
                preconditions: c.body.preconditions.clone(),
                steps: steps_of(&c.body.steps, &names),
                links: c
                    .body
                    .links
                    .iter()
                    .map(|l| ImportedLink {
                        name: l.name.clone(),
                        url: l.url.clone(),
                        kind: l.link_type.as_str().to_string(),
                    })
                    .collect(),
                tags: c.tags.clone(),
                fields: c
                    .fields
                    .iter()
                    .filter_map(|(k, v)| value_of(v).map(|v| (k.clone(), v)))
                    .collect(),
                automated: c.automated,
                full_names,
                attachments: atts
                    .iter()
                    .map(|a| ImportedAttachment {
                        name: a.name.clone(),
                        source: attachment_entry(a.id, &a.name),
                        old_id: Some(a.id.to_string()),
                    })
                    .collect(),
                parameters: c.body.parameters.as_ref().map(imported_parameters),
                requirements: reqs_of.remove(&c.id).unwrap_or_default(),
            };
            out.push((ic, atts));
        }
        Ok(out)
    }

    /// The shared steps of an export: those the cases of `numbers` reference at their head
    /// version and, when the export covers every case, all that are not archived. Ordered by
    /// number; at most `max_bytes` of steps JSON in all (409 `limit_reached`), read row by row
    /// so that no more than that is held.
    pub async fn export_shared_steps(
        &self,
        project_id: Uuid,
        src: &ExportSource,
        max_bytes: usize,
    ) -> TmResult<Vec<ImportedSharedStep>> {
        let mut conn = self.pool.acquire().await?;
        let any: bool =
            sqlx::query_scalar("select exists (select 1 from shared_steps where project_id = $1)")
                .bind(project_id)
                .fetch_one(&mut *conn)
                .await?;
        if !any {
            return Ok(Vec::new());
        }
        let mut out: Vec<(Uuid, ImportedSharedStep)> = Vec::new();
        let mut bytes = 0usize;
        {
            let mut rows = sqlx::query(
                "with refs as (select distinct (jsonb_path_query(c.body, '$.steps[*].shared.id') #>> '{}')::uuid as id \
                   from cases c where c.project_id = $1 and c.number = any($2)) \
                 select s.id, s.number, s.name, s.description, s.steps from shared_steps s \
                 where s.project_id = $1 and (s.id in (select id from refs) or ($3 and s.archived_at is null)) \
                 order by s.number",
            )
            .bind(project_id)
            .bind(&src.numbers)
            .bind(src.all)
            .fetch(&mut *conn);
            while let Some(r) = rows.try_next().await? {
                let steps: sqlx::types::Json<Vec<CaseStep>> = r.try_get("steps")?;
                let steps = steps_of(&steps.0, &HashMap::new());
                bytes += serde_json::to_vec(&steps)?.len();
                if bytes > max_bytes {
                    return Err(TmError::limit("maxExportSharedStepBytes", max_bytes as u64));
                }
                out.push((
                    r.try_get("id")?,
                    ImportedSharedStep {
                        row: 0,
                        number: Some(r.try_get("number")?),
                        keys: Vec::new(),
                        name: r.try_get("name")?,
                        description: r.try_get("description")?,
                        steps,
                    },
                ));
            }
        }
        let ids: Vec<Uuid> = out.iter().map(|(id, _)| *id).collect();
        let keys: Vec<(Uuid, String, String)> = sqlx::query_as(
            "select shared_step_id, kind, value from shared_step_keys where shared_step_id = any($1) order by kind, value",
        )
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?;
        let mut keys_of: HashMap<Uuid, Vec<ImportedKey>> = HashMap::new();
        for (id, kind, value) in keys {
            keys_of
                .entry(id)
                .or_default()
                .push(ImportedKey { kind, value });
        }
        Ok(out
            .into_iter()
            .map(|(id, mut x)| {
                x.keys = keys_of.remove(&id).unwrap_or_default();
                x
            })
            .collect())
    }

    /// One page of the requirements of an export, parents before children (ordered by depth,
    /// then number); `after` is the position of the last one of the previous page.
    pub async fn export_requirements(
        &self,
        project_id: Uuid,
        src: &ExportSource,
        after: Option<(i16, i64)>,
        limit: i64,
    ) -> TmResult<Vec<((i16, i64), ImportedRequirement)>> {
        let (d, n) = after.unwrap_or((0, 0));
        let rows = sqlx::query(
            "select r.number, r.depth, r.title, r.description, r.type, r.status, p.number as parent, \
               r.tracker_id, r.issue_key, r.url, r.external_state \
             from requirements r left join requirements p on p.id = r.parent_id and p.archived_at is null \
             where r.project_id = $1 and r.archived_at is null \
               and ($2::uuid[] is null or r.id = any($2)) and (r.depth, r.number) > ($3, $4) \
             order by r.depth, r.number limit $5",
        )
        .bind(project_id)
        .bind(src.requirement_ids.as_deref())
        .bind(d)
        .bind(n)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let number: i64 = r.try_get("number")?;
                let depth: i16 = r.try_get("depth")?;
                Ok((
                    (depth, number),
                    ImportedRequirement {
                        row: 0,
                        number: Some(number),
                        title: r.try_get("title")?,
                        description: r.try_get("description")?,
                        req_type: r.try_get("type")?,
                        status: Some(r.try_get("status")?),
                        parent: r.try_get("parent")?,
                        tracker_id: r.try_get("tracker_id")?,
                        issue_key: r.try_get("issue_key")?,
                        url: r.try_get("url")?,
                        external_state: r.try_get("external_state")?,
                    },
                ))
            })
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }
}
