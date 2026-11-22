//! Qase: CSV export V1 (first header `id`) and V2 (`v2.id`), and JSON in the shape of API v1
//! (`GET /v1/case/{code}`).
//!
//! CSV (https://docs.qase.io/en/articles/14423241, …/14441496, …/14442545): suite rows come
//! first (`suite_without_cases = 1`, `suite_id`, `suite_parent_id`, title in `suite` or
//! `title`); a case row names its suite in `suite_id` (else `suite_parent_id`, else by the
//! `suite` title). Steps in `steps_actions`, `steps_result(s)`, `steps_data` as numbered lists
//! (V2: each step quoted, nesting by tabs). System fields as slugs: `priority`, `severity`,
//! `type`, `behavior`, `automation`, `layer`, `is_flaky`, `milestone`; `status`
//! `draft|actual|deprecated` → `draft|active|outdated`; tags separated by commas; other columns
//! are custom fields. `postconditions` and `parameters` go to the description.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Read};

use fastproof_ingest::{JsonBounds, scan_json};
use serde_json::Value;
use serde_json::value::RawValue;

use crate::csv::{Table, cell};
use crate::generic::Target;
use crate::steps::{numbered, rich_items, tree};
use crate::text::io_error;
use crate::{
    ImportError, ImportedAttachment, ImportedCase, ImportedKey, ImportedStep, ImportedValue,
    MAX_JSON_DEPTH, ParseOptions, RowError, Visit, emit, emit_error, map_status, norm_header, rich,
    split_list,
};

const SYSTEM: &[&str] = &[
    "id",
    "v2id",
    "title",
    "description",
    "preconditions",
    "postconditions",
    "suite",
    "suiteid",
    "suiteparentid",
    "suitewithoutcases",
    "status",
    "tags",
    "stepstype",
    "stepsactions",
    "stepsresult",
    "stepsresults",
    "stepsdata",
    "parameters",
    "milestoneid",
    "ismuted",
    "position",
    "createdat",
    "updatedat",
    "author",
    "authorid",
    "memberid",
    "suitedescription",
    "suitepreconditions",
];
const FIELDS: &[&str] = &[
    "priority",
    "severity",
    "type",
    "behavior",
    "automation",
    "layer",
    "is_flaky",
    "milestone",
];
const UNSET: &[&str] = &["undefined", "not set", "notset", "none", "-"];

fn suite_path(suites: &HashMap<String, (String, String)>, mut id: String) -> Vec<String> {
    let mut out = Vec::new();
    for _ in 0..64 {
        let Some((title, parent)) = suites.get(&id) else {
            break;
        };
        out.push(title.clone());
        if parent.is_empty() || *parent == id {
            break;
        }
        id = parent.clone();
    }
    out.reverse();
    out
}

fn field_value(v: &str) -> Option<ImportedValue> {
    let t = v.trim();
    if t.is_empty() || UNSET.contains(&t.to_lowercase().as_str()) {
        return None;
    }
    Some(ImportedValue::Text(t.to_string()))
}

fn sections(base: String, extra: &[(&str, String)]) -> String {
    let mut parts = vec![base];
    for (label, v) in extra {
        if !v.trim().is_empty() {
            parts.push(format!("**{label}**\n\n{v}"));
        }
    }
    parts.retain(|p| !p.is_empty());
    parts.join("\n\n")
}

/// Parses a Qase CSV (V1 or V2).
pub fn parse_csv(
    r: Box<dyn BufRead + '_>,
    opts: &ParseOptions,
    visit: &mut Visit<'_>,
) -> Result<(), ImportError> {
    let mut t = Table::open(r, opts.limits)?;
    let v2 = t
        .headers
        .first()
        .is_some_and(|h| h.trim().eq_ignore_ascii_case("v2.id"));
    let id = t.col_any(&["v2.id", "id"]);
    let title = t.col("title");
    if title.is_none() {
        return Err(ImportError::invalid("not a Qase export: no title column"));
    }
    let col = |n: &str| t.col(n);
    let (desc, pre, post) = (
        col("description"),
        col("preconditions"),
        col("postconditions"),
    );
    let (suite, suite_id, suite_parent, no_cases) = (
        col("suite"),
        col("suite_id"),
        col("suite_parent_id"),
        col("suite_without_cases"),
    );
    let (status, tags, params) = (col("status"), col("tags"), col("parameters"));
    let actions = t.col("steps_actions");
    let results = t.col_any(&["steps_result", "steps_results"]);
    let data = t.col("steps_data");
    let mapping = opts.mapping.clone().unwrap_or_default();
    let mut fields = Vec::new();
    for (i, h) in t.headers.iter().enumerate() {
        if let Some(target) = mapping.get(h) {
            match Target::parse(target) {
                Some(Target::Field(k)) => fields.push((i, k)),
                Some(Target::Ignore) => {}
                _ => {
                    return Err(ImportError::invalid(format!(
                        "mapping: {h:?} can only become field:<name> or ignore"
                    )));
                }
            }
            continue;
        }
        let n = norm_header(h);
        if let Some(f) = FIELDS.iter().find(|f| norm_header(f) == n) {
            fields.push((i, f.to_string()));
        } else if !SYSTEM.contains(&n.as_str()) && !h.trim().is_empty() {
            fields.push((i, h.trim().to_string()));
        }
    }
    let mut suites: HashMap<String, (String, String)> = HashMap::new();
    let mut by_title: HashMap<String, String> = HashMap::new();
    while let Some((row, rec)) = t.reader.next_record()? {
        let rec = match rec {
            Ok(r) => r,
            Err(e) => {
                emit_error(e, visit)?;
                continue;
            }
        };
        if cell(&rec, no_cases).trim() == "1" {
            let sid = cell(&rec, suite_id).trim().to_string();
            let name = [cell(&rec, suite), cell(&rec, title)]
                .into_iter()
                .map(str::trim)
                .find(|s| !s.is_empty())
                .unwrap_or("")
                .to_string();
            if sid.is_empty() || name.is_empty() {
                emit_error(
                    RowError::new(row, "invalid_row", "a suite row needs suite_id and a title"),
                    visit,
                )?;
                continue;
            }
            if suites.len() < 100_000 {
                by_title.entry(name.to_lowercase()).or_insert(sid.clone());
                suites.insert(sid, (name, cell(&rec, suite_parent).trim().to_string()));
            }
            continue;
        }
        let mut c = ImportedCase {
            row,
            name: cell(&rec, title).trim().to_string(),
            ..Default::default()
        };
        let qid = cell(&rec, id).trim();
        if !qid.is_empty() {
            c.keys.push(ImportedKey {
                kind: "qase".into(),
                value: qid.to_string(),
            });
        }
        let sid = [cell(&rec, suite_id), cell(&rec, suite_parent)]
            .into_iter()
            .map(str::trim)
            .find(|s| !s.is_empty() && suites.contains_key(*s))
            .map(str::to_string)
            .or_else(|| {
                by_title
                    .get(&cell(&rec, suite).trim().to_lowercase())
                    .cloned()
            });
        c.folder = match sid {
            Some(s) => suite_path(&suites, s),
            None => {
                let s = cell(&rec, suite).trim();
                if s.is_empty() {
                    Vec::new()
                } else {
                    vec![s.to_string()]
                }
            }
        };
        c.description = sections(
            rich(cell(&rec, desc)),
            &[
                ("Postconditions", rich(cell(&rec, post))),
                ("Parameters", cell(&rec, params).trim().to_string()),
            ],
        );
        c.preconditions = rich(cell(&rec, pre));
        c.status = map_status(cell(&rec, status));
        c.tags = split_list(cell(&rec, tags));
        for (i, name) in &fields {
            if let Some(v) = field_value(cell(&rec, Some(*i))) {
                c.fields.insert(name.clone(), v);
            }
        }
        let items = |col: Option<usize>| rich_items(numbered(cell(&rec, col), v2));
        c.steps = tree(&items(actions), &items(results), &items(data));
        emit(c, &opts.limits, visit)?;
    }
    Ok(())
}

fn text(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn json_steps(
    v: &Value,
    depth: usize,
    attachments: &mut Vec<ImportedAttachment>,
    shared: &mut bool,
) -> Vec<ImportedStep> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for s in arr.iter().take(10_000) {
        if s.get("shared_step_hash").is_some_and(|h| !h.is_null()) {
            *shared = true;
        }
        let mut step = ImportedStep {
            action: rich(&text(s, "action")),
            expected: rich(&text(s, "expected_result")),
            data: rich(&text(s, "data")),
            ..Default::default()
        };
        if let Some(a) = s.get("attachments").and_then(Value::as_array) {
            for x in a.iter().take(1000) {
                if let Some(att) = attachment(x) {
                    step.attachments.push(att.source.clone());
                    attachments.push(att);
                }
            }
        }
        if depth < 16 {
            step.steps = json_steps(
                s.get("steps").unwrap_or(&Value::Null),
                depth + 1,
                attachments,
                shared,
            );
        }
        out.push(step);
    }
    out
}

fn attachment(x: &Value) -> Option<ImportedAttachment> {
    let name = [text(x, "filename"), text(x, "file"), text(x, "name")]
        .into_iter()
        .find(|s| !s.is_empty())?;
    Some(ImportedAttachment {
        source: name.clone(),
        name,
        old_id: None,
    })
}

fn custom_value(v: &Value) -> Option<ImportedValue> {
    match v {
        Value::String(s) => field_value(s),
        Value::Number(n) => n.as_f64().map(ImportedValue::Number),
        Value::Bool(b) => Some(ImportedValue::Text(b.to_string())),
        Value::Array(a) => Some(ImportedValue::Many(
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
        )),
        _ => None,
    }
}

fn json_case(v: &Value, row: u64, folder: &[String], code: Option<&str>) -> ImportedCase {
    let mut c = ImportedCase {
        row,
        name: text(v, "title"),
        folder: folder.to_vec(),
        ..Default::default()
    };
    let id = text(v, "id");
    if !id.is_empty() {
        c.keys.push(ImportedKey {
            kind: "qase".into(),
            value: match code {
                Some(code) => format!("{code}-{id}"),
                None => id,
            },
        });
    }
    let mut params = String::new();
    if let Some(p) = v
        .get("params")
        .filter(|p| !p.is_null() && p.as_array().is_none_or(|a| !a.is_empty()))
    {
        params = serde_json::to_string(p).unwrap_or_default();
    }
    c.description = sections(
        rich(&text(v, "description")),
        &[
            ("Postconditions", rich(&text(v, "postconditions"))),
            ("Parameters", params),
        ],
    );
    c.preconditions = rich(&text(v, "preconditions"));
    c.status = match v.get("status") {
        Some(Value::String(s)) => map_status(s),
        Some(Value::Number(n)) => map_status(&n.to_string()),
        _ => None,
    };
    if let Some(tags) = v.get("tags").and_then(Value::as_array) {
        for t in tags.iter().take(1000) {
            let s = match t {
                Value::String(s) => s.clone(),
                o => text(o, "title"),
            };
            if !s.trim().is_empty() {
                c.tags.push(s.trim().to_string());
            }
        }
    }
    for f in FIELDS {
        if let Some(val) = v.get(*f).and_then(custom_value) {
            c.fields.insert(f.to_string(), val);
        }
    }
    if let Some(cf) = v.get("custom_fields").and_then(Value::as_array) {
        for x in cf.iter().take(1000) {
            let name = [
                text(x, "title"),
                text(x, "name"),
                format!("cf_{}", text(x, "id")),
            ]
            .into_iter()
            .find(|s| !s.is_empty() && s != "cf_")
            .unwrap_or_default();
            if let (false, Some(val)) = (name.is_empty(), x.get("value").and_then(custom_value)) {
                c.fields.insert(name, val);
            }
        }
    }
    let mut shared = false;
    c.steps = json_steps(
        v.get("steps").unwrap_or(&Value::Null),
        1,
        &mut c.attachments,
        &mut shared,
    );
    if shared {
        c.tags.push("imported-shared-step".into());
    }
    if let Some(a) = v.get("attachments").and_then(Value::as_array) {
        for x in a.iter().take(1000) {
            if let Some(att) = attachment(x) {
                c.attachments.push(att);
            }
        }
    }
    c
}

struct Walk<'v, 'a> {
    opts: &'a ParseOptions,
    visit: &'a mut Visit<'v>,
    code: Option<String>,
    count: usize,
    suite_titles: HashMap<i64, (String, Option<i64>)>,
}

/// Bytes of one case of a Qase JSON file, at most (a larger one is a row error).
pub const MAX_CASE_JSON_BYTES: usize = 8 * 1024 * 1024;

/// The keys of a Qase JSON root, borrowed from the file.
#[derive(Default, serde::Deserialize)]
struct RawRoot<'a> {
    #[serde(borrow, default)]
    code: Option<&'a RawValue>,
    #[serde(borrow, default)]
    project_code: Option<&'a RawValue>,
    #[serde(borrow, default, rename = "projectCode")]
    project_code_camel: Option<&'a RawValue>,
    #[serde(borrow, default)]
    suites_flat: Option<&'a RawValue>,
    #[serde(borrow, default, rename = "suiteList")]
    suite_list: Option<&'a RawValue>,
    #[serde(borrow, default)]
    suites: Option<&'a RawValue>,
    #[serde(borrow, default)]
    cases: Option<&'a RawValue>,
    #[serde(borrow, default)]
    result: Option<&'a RawValue>,
}

#[derive(Default, serde::Deserialize)]
struct RawResult<'a> {
    #[serde(borrow, default)]
    entities: Option<&'a RawValue>,
}

#[derive(Default, serde::Deserialize)]
struct RawSuite<'a> {
    #[serde(borrow, default)]
    title: Option<&'a RawValue>,
    #[serde(borrow, default)]
    cases: Option<&'a RawValue>,
    #[serde(borrow, default)]
    suites: Option<&'a RawValue>,
}

fn is_array(raw: &RawValue) -> bool {
    raw.get().trim_start().starts_with('[')
}

fn raw_text(raw: Option<&RawValue>) -> String {
    match raw.map(|r| serde_json::from_str::<Value>(r.get())) {
        Some(Ok(Value::String(s))) => s,
        Some(Ok(Value::Number(n))) => n.to_string(),
        _ => String::new(),
    }
}

fn json_error(e: serde_json::Error) -> ImportError {
    ImportError::invalid(format!("qase json: invalid JSON: {e}"))
}

/// Hands every element of a JSON array to `f`, one at a time, without building the array.
fn each<'a>(
    raw: &'a RawValue,
    mut f: impl FnMut(&'a RawValue) -> Result<(), ImportError>,
) -> Result<(), ImportError> {
    struct Each<'f, 'a, F> {
        f: &'f mut F,
        err: &'f mut Option<ImportError>,
        _p: std::marker::PhantomData<&'a ()>,
    }
    impl<'a, F: FnMut(&'a RawValue) -> Result<(), ImportError>> serde::de::Visitor<'a>
        for Each<'_, 'a, F>
    {
        type Value = ();

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an array")
        }

        fn visit_seq<A: serde::de::SeqAccess<'a>>(self, mut seq: A) -> Result<(), A::Error> {
            while let Some(el) = seq.next_element::<&'a RawValue>()? {
                if let Err(e) = (self.f)(el) {
                    *self.err = Some(e);
                    return Err(serde::de::Error::custom("stopped"));
                }
            }
            Ok(())
        }
    }
    let mut err = None;
    let mut de = serde_json::Deserializer::from_str(raw.get());
    let r = serde::Deserializer::deserialize_seq(
        &mut de,
        Each {
            f: &mut f,
            err: &mut err,
            _p: std::marker::PhantomData,
        },
    );
    if let Some(e) = err {
        return Err(e);
    }
    r.map_err(json_error)
}

impl Walk<'_, '_> {
    fn case(&mut self, raw: &RawValue, folder: &[String]) -> Result<(), ImportError> {
        self.count += 1;
        if self.count > self.opts.limits.max_rows {
            return Err(ImportError::limit(
                "maxImportRows",
                self.opts.limits.max_rows,
            ));
        }
        if raw.get().len() > MAX_CASE_JSON_BYTES {
            return emit_error(
                RowError::new(
                    self.count as u64,
                    "limit_reached",
                    format!("the case has more than {MAX_CASE_JSON_BYTES} bytes of JSON"),
                ),
                self.visit,
            );
        }
        let v: Value = serde_json::from_str(raw.get()).map_err(json_error)?;
        let folder = if folder.is_empty() {
            v.get("suite_id")
                .and_then(Value::as_i64)
                .map(|s| self.path_of(s))
                .unwrap_or_default()
        } else {
            folder.to_vec()
        };
        let c = json_case(&v, self.count as u64, &folder, self.code.as_deref());
        emit(c, &self.opts.limits, self.visit)
    }

    fn path_of(&self, mut id: i64) -> Vec<String> {
        let mut out = Vec::new();
        for _ in 0..64 {
            let Some((t, parent)) = self.suite_titles.get(&id) else {
                break;
            };
            out.push(t.clone());
            match parent {
                Some(p) if *p != id => id = *p,
                _ => break,
            }
        }
        out.reverse();
        out
    }

    fn suites(
        &mut self,
        raw: &RawValue,
        path: &mut Vec<String>,
        depth: usize,
    ) -> Result<(), ImportError> {
        if !is_array(raw) {
            return Ok(());
        }
        each(raw, |el| {
            let s: RawSuite<'_> = serde_json::from_str(el.get()).unwrap_or_default();
            path.push(raw_text(s.title));
            if let Some(cases) = s.cases.filter(|c| is_array(c)) {
                each(cases, |c| self.case(c, path))?;
            }
            if depth < 64
                && let Some(sub) = s.suites
            {
                self.suites(sub, path, depth + 1)?;
            }
            path.pop();
            Ok(())
        })
    }
}

/// Parses Qase JSON: `{suites: [{title, cases, suites}]}` (export), `{result: {entities}}`
/// (API), `{cases: […]}` or an array of cases; a root `code`/`project_code` prefixes keys
/// (`PROJ-45`). The file is read once and walked in place: only one case at a time is
/// built, so memory stays about the size of the file.
pub fn parse_json(
    mut r: Box<dyn BufRead + '_>,
    opts: &ParseOptions,
    visit: &mut Visit<'_>,
) -> Result<(), ImportError> {
    let mut bytes = Vec::new();
    r.read_to_end(&mut bytes).map_err(io_error)?;
    let bounds = JsonBounds {
        max_depth: MAX_JSON_DEPTH,
        max_values: 50_000_000,
        max_string_bytes: (opts.limits.max_cell_bytes as u64).saturating_mul(6),
    };
    scan_json(&bytes, &bounds, "qase json").map_err(|e| match e.limit_name() {
        Some("max_json_depth") => ImportError::limit("maxJsonDepth", MAX_JSON_DEPTH),
        Some("max_string_bytes") => {
            ImportError::limit("maxImportCellBytes", opts.limits.max_cell_bytes)
        }
        _ => ImportError::invalid(e.to_string()),
    })?;
    let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    let doc = std::str::from_utf8(body)
        .map_err(|_| ImportError::invalid("qase json: invalid JSON: not UTF-8"))?;
    let root_raw: &RawValue = serde_json::from_str(doc).map_err(json_error)?;
    let root: RawRoot<'_> = if root_raw.get().trim_start().starts_with('{') {
        serde_json::from_str(doc).map_err(json_error)?
    } else {
        RawRoot::default()
    };
    let code = [root.code, root.project_code, root.project_code_camel]
        .into_iter()
        .flatten()
        .find_map(|r| match serde_json::from_str::<Value>(r.get()) {
            Ok(Value::String(s)) => Some(s),
            _ => None,
        });
    let mut suite_titles = HashMap::new();
    if let Some(flat) = root.suites_flat.or(root.suite_list).filter(|r| is_array(r)) {
        each(flat, |x| {
            let x: Value = serde_json::from_str(x.get()).map_err(json_error)?;
            if let Some(id) = x.get("id").and_then(Value::as_i64) {
                suite_titles.insert(
                    id,
                    (
                        text(&x, "title"),
                        x.get("parent_id").and_then(Value::as_i64),
                    ),
                );
            }
            Ok(())
        })?;
    }
    let mut w = Walk {
        opts,
        visit,
        code,
        count: 0,
        suite_titles,
    };
    let entities = root
        .result
        .and_then(|r| serde_json::from_str::<RawResult<'_>>(r.get()).ok())
        .and_then(|r| r.entities);
    let cases = entities.or(root.cases).or_else(|| {
        root_raw
            .get()
            .trim_start()
            .starts_with('[')
            .then_some(root_raw)
    });
    if let Some(s) = root.suites.filter(|s| is_array(s)) {
        let mut path = Vec::new();
        w.suites(s, &mut path, 1)?;
    }
    if let Some(arr) = cases.filter(|c| is_array(c)) {
        each(arr, |c| w.case(c, &[]))?;
    } else if root.suites.is_none() {
        return Err(ImportError::invalid(
            "not a Qase JSON export: no suites or cases",
        ));
    }
    Ok(())
}

/// Columns of a Qase CSV header that are custom fields (for the preview).
pub fn custom_columns(headers: &[String]) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter(|h| {
            let n = norm_header(h);
            !SYSTEM.contains(&n.as_str()) && !FIELDS.iter().any(|f| norm_header(f) == n)
        })
        .map(|h| (h.clone(), format!("field:{}", h.trim())))
        .collect()
}
