//! Exports of cases (T3): the cases of a filter (at most [`super::import::MAX_EXPORT_CASES`])
//! read in pages as [`ImportedCase`]s, the shape the FastProof NDJSON format writes and the
//! importer reads back. Attachments are named `<id>-<file name>` (the entry under
//! `attachments/` of a zip export).

use std::collections::HashMap;

use fastproof_tm_import::{
    ImportedAttachment, ImportedCase, ImportedFieldDef, ImportedKey, ImportedLink, ImportedStep,
    ImportedValue,
};
use sqlx::Row;
use uuid::Uuid;

use super::cases::{CaseSelection, filter_numbers, selection_numbers};
use super::fields::field_defs;
use super::lookup::{CASE_COLS, case_row};
use super::{CaseFieldDef, CaseFilter, CaseRow, CaseStep, FieldValue, TmError, TmResult};
use crate::Store;

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

fn steps_of(steps: &[CaseStep], names: &HashMap<Uuid, String>) -> Vec<ImportedStep> {
    steps
        .iter()
        .map(|s| ImportedStep {
            id: s.id.clone(),
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
        self.export_source_query(project_id, filter, None, "", max)
            .await
    }

    /// The cases of a filter or, when given, of an FPQL query (exclusive; `me` is the caller
    /// for `me()`), not archived, as [`Self::export_source`].
    pub async fn export_source_query(
        &self,
        project_id: Uuid,
        filter: &CaseFilter,
        query: Option<&str>,
        me: &str,
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
                    &super::query::QueryCtx::new(me),
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
        conn.commit().await?;
        Ok(ExportSource {
            numbers,
            defs,
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
            };
            out.push((ic, atts));
        }
        Ok(out)
    }
}
