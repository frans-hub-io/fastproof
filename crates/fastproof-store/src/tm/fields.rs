//! Case field definitions (`case_field_defs`): built-in fields created per project on first
//! use, custom fields with a fixed type, option and default-value validation, and the value
//! check that case writes use.

use std::collections::HashSet;

use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::{
    CaseFieldDef, CaseFieldPatch, CaseFieldWrite, FieldOption, FieldType, FieldValue, TmError,
    TmLimits, TmResult, check_name, lock_project, violated_constraint,
};
use crate::{Store, json_no_nul};

/// Bytes of an option value, at most.
pub const MAX_OPTION_BYTES: usize = 200;

/// A built-in field: key, name, type, options, Allure label.
pub struct BuiltInField {
    pub key: &'static str,
    pub name: &'static str,
    pub field_type: FieldType,
    pub options: &'static [&'static str],
    pub source_label: Option<&'static str>,
}

/// Fields every project has. Their key and type never change; they can be renamed, edited
/// and archived like custom fields.
pub const BUILT_IN_FIELDS: &[BuiltInField] = &[
    BuiltInField {
        key: "priority",
        name: "Priority",
        field_type: FieldType::Select,
        options: &["critical", "high", "medium", "low"],
        source_label: None,
    },
    BuiltInField {
        key: "severity",
        name: "Severity",
        field_type: FieldType::Select,
        options: &["blocker", "critical", "normal", "minor", "trivial"],
        source_label: Some("severity"),
    },
    BuiltInField {
        key: "layer",
        name: "Layer",
        field_type: FieldType::Text,
        options: &[],
        source_label: Some("layer"),
    },
    BuiltInField {
        key: "owner",
        name: "Owner",
        field_type: FieldType::Text,
        options: &[],
        source_label: Some("owner"),
    },
    BuiltInField {
        key: "epic",
        name: "Epic",
        field_type: FieldType::Text,
        options: &[],
        source_label: Some("epic"),
    },
    BuiltInField {
        key: "feature",
        name: "Feature",
        field_type: FieldType::Text,
        options: &[],
        source_label: Some("feature"),
    },
    BuiltInField {
        key: "story",
        name: "Story",
        field_type: FieldType::Text,
        options: &[],
        source_label: Some("story"),
    },
    BuiltInField {
        key: "suite",
        name: "Suite",
        field_type: FieldType::Text,
        options: &[],
        source_label: Some("suite"),
    },
    BuiltInField {
        key: "component",
        name: "Component",
        field_type: FieldType::Text,
        options: &[],
        source_label: Some("component"),
    },
];

const DEF_COLS: &str = "id, key, name, type, options, required, default_value, source_label, \
    built_in, position, archived_at is not null as archived";

fn def_row(r: &PgRow) -> sqlx::Result<CaseFieldDef> {
    let t: String = r.try_get("type")?;
    let options: serde_json::Value = r.try_get("options")?;
    let default: Option<serde_json::Value> = r.try_get("default_value")?;
    Ok(CaseFieldDef {
        id: r.try_get("id")?,
        key: r.try_get("key")?,
        name: r.try_get("name")?,
        field_type: FieldType::parse(&t).unwrap_or(FieldType::Text),
        options: serde_json::from_value(options).unwrap_or_default(),
        required: r.try_get("required")?,
        default_value: default
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default(),
        source_label: r.try_get("source_label")?,
        built_in: r.try_get("built_in")?,
        position: r.try_get("position")?,
        archived: r.try_get("archived")?,
    })
}

/// Field key: `^[a-z][a-z0-9_]{0,62}$`.
pub fn valid_key(k: &str) -> bool {
    let b = k.as_bytes();
    !b.is_empty()
        && b.len() <= 63
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// Allure label name mapped into a field: `^[A-Za-z][A-Za-z0-9_.-]{0,62}$`.
pub fn valid_label(l: &str) -> bool {
    let b = l.as_bytes();
    !b.is_empty()
        && b.len() <= 63
        && b[0].is_ascii_alphabetic()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

fn valid_color(c: &str) -> bool {
    let b = c.as_bytes();
    b.len() == 7 && b[0] == b'#' && b[1..].iter().all(u8::is_ascii_hexdigit)
}

/// An ISO date `yyyy-mm-dd`.
pub fn valid_date(s: &str) -> bool {
    s.len() == 10 && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// Checks the options of a field of type `t`.
pub fn check_options(t: FieldType, options: &[FieldOption], limits: &TmLimits) -> TmResult<()> {
    let selectable = matches!(t, FieldType::Select | FieldType::Multiselect);
    if !selectable && !options.is_empty() {
        return Err(TmError::bad_request(
            "options are only allowed on select and multiselect fields",
        ));
    }
    if options.len() > limits.max_field_options {
        return Err(TmError::limit(
            "maxFieldOptions",
            limits.max_field_options as u64,
        ));
    }
    let mut seen = HashSet::with_capacity(options.len());
    for o in options {
        if o.value.trim().is_empty()
            || o.value.len() > MAX_OPTION_BYTES
            || o.value.trim() != o.value
            || o.value.chars().any(char::is_control)
        {
            return Err(TmError::bad_request(format!(
                "option values must have 1 to {MAX_OPTION_BYTES} bytes without surrounding spaces or control characters"
            )));
        }
        if !seen.insert(o.value.as_str()) {
            return Err(TmError::bad_request(format!(
                "duplicate option {:?}",
                o.value
            )));
        }
        if let Some(c) = &o.color
            && !valid_color(c)
        {
            return Err(TmError::bad_request("option colors are #rrggbb"));
        }
    }
    Ok(())
}

/// Checks one value against a definition: type, active options, text size, finite number,
/// ISO date. `null` is always well-formed (requiredness is the case's check).
pub fn check_value(def: &CaseFieldDef, v: &FieldValue, limits: &TmLimits) -> Result<(), String> {
    let active = |s: &str| def.options.iter().any(|o| o.value == s && !o.archived);
    match (def.field_type, v) {
        (_, FieldValue::Null) => Ok(()),
        (FieldType::Select, FieldValue::Text(s)) if active(s) => Ok(()),
        (FieldType::Select, FieldValue::Text(s)) => {
            Err(format!("{s:?} is not an option of {}", def.key))
        }
        (FieldType::Multiselect, FieldValue::Many(vs)) => {
            if vs.len() > def.options.len().max(1) {
                return Err(format!("too many values for {}", def.key));
            }
            let mut seen = HashSet::new();
            for s in vs {
                if !active(s) {
                    return Err(format!("{s:?} is not an option of {}", def.key));
                }
                if !seen.insert(s.as_str()) {
                    return Err(format!("duplicate value {s:?} for {}", def.key));
                }
            }
            Ok(())
        }
        (FieldType::Text, FieldValue::Text(s)) if s.len() <= limits.max_markdown_bytes => Ok(()),
        (FieldType::Text, FieldValue::Text(_)) => Err(format!(
            "{} must have at most {} bytes",
            def.key, limits.max_markdown_bytes
        )),
        (FieldType::Number, FieldValue::Number(n)) if n.is_finite() => Ok(()),
        (FieldType::Date, FieldValue::Text(s)) if valid_date(s) => Ok(()),
        (FieldType::Date, _) => Err(format!("{} must be a date yyyy-mm-dd", def.key)),
        (t, _) => Err(format!("{} must be a {t} value", def.key)),
    }
}

/// Creates the built-in fields a project lacks (idempotent, cheap when all exist).
pub async fn ensure_built_in(conn: &mut PgConnection, project_id: Uuid) -> sqlx::Result<()> {
    let mut ids = Vec::new();
    let mut keys = Vec::new();
    let mut names = Vec::new();
    let mut types = Vec::new();
    let mut options = Vec::new();
    let mut labels: Vec<Option<String>> = Vec::new();
    let mut positions = Vec::new();
    for (i, b) in BUILT_IN_FIELDS.iter().enumerate() {
        ids.push(Uuid::now_v7());
        keys.push(b.key.to_string());
        names.push(b.name.to_string());
        types.push(b.field_type.as_str().to_string());
        let opts: Vec<FieldOption> = b
            .options
            .iter()
            .map(|v| FieldOption {
                value: v.to_string(),
                color: None,
                archived: false,
            })
            .collect();
        options.push(serde_json::to_value(opts).unwrap_or_default());
        labels.push(b.source_label.map(str::to_string));
        positions.push(i as i32);
    }
    sqlx::query(
        "insert into case_field_defs (id, project_id, key, name, type, options, source_label, built_in, position) \
         select t.i, $1, t.k, t.n, t.t, t.o, t.l, true, t.p \
         from unnest($2::uuid[], $3::text[], $4::text[], $5::text[], $6::jsonb[], $7::text[], $8::int[]) \
           as t(i, k, n, t, o, l, p) \
         where not exists (select 1 from case_field_defs d where d.project_id = $1 and d.key = t.k) \
         on conflict (project_id, key) do nothing",
    )
    .bind(project_id)
    .bind(&ids)
    .bind(&keys)
    .bind(&names)
    .bind(&types)
    .bind(&options)
    .bind(&labels)
    .bind(&positions)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The field definitions of a project (built-ins created first), by position and key.
pub async fn field_defs(
    conn: &mut PgConnection,
    project_id: Uuid,
    include_archived: bool,
) -> sqlx::Result<Vec<CaseFieldDef>> {
    ensure_built_in(conn, project_id).await?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {DEF_COLS} from case_field_defs where project_id = $1 and ($2 or archived_at is null) \
         order by position, key"
    )))
    .bind(project_id)
    .bind(include_archived)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(def_row).collect()
}

/// One field definition of the project.
pub async fn field_def(
    conn: &mut PgConnection,
    project_id: Uuid,
    id: Uuid,
) -> sqlx::Result<Option<CaseFieldDef>> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {DEF_COLS} from case_field_defs where project_id = $1 and id = $2"
    )))
    .bind(project_id)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    r.as_ref().map(def_row).transpose()
}

fn check_default(def: &CaseFieldDef, limits: &TmLimits) -> TmResult<()> {
    check_value(def, &def.default_value, limits)
        .map_err(|m| TmError::bad_request(format!("defaultValue: {m}")))
}

async fn label_taken(
    conn: &mut PgConnection,
    project_id: Uuid,
    label: &str,
    except: Option<Uuid>,
) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "select exists (select 1 from case_field_defs where project_id = $1 and source_label = $2 \
         and archived_at is null and id is distinct from $3)",
    )
    .bind(project_id)
    .bind(label)
    .bind(except)
    .fetch_one(&mut *conn)
    .await
}

fn key_taken(e: sqlx::Error) -> TmError {
    if violated_constraint(&e) == Some("case_field_defs_key_unique") {
        TmError::conflict("conflict", "a field with this key exists")
    } else {
        e.into()
    }
}

async fn store_def(
    conn: &mut PgConnection,
    project_id: Uuid,
    d: &CaseFieldDef,
    insert: bool,
) -> TmResult<()> {
    let sql = if insert {
        "insert into case_field_defs (id, project_id, key, name, type, options, required, default_value, \
           source_label, built_in, position, archived_at) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, false, $10, case when $11 then now() end)"
    } else {
        "update case_field_defs set key = $3, name = $4, options = $6, required = $7, default_value = $8, \
           source_label = $9, position = $10, updated_at = now(), \
           archived_at = case when not $11 then null else coalesce(archived_at, now()) end \
         where id = $1 and project_id = $2 and type = $5"
    };
    let default = (!d.default_value.is_null())
        .then(|| serde_json::to_value(&d.default_value))
        .transpose()?
        .map(json_no_nul);
    sqlx::query(sql)
        .bind(d.id)
        .bind(project_id)
        .bind(&d.key)
        .bind(&d.name)
        .bind(d.field_type.as_str())
        .bind(json_no_nul(serde_json::to_value(&d.options)?))
        .bind(d.required)
        .bind(default)
        .bind(&d.source_label)
        .bind(d.position)
        .bind(d.archived)
        .execute(&mut *conn)
        .await
        .map_err(key_taken)?;
    Ok(())
}

fn check_def(d: &CaseFieldDef, limits: &TmLimits) -> TmResult<()> {
    if !valid_key(&d.key) {
        return Err(TmError::bad_request(
            "key must match ^[a-z][a-z0-9_]{0,62}$",
        ));
    }
    check_name("name", &d.name, limits.max_name_bytes)?;
    if let Some(l) = &d.source_label
        && !valid_label(l)
    {
        return Err(TmError::bad_request(
            "sourceLabel must match ^[A-Za-z][A-Za-z0-9_.-]{0,62}$",
        ));
    }
    check_options(d.field_type, &d.options, limits)?;
    check_default(d, limits)
}

impl Store {
    /// Field definitions of a project.
    pub async fn case_fields(
        &self,
        project_id: Uuid,
        include_archived: bool,
    ) -> anyhow::Result<Vec<CaseFieldDef>> {
        let mut conn = self.pool.acquire().await?;
        Ok(field_defs(&mut conn, project_id, include_archived).await?)
    }

    /// Creates a custom field.
    pub async fn create_case_field(
        &self,
        project_id: Uuid,
        w: &CaseFieldWrite,
        limits: &TmLimits,
    ) -> TmResult<CaseFieldDef> {
        let d = CaseFieldDef {
            id: Uuid::now_v7(),
            key: w.key.clone(),
            name: w.name.trim().to_string(),
            field_type: w.field_type,
            options: w.options.clone().unwrap_or_default(),
            required: w.required.unwrap_or(false),
            default_value: w.default_value.clone().unwrap_or_default(),
            source_label: w.source_label.clone().flatten(),
            built_in: false,
            position: w.position.unwrap_or(1000),
            archived: false,
        };
        check_def(&d, limits)?;
        let mut tx = self.pool.begin().await?;
        lock_project(&mut tx, project_id, "fields").await?;
        ensure_built_in(&mut tx, project_id).await?;
        let n: i64 =
            sqlx::query_scalar("select count(*) from case_field_defs where project_id = $1")
                .bind(project_id)
                .fetch_one(&mut *tx)
                .await?;
        if n as usize >= limits.max_fields {
            return Err(TmError::limit("maxFields", limits.max_fields as u64));
        }
        if let Some(l) = &d.source_label
            && label_taken(&mut tx, project_id, l, None).await?
        {
            return Err(TmError::conflict(
                "conflict",
                "another field is mapped to this label",
            ));
        }
        store_def(&mut tx, project_id, &d, true).await?;
        tx.commit().await?;
        Ok(d)
    }

    /// Changes a field. The type cannot change (`type` may be sent only unchanged); built-in
    /// fields keep their key.
    pub async fn update_case_field(
        &self,
        project_id: Uuid,
        id: Uuid,
        p: &CaseFieldPatch,
        limits: &TmLimits,
    ) -> TmResult<CaseFieldDef> {
        let mut tx = self.pool.begin().await?;
        lock_project(&mut tx, project_id, "fields").await?;
        let mut d = field_def(&mut tx, project_id, id)
            .await?
            .ok_or(TmError::NotFound("field"))?;
        if let Some(t) = p.field_type
            && t != d.field_type
        {
            return Err(TmError::bad_request("the type of a field cannot change"));
        }
        if let Some(k) = &p.key
            && *k != d.key
        {
            if d.built_in {
                return Err(TmError::bad_request(
                    "the key of a built-in field cannot change",
                ));
            }
            d.key = k.clone();
        }
        if let Some(n) = &p.name {
            d.name = n.trim().to_string();
        }
        if let Some(o) = &p.options {
            d.options = o.clone();
        }
        if let Some(r) = p.required {
            d.required = r;
        }
        if let Some(v) = &p.default_value {
            d.default_value = v.clone();
        }
        if let Some(l) = &p.source_label {
            d.source_label = l.clone();
        }
        if let Some(pos) = p.position {
            d.position = pos;
        }
        if let Some(a) = p.archived {
            d.archived = a;
        }
        check_def(&d, limits)?;
        if let Some(l) = &d.source_label
            && !d.archived
            && label_taken(&mut tx, project_id, l, Some(d.id)).await?
        {
            return Err(TmError::conflict(
                "conflict",
                "another field is mapped to this label",
            ));
        }
        store_def(&mut tx, project_id, &d, false).await?;
        let d = field_def(&mut tx, project_id, id)
            .await?
            .ok_or(TmError::NotFound("field"))?;
        tx.commit().await?;
        Ok(d)
    }

    /// Archives a field (values on cases are kept; `PATCH {archived: false}` restores it).
    pub async fn delete_case_field(&self, project_id: Uuid, id: Uuid) -> TmResult<CaseFieldDef> {
        let mut tx = self.pool.begin().await?;
        let n = sqlx::query(
            "update case_field_defs set archived_at = coalesce(archived_at, now()), updated_at = now() \
             where project_id = $1 and id = $2",
        )
        .bind(project_id)
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if n == 0 {
            return Err(TmError::NotFound("field"));
        }
        let d = field_def(&mut tx, project_id, id)
            .await?
            .ok_or(TmError::NotFound("field"))?;
        tx.commit().await?;
        Ok(d)
    }
}

/// Distinct values of an import column, at most, for it to become a `select` field; a column
/// with more values (or a value longer than [`MAX_OPTION_BYTES`]) becomes a `text` field.
pub const MAX_IMPORT_SELECT_OPTIONS: usize = 50;
/// Unknown import columns that become fields, at most per import.
pub const MAX_IMPORT_COLUMNS: usize = 50;

/// An import column that matches no field definition, with what the import saw in it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportColumn {
    /// The column name as in the file (the new field's name).
    pub name: String,
    /// Distinct non-empty values (at most [`MAX_IMPORT_SELECT_OPTIONS`]); cleared once `text`.
    #[serde(default)]
    pub values: std::collections::BTreeSet<String>,
    /// Too many or too long values for a `select`.
    #[serde(default)]
    pub text: bool,
    /// A cell held a list (a `multiselect` unless `text`).
    #[serde(default)]
    pub many: bool,
    /// A cell held a number and every value parses as one.
    #[serde(default)]
    pub number: bool,
}

impl ImportColumn {
    /// Records the values of one cell (`many`: the cell was a list).
    pub fn observe(&mut self, values: &[String], many: bool, number: bool) {
        self.many |= many;
        if number && self.values.is_empty() && !self.text {
            self.number = true;
        }
        if !number {
            self.number = false;
        }
        if self.text {
            return;
        }
        for v in values {
            let v = v.trim();
            if v.is_empty() {
                continue;
            }
            if v.len() > MAX_OPTION_BYTES || v.chars().any(char::is_control) {
                self.text = true;
                self.values.clear();
                return;
            }
            if !self.values.iter().any(|x| x.eq_ignore_ascii_case(v)) {
                if self.values.len() >= MAX_IMPORT_SELECT_OPTIONS {
                    self.text = true;
                    self.values.clear();
                    return;
                }
                self.values.insert(v.to_string());
            }
        }
    }

    /// The type of the field the column becomes: `number` when every value was a number,
    /// `select`/`multiselect` with the seen values as options, else `text`.
    pub fn field_type(&self) -> FieldType {
        if self.number && !self.many {
            FieldType::Number
        } else if self.text || self.values.is_empty() {
            FieldType::Text
        } else if self.many {
            FieldType::Multiselect
        } else {
            FieldType::Select
        }
    }
}

/// A field key from a column name: lowercase ASCII letters, digits and `_`, starting with a
/// letter (`f_` is put in front otherwise), unique among `taken` (`_2`, `_3`, … appended).
pub fn import_field_key(name: &str, taken: &HashSet<String>) -> String {
    let mut k = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            k.push(c);
        } else if !k.ends_with('_') {
            k.push('_');
        }
    }
    let mut k = k.trim_matches('_').to_string();
    if !k.starts_with(|c: char| c.is_ascii_lowercase()) {
        k = format!("f_{k}").trim_end_matches('_').to_string();
    }
    k.truncate(56);
    let k = k.trim_end_matches('_').to_string();
    if !taken.contains(&k) {
        return k;
    }
    (2..)
        .map(|i| format!("{k}_{i}"))
        .find(|c| !taken.contains(c))
        .unwrap_or(k)
}

/// The name of the field an import column becomes: the column name, or for a column the parser
/// gave as a key (`automation_type`) a readable one (`Automation type`).
pub fn import_field_name(column: &str) -> String {
    let c: String = column.trim().chars().take(200).collect();
    if c.is_empty() || c.contains(char::is_whitespace) || c != c.to_lowercase() {
        return c;
    }
    let spaced = c.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().chain(chars).collect(),
        None => c,
    }
}

/// The definition an unknown import column becomes (see [`ImportColumn::field_type`]).
pub fn import_field_write(col: &ImportColumn, key: String, limits: &TmLimits) -> CaseFieldWrite {
    let t = col.field_type();
    let name = import_field_name(&col.name);
    let options = matches!(t, FieldType::Select | FieldType::Multiselect).then(|| {
        col.values
            .iter()
            .take(limits.max_field_options)
            .map(|v| FieldOption {
                value: v.clone(),
                color: None,
                archived: false,
            })
            .collect()
    });
    CaseFieldWrite {
        key,
        name: if name.is_empty() {
            "Field".into()
        } else {
            name
        },
        field_type: t,
        options,
        required: None,
        default_value: None,
        source_label: None,
        position: None,
    }
}

impl Store {
    /// Creates a field definition for each unknown import column (`unknownFieldValues:
    /// create`): the column name becomes the field name (so the import maps the column to
    /// it), the key comes from the name, the type from the values (see
    /// [`ImportColumn::field_type`]). At most [`MAX_IMPORT_COLUMNS`]; columns past `maxFields`
    /// or with a name an active field already has are left out. Returns the created fields.
    pub async fn create_import_fields(
        &self,
        project_id: Uuid,
        columns: &[ImportColumn],
        limits: &TmLimits,
    ) -> TmResult<Vec<CaseFieldDef>> {
        let mut conn = self.pool.acquire().await?;
        ensure_built_in(&mut conn, project_id).await?;
        let existing = field_defs(&mut conn, project_id, true).await?;
        drop(conn);
        let mut taken: HashSet<String> = existing.iter().map(|d| d.key.clone()).collect();
        let names: HashSet<String> = existing
            .iter()
            .filter(|d| !d.archived)
            .flat_map(|d| [d.key.to_lowercase(), d.name.trim().to_lowercase()])
            .collect();
        let mut out = Vec::new();
        for col in columns.iter().take(MAX_IMPORT_COLUMNS) {
            let name = col.name.trim().to_lowercase();
            if name.is_empty()
                || names.contains(&name)
                || names.contains(&import_field_name(&col.name).to_lowercase())
            {
                continue;
            }
            let key = import_field_key(&col.name, &taken);
            let w = import_field_write(col, key.clone(), limits);
            match self.create_case_field(project_id, &w, limits).await {
                Ok(d) => {
                    taken.insert(key);
                    out.push(d);
                }
                Err(TmError::Conflict {
                    code: "limit_reached",
                    ..
                }) => break,
                Err(TmError::Invalid { .. }) | Err(TmError::Conflict { .. }) => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(t: FieldType, opts: &[&str]) -> CaseFieldDef {
        CaseFieldDef {
            id: Uuid::nil(),
            key: "k".into(),
            name: "K".into(),
            field_type: t,
            options: opts
                .iter()
                .map(|v| FieldOption {
                    value: v.to_string(),
                    color: None,
                    archived: *v == "old",
                })
                .collect(),
            required: false,
            default_value: FieldValue::Null,
            source_label: None,
            built_in: false,
            position: 0,
            archived: false,
        }
    }

    #[test]
    fn import_columns_become_fields() {
        let taken: HashSet<String> = ["type".to_string()].into_iter().collect();
        assert_eq!(
            import_field_key("Automation Type", &taken),
            "automation_type"
        );
        assert_eq!(import_field_key("Type", &taken), "type_2");
        assert_eq!(import_field_key("2nd owner", &taken), "f_2nd_owner");
        assert_eq!(import_field_key("Ünïcode!", &taken), "n_code");
        assert_eq!(import_field_key("", &taken), "f");
        assert_eq!(import_field_name("automation_type"), "Automation type");
        assert_eq!(import_field_name("Automation Type"), "Automation Type");
        assert!(valid_key(&import_field_key(&"x".repeat(300), &taken)));
        let mut c = ImportColumn::default();
        c.observe(&["Functional".into()], false, false);
        c.observe(&["functional".into(), "Regression".into()], false, false);
        assert_eq!(c.field_type(), FieldType::Select);
        assert_eq!(c.values.len(), 2, "values compare without case");
        for i in 0..MAX_IMPORT_SELECT_OPTIONS {
            c.observe(&[format!("v{i}")], false, false);
        }
        assert_eq!(c.field_type(), FieldType::Text, "too many values");
        assert!(c.values.is_empty());
        let mut m = ImportColumn::default();
        m.observe(&["a".into(), "b".into()], true, false);
        assert_eq!(m.field_type(), FieldType::Multiselect);
        let mut n = ImportColumn::default();
        n.observe(&["600".into()], false, true);
        assert_eq!(n.field_type(), FieldType::Number);
        n.observe(&["10m".into()], false, false);
        assert_eq!(n.field_type(), FieldType::Select);
        let mut long = ImportColumn::default();
        long.observe(&["x".repeat(MAX_OPTION_BYTES + 1)], false, false);
        assert_eq!(long.field_type(), FieldType::Text);
    }

    #[test]
    fn keys_labels_and_values() {
        assert!(valid_key("priority") && valid_key("a_1"));
        assert!(!valid_key("") && !valid_key("1a") && !valid_key("A") && !valid_key("a-b"));
        assert!(!valid_key(&"a".repeat(64)));
        assert!(valid_label("severity") && valid_label("allure.label-x"));
        assert!(!valid_label("x y") && !valid_label("_x"));
        let l = TmLimits::default();
        let s = def(FieldType::Select, &["high", "low", "old"]);
        assert!(check_value(&s, &FieldValue::Text("high".into()), &l).is_ok());
        assert!(check_value(&s, &FieldValue::Text("old".into()), &l).is_err());
        assert!(check_value(&s, &FieldValue::Text("nope".into()), &l).is_err());
        assert!(check_value(&s, &FieldValue::Number(1.0), &l).is_err());
        assert!(check_value(&s, &FieldValue::Null, &l).is_ok());
        let m = def(FieldType::Multiselect, &["a", "b"]);
        assert!(check_value(&m, &FieldValue::Many(vec!["a".into(), "b".into()]), &l).is_ok());
        assert!(check_value(&m, &FieldValue::Many(vec!["a".into(), "a".into()]), &l).is_err());
        let d = def(FieldType::Date, &[]);
        assert!(check_value(&d, &FieldValue::Text("2026-11-22".into()), &l).is_ok());
        assert!(check_value(&d, &FieldValue::Text("2026-13-01".into()), &l).is_err());
        let n = def(FieldType::Number, &[]);
        assert!(check_value(&n, &FieldValue::Number(f64::NAN), &l).is_err());
        assert!(check_value(&n, &FieldValue::Number(2.5), &l).is_ok());
        let t = def(FieldType::Text, &[]);
        assert!(check_value(&t, &FieldValue::Text("x".repeat(16_385)), &l).is_err());
    }

    #[test]
    fn options() {
        let l = TmLimits::default();
        let o = |v: &str| FieldOption {
            value: v.into(),
            color: None,
            archived: false,
        };
        assert!(check_options(FieldType::Select, &[o("a"), o("b")], &l).is_ok());
        assert!(check_options(FieldType::Select, &[o("a"), o("a")], &l).is_err());
        assert!(check_options(FieldType::Select, &[o(" a")], &l).is_err());
        assert!(check_options(FieldType::Text, &[o("a")], &l).is_err());
        let mut c = o("c");
        c.color = Some("red".into());
        assert!(check_options(FieldType::Select, &[c], &l).is_err());
        let many: Vec<FieldOption> = (0..1001).map(|i| o(&format!("v{i}"))).collect();
        assert!(matches!(
            check_options(FieldType::Select, &many, &l),
            Err(TmError::Conflict {
                code: "limit_reached",
                ..
            })
        ));
    }
}
