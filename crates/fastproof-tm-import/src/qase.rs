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

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, BufRead, Read};
use std::rc::Rc;

use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use crate::csv::{Table, cell};
use crate::generic::Target;
use crate::params::{Caps, Over, over_limit};
use crate::steps::{numbered, rich_items, tree};
use crate::text::io_error;
use crate::{
    ImportError, ImportedAttachment, ImportedCase, ImportedKey, ImportedParameters,
    ImportedSharedStep, ImportedStep, ImportedValue, MAX_JSON_DEPTH, ParseLimits, ParseOptions,
    RowError, Visit, cap_name, emit, emit_error, emit_shared, map_status, norm_header, rich,
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

/// The path of a CSV suite, root first: follows parents at most `max_depth + 1` levels and
/// stops at a suite already on the path (a cycle).
fn suite_path(
    suites: &HashMap<String, (String, String)>,
    mut id: String,
    max_depth: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    while out.len() <= max_depth && seen.insert(id.clone()) {
        let Some((title, parent)) = suites.get(&id) else {
            break;
        };
        out.push(title.clone());
        if parent.is_empty() {
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
    let mut paths: HashMap<String, Rc<[String]>> = HashMap::new();
    let l = &opts.limits;
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
                .map(|s| cap_name(s, l.max_name_bytes))
                .unwrap_or_default();
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
                paths.clear();
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
            Some(s) => paths
                .entry(s.clone())
                .or_insert_with(|| suite_path(&suites, s, l.max_folder_depth).into())
                .to_vec(),
            None => {
                let s = cell(&rec, suite).trim();
                if s.is_empty() {
                    Vec::new()
                } else {
                    vec![cap_name(s, l.max_name_bytes)]
                }
            }
        };
        let raw_params = cell(&rec, params).trim();
        let parsed = serde_json::from_str::<Value>(raw_params)
            .ok()
            .and_then(|v| qase_params(&v, l));
        c.description = sections(
            rich(cell(&rec, desc)),
            &[
                ("Postconditions", rich(cell(&rec, post))),
                (
                    "Parameters",
                    if parsed.is_some() {
                        String::new()
                    } else {
                        raw_params.to_string()
                    },
                ),
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
        if let Some(p) = parsed {
            crate::params::rewrite_case(
                &mut c.description,
                &mut c.preconditions,
                &mut c.steps,
                &p.renames,
            );
            c.parameters = Some(p.params);
        }
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

/// What a case's steps said about shared steps: whether any step came from one, and the
/// shared steps to define (by hash, once per case).
#[derive(Default)]
struct Shares {
    found: bool,
    defs: Vec<ImportedSharedStep>,
}

fn shared_key(hash: &str) -> ImportedKey {
    ImportedKey {
        kind: "qase".into(),
        value: hash.to_string(),
    }
}

fn clean_shared(steps: &mut [ImportedStep]) {
    for s in steps {
        s.shared = None;
        s.attachments.clear();
        s.id.clear();
        clean_shared(&mut s.steps);
    }
}

fn json_steps(
    v: &Value,
    depth: usize,
    attachments: &mut Vec<ImportedAttachment>,
    shared: &mut Shares,
) -> Vec<ImportedStep> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut hashes: Vec<Option<(String, Option<Vec<ImportedStep>>)>> = Vec::new();
    for s in arr.iter().take(10_000) {
        let hash = s
            .get("shared_step_hash")
            .filter(|h| !h.is_null())
            .map(|h| match h {
                Value::String(x) => x.clone(),
                o => o.to_string(),
            })
            .filter(|h| !h.is_empty() && h.len() <= 200);
        if s.get("shared_step_hash").is_some_and(|h| !h.is_null()) {
            shared.found = true;
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
        let nested = hash.as_ref().and_then(|_| {
            let n = s.get("shared_step_nested_steps")?;
            let mut throwaway = Vec::new();
            let mut st = json_steps(n, depth + 1, &mut throwaway, &mut Shares::default());
            (!st.is_empty()).then(|| {
                clean_shared(&mut st);
                st
            })
        });
        hashes.push(hash.map(|h| (h, nested)));
        out.push(step);
    }
    if depth == 1 {
        let mut i = 0;
        while i < out.len() {
            let Some((hash, nested)) = hashes[i].clone() else {
                i += 1;
                continue;
            };
            let mut j = i + 1;
            while j < out.len() && hashes[j].as_ref().is_some_and(|(h, _)| *h == hash) {
                j += 1;
            }
            let mut content = match nested {
                Some(n) => n,
                None => out[i..j].to_vec(),
            };
            clean_shared(&mut content);
            let name: String = if content.len() == 1 && !content[0].action.trim().is_empty() {
                content[0].action.chars().take(120).collect()
            } else {
                format!("Shared step {hash}")
            };
            if !shared.defs.iter().any(|d| d.keys[0].value == hash) {
                shared.defs.push(ImportedSharedStep {
                    keys: vec![shared_key(&hash)],
                    name,
                    steps: content,
                    ..Default::default()
                });
            }
            for st in &mut out[i..j] {
                st.shared = Some(shared_key(&hash));
            }
            i = j;
        }
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

fn json_case(
    v: &Value,
    row: u64,
    folder: &[String],
    code: Option<&str>,
    limits: &ParseLimits,
) -> (ImportedCase, Vec<ImportedSharedStep>) {
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
        .filter(|p| qase_params(p, limits).is_none())
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
    let mut shared = Shares::default();
    c.steps = json_steps(
        v.get("steps").unwrap_or(&Value::Null),
        1,
        &mut c.attachments,
        &mut shared,
    );
    if shared.found {
        c.tags.push("imported-shared-step".into());
    }
    let parameters = v.get("params").and_then(|p| qase_params(p, limits));
    if let Some(p) = parameters {
        let renames: Vec<(String, String)> = p.renames;
        crate::params::rewrite_case(
            &mut c.description,
            &mut c.preconditions,
            &mut c.steps,
            &renames,
        );
        c.parameters = Some(p.params);
    }
    if let Some(a) = v.get("attachments").and_then(Value::as_array) {
        for x in a.iter().take(1000) {
            if let Some(att) = attachment(x) {
                c.attachments.push(att);
            }
        }
    }
    (c, shared.defs)
}

/// Parameters read from a Qase `params` value, with the placeholder renames of their names.
struct QaseParams {
    params: ImportedParameters,
    renames: Vec<(String, String)>,
}

fn param_values(v: &Value, caps: &Caps) -> Option<Result<Vec<String>, Over>> {
    let one = |x: &Value| match x {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    };
    let values: Vec<String> = match v {
        Value::Array(a) if a.len() > caps.rows => return Some(Err(Over::Rows)),
        Value::Array(a) => a.iter().map(one).collect::<Option<_>>()?,
        x => vec![one(x)?],
    };
    if values.iter().any(|s| s.len() > caps.value_bytes) {
        return Some(Err(Over::Value));
    }
    Some(Ok(values))
}

fn group_row(
    o: &serde_json::Map<String, Value>,
    caps: &Caps,
) -> Option<Result<Vec<(String, String)>, Over>> {
    if o.len() > caps.columns {
        return Some(Err(Over::Columns));
    }
    let mut out = Vec::with_capacity(o.len());
    for (k, v) in o {
        match param_values(v, caps)? {
            Ok(mut v) if v.len() == 1 => out.push((k.clone(), v.remove(0))),
            Ok(_) => return None,
            Err(e) => return Some(Err(e)),
        }
    }
    Some(Ok(out))
}

/// `params` of a Qase case: `{"single": {name: [values]}, "group": [rows or groups of rows]}`
/// or the flat `{name: [values]}`. Every combination of the single parameters and one row of
/// each group is a row. A shape this does not know stays in the description as JSON. Names,
/// values and rows are checked against the limits while they are read: parameters past them
/// come back as [`over_limit`] and the case is refused.
fn qase_params(p: &Value, limits: &ParseLimits) -> Option<QaseParams> {
    let caps = Caps::from(limits);
    let over = |o: Over| {
        Some(QaseParams {
            params: over_limit(o, &caps),
            renames: Vec::new(),
        })
    };
    let obj = p.as_object()?;
    let (single, group) = if obj.contains_key("single") || obj.contains_key("group") {
        (obj.get("single"), obj.get("group"))
    } else {
        (Some(p), None)
    };
    let mut names: Vec<String> = Vec::new();
    let mut singles: Vec<(usize, Vec<String>)> = Vec::new();
    if let Some(Value::Object(m)) = single {
        if m.len() > caps.columns {
            return over(Over::Columns);
        }
        for (k, v) in m {
            match param_values(v, &caps)? {
                Ok(v) => singles.push((names.len(), v)),
                Err(o) => return over(o),
            }
            names.push(k.clone());
        }
    } else if single.is_some_and(|s| !s.is_null()) {
        return None;
    }
    let mut groups: Vec<Vec<Vec<(usize, String)>>> = Vec::new();
    if let Some(g) = group.filter(|g| !g.is_null()) {
        let list = g.as_array()?;
        let sets: Vec<&[Value]> = if list.iter().all(Value::is_array) && !list.is_empty() {
            list.iter()
                .filter_map(|x| x.as_array().map(Vec::as_slice))
                .collect()
        } else {
            vec![list.as_slice()]
        };
        if sets.len() > caps.columns {
            return over(Over::Columns);
        }
        for rows in sets {
            if rows.len() > caps.rows {
                return over(Over::Rows);
            }
            let mut alt = Vec::new();
            for r in rows {
                let cells = match group_row(r.as_object()?, &caps)? {
                    Ok(c) => c,
                    Err(o) => return over(o),
                };
                let mut row = Vec::with_capacity(cells.len());
                for (k, v) in cells {
                    let i = match names.iter().position(|n| *n == k) {
                        Some(i) => i,
                        None => {
                            if names.len() >= caps.columns {
                                return over(Over::Columns);
                            }
                            names.push(k);
                            names.len() - 1
                        }
                    };
                    row.push((i, v));
                }
                alt.push(row);
            }
            if alt.is_empty() {
                return None;
            }
            groups.push(alt);
        }
    }
    if names.is_empty() {
        return None;
    }
    if names.iter().any(|n| n.len() > caps.value_bytes) {
        return over(Over::Value);
    }
    let product = singles
        .iter()
        .map(|(_, v)| v.len())
        .chain(groups.iter().map(Vec::len))
        .try_fold(1usize, |n, k| n.checked_mul(k))
        .unwrap_or(usize::MAX);
    if product > caps.rows {
        return over(Over::Rows);
    }
    let clean = crate::params::unique_names(&names);
    let mut rows: Vec<Vec<Option<String>>> = vec![vec![None; names.len()]];
    for (i, values) in &singles {
        let mut next = Vec::with_capacity(rows.len() * values.len());
        for r in &rows {
            for v in values {
                let mut r = r.clone();
                r[*i] = Some(v.clone());
                next.push(r);
            }
        }
        rows = next;
    }
    for alts in &groups {
        let mut next = Vec::with_capacity(rows.len() * alts.len());
        for r in &rows {
            for a in alts {
                let mut r = r.clone();
                for (i, v) in a {
                    r[*i] = Some(v.clone());
                }
                next.push(r);
            }
        }
        rows = next;
    }
    let rows = rows
        .into_iter()
        .map(|r| {
            r.into_iter()
                .enumerate()
                .map(|(i, v)| (clean[i].clone(), v.unwrap_or_default()))
                .collect()
        })
        .collect();
    let params = crate::params::from_rows(rows)?;
    Some(QaseParams {
        params: ImportedParameters {
            columns: clean.clone(),
            ..params
        },
        renames: names.into_iter().zip(clean).collect(),
    })
}

/// Bytes of one case of a Qase JSON file, at most (a larger one is a row error).
pub const MAX_CASE_JSON_BYTES: usize = 8 * 1024 * 1024;
/// Memory of the values of one case (or flat suite entry) as read, at most; past it the
/// case is a row error. Numbers and empty containers cost far more as values than as text.
const MAX_CASE_VALUE_BYTES: usize = 32 * 1024 * 1024;
/// Bytes of the cases of a suite that wait for its title (a `title` after `cases`), in all;
/// past it the file fails.
pub const MAX_DEFERRED_BYTES: usize = 4 * MAX_CASE_JSON_BYTES;
/// Bytes of the reader's buffer.
const SCAN_BUFFER: usize = 64 * 1024;
/// What one value costs in [`Budget`] besides its text.
const VALUE_COST: usize = std::mem::size_of::<Value>();

#[derive(Debug)]
struct ScanLimit;

impl std::fmt::Display for ScanLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON limit was reached")
    }
}

impl std::error::Error for ScanLimit {}

/// The decoded file as the JSON parser reads it: counts the bytes handed out and enforces,
/// as they pass, the nesting depth and the length of strings (escapes included).
struct Scan<'r> {
    inner: Box<dyn BufRead + 'r>,
    buf: Box<[u8]>,
    start: usize,
    end: usize,
    pos: Rc<Cell<u64>>,
    hit: Rc<Cell<Option<(&'static str, u64)>>>,
    depth: u32,
    in_string: bool,
    escape: bool,
    /// Hex digits of a `\u` escape still to come.
    hex_left: u8,
    string_len: u64,
    /// Bytes of the current string once decoded (a `\u` escape counts 3, the most a UTF-16
    /// unit takes in UTF-8).
    decoded_len: u64,
    max_string: u64,
    max_cell: u64,
}

impl Scan<'_> {
    fn step(&mut self, b: u8) -> Option<(&'static str, u64)> {
        if self.in_string {
            if self.hex_left > 0 {
                self.hex_left -= 1;
            } else if self.escape {
                self.escape = false;
                if b == b'u' {
                    self.hex_left = 4;
                    self.decoded_len += 3;
                } else {
                    self.decoded_len += 1;
                }
            } else if b == b'\\' {
                self.escape = true;
            } else if b == b'"' {
                self.in_string = false;
                return None;
            } else {
                self.decoded_len += 1;
            }
            self.string_len += 1;
            return (self.string_len > self.max_string || self.decoded_len > self.max_cell)
                .then_some(("maxImportCellBytes", self.max_cell));
        }
        match b {
            b'"' => {
                self.in_string = true;
                self.string_len = 0;
                self.decoded_len = 0;
                self.escape = false;
                self.hex_left = 0;
            }
            b'{' | b'[' => {
                self.depth += 1;
                if self.depth > MAX_JSON_DEPTH {
                    return Some(("maxJsonDepth", MAX_JSON_DEPTH as u64));
                }
            }
            b'}' | b']' => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        None
    }
}

impl Read for Scan<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.start == self.end {
            let n = self.inner.read(&mut self.buf)?;
            if n == 0 {
                return Ok(0);
            }
            self.start = 0;
            self.end = n;
        }
        let n = out.len().min(self.end - self.start);
        out[..n].copy_from_slice(&self.buf[self.start..self.start + n]);
        for &b in &out[..n] {
            if let Some(l) = self.step(b) {
                self.hit.set(Some(l));
                return Err(io::Error::other(ScanLimit));
            }
        }
        self.start += n;
        self.pos.set(self.pos.get() + n as u64);
        Ok(n)
    }
}

/// What reading one case may cost: bytes of its text and memory of its values. Past it the
/// rest of the case is skipped.
struct Budget<'p> {
    pos: &'p Cell<u64>,
    start: u64,
    cost: Cell<usize>,
    over: Cell<bool>,
}

impl<'p> Budget<'p> {
    fn new(pos: &'p Cell<u64>) -> Self {
        Self {
            pos,
            start: pos.get(),
            cost: Cell::new(0),
            over: Cell::new(false),
        }
    }

    /// Adds `n` bytes of values; whether the case is over its budget.
    fn charge(&self, n: usize) -> bool {
        if !self.over.get() {
            let c = self.cost.get().saturating_add(n);
            self.cost.set(c);
            if c > MAX_CASE_VALUE_BYTES
                || self.pos.get().saturating_sub(self.start) > MAX_CASE_JSON_BYTES as u64
            {
                self.over.set(true);
            }
        }
        self.over.get()
    }
}

/// Reads one value into a [`Value`] within a [`Budget`]; once over it, the rest is skipped
/// and `Null` returned.
#[derive(Clone, Copy)]
struct Bounded<'b, 'p>(&'b Budget<'p>);

impl<'de> DeserializeSeed<'de> for Bounded<'_, '_> {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Bounded<'_, '_> {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Value, E> {
        self.0.charge(VALUE_COST);
        Ok(Value::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Value, E> {
        self.0.charge(VALUE_COST);
        Ok(Value::from(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
        self.0.charge(VALUE_COST);
        Ok(Value::from(v))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        self.0.charge(VALUE_COST);
        Ok(serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
        if self.0.charge(VALUE_COST + v.len()) {
            return Ok(Value::Null);
        }
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Value, E> {
        if self.0.charge(VALUE_COST + v.len()) {
            return Ok(Value::Null);
        }
        Ok(Value::String(v))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut out = Vec::new();
        while !self.0.charge(VALUE_COST) {
            match seq.next_element_seed(self)? {
                Some(v) => out.push(v),
                None => return Ok(Value::Array(out)),
            }
        }
        drop(out);
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Value::Null)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut out = serde_json::Map::new();
        while !self.0.charge(VALUE_COST) {
            let Some(k) = map.next_key::<String>()? else {
                return Ok(Value::Object(out));
            };
            if self.0.charge(VALUE_COST + k.len()) {
                map.next_value::<IgnoredAny>()?;
                break;
            }
            let v = map.next_value_seed(self)?;
            out.insert(k, v);
        }
        drop(out);
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Value::Null)
    }
}

/// Visitor methods that skip the kinds of values a visitor does not read.
macro_rules! skip {
    (scalars, $v:expr) => {
        fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok($v)
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok($v)
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok($v)
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok($v)
        }
        fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok($v)
        }
    };
    (null, $v:expr) => {
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok($v)
        }
        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok($v)
        }
    };
    (seq, $v:expr) => {
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok($v)
        }
    };
    (map, $v:expr) => {
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok($v)
        }
    };
}

/// A string (and, with `numbers`, a number as text); anything else is skipped as `None`.
struct TextSeed {
    numbers: bool,
}

impl<'de> DeserializeSeed<'de> for TextSeed {
    type Value = Option<String>;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Option<String>, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for TextSeed {
    type Value = Option<String>;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a string")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Option<String>, E> {
        Ok(Some(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Option<String>, E> {
        Ok(Some(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Option<String>, E> {
        Ok(self.numbers.then(|| v.to_string()))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Option<String>, E> {
        Ok(self.numbers.then(|| v.to_string()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Option<String>, E> {
        Ok(self
            .numbers
            .then(|| serde_json::Number::from_f64(v).map(|n| n.to_string()))
            .flatten())
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Option<String>, E> {
        Ok(None)
    }

    skip!(null, None);
    skip!(seq, None);
    skip!(map, None);
}

/// The title of a suite: known once its `title` key has been read (or the suite has ended).
type Title = Rc<RefCell<Option<String>>>;

/// A case met in a suite whose title comes later in the file, kept as compact JSON until the
/// title is known.
struct Deferred {
    row: u64,
    path: Vec<Title>,
    json: String,
}

/// A suite being read.
struct Level {
    title: Title,
    deferred: Vec<Deferred>,
}

/// Which list of the root the cases came from (the first one read applies).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Array,
    Cases,
    Entities,
}

struct Walk<'v, 'a> {
    opts: &'a ParseOptions,
    visit: &'a mut Visit<'v>,
    /// `code`, `project_code`, `projectCode` of the root as read; the first string applies.
    codes: [Option<String>; 3],
    count: usize,
    emitted: usize,
    suite_titles: HashMap<i64, (String, Option<i64>)>,
    /// Paths of flat suites, built once per suite.
    paths: HashMap<i64, Rc<[String]>>,
    /// Entries of flat suite lists read; they count toward `maxImportRows` with the cases.
    flat_rows: usize,
    /// Bytes of [`Deferred`] cases held.
    deferred_bytes: usize,
    flat_seen: bool,
    /// Hashes of the shared steps handed over.
    shared_seen: HashSet<String>,
    /// A root case named a `suite_id` before the flat suite list was read.
    early_suite_ids: bool,
    levels: Vec<Level>,
    pos: Rc<Cell<u64>>,
    err: Option<ImportError>,
    saw_suites: bool,
    source: Option<Source>,
}

impl Walk<'_, '_> {
    fn code(&self) -> Option<&str> {
        self.codes.iter().flatten().next().map(String::as_str)
    }

    /// Keeps `e` and stops the parse.
    fn fail<E: de::Error>(&mut self, e: ImportError) -> E {
        self.err = Some(e);
        E::custom("stopped")
    }

    fn lift<T, E: de::Error>(&mut self, r: Result<T, ImportError>) -> Result<T, E> {
        r.map_err(|e| self.fail(e))
    }

    fn next_row(&mut self) -> Result<u64, ImportError> {
        self.count += 1;
        if self.count.saturating_add(self.flat_rows) > self.opts.limits.max_rows {
            return Err(ImportError::limit(
                "maxImportRows",
                self.opts.limits.max_rows,
            ));
        }
        Ok(self.count as u64)
    }

    fn set_code(&mut self, slot: usize, v: Option<String>) -> Result<(), ImportError> {
        let before = self.code().map(str::to_owned);
        self.codes[slot] = v;
        if self.emitted > 0 && self.code() != before.as_deref() {
            return Err(ImportError::invalid(
                "qase json: the root `code` must come before the cases",
            ));
        }
        Ok(())
    }

    fn emit_case(&mut self, v: &Value, row: u64, folder: &[String]) -> Result<(), ImportError> {
        let (c, defs) = json_case(v, row, folder, self.code(), &self.opts.limits);
        self.emitted += 1;
        for mut d in defs {
            let hash = d.keys[0].value.clone();
            if self.shared_seen.len() >= self.opts.limits.max_shared_steps
                || !self.shared_seen.insert(hash)
            {
                continue;
            }
            d.row = row;
            emit_shared(d, &self.opts.limits, self.visit)?;
        }
        emit(c, &self.opts.limits, self.visit)
    }

    fn too_large(&mut self, row: u64) -> Result<(), ImportError> {
        emit_error(
            RowError::new(
                row,
                "limit_reached",
                format!(
                    "the case has more than {MAX_CASE_JSON_BYTES} bytes of JSON or too many values"
                ),
            ),
            self.visit,
        )
    }

    fn root_case(&mut self, v: &Value, row: u64) -> Result<(), ImportError> {
        let suite = v.get("suite_id").and_then(Value::as_i64);
        if suite.is_some() && !self.flat_seen {
            self.early_suite_ids = true;
        }
        let folder = match suite {
            Some(s) => self.path_of(s),
            None => Rc::from(Vec::new()),
        };
        self.emit_case(v, row, &folder)
    }

    fn suite_case(&mut self, v: &Value, row: u64) -> Result<(), ImportError> {
        if let Some(i) = self.levels.iter().position(|l| l.title.borrow().is_none()) {
            let path = self.levels.iter().map(|l| l.title.clone()).collect();
            let mut json = v.to_string();
            json.shrink_to_fit();
            self.deferred_bytes = self.deferred_bytes.saturating_add(json.len());
            if self.deferred_bytes > MAX_DEFERRED_BYTES {
                return Err(ImportError::invalid(format!(
                    "qase json: more than {MAX_DEFERRED_BYTES} bytes of cases wait for the title of their suite; put each suite's `title` before its `cases`"
                )));
            }
            self.levels[i].deferred.push(Deferred { row, path, json });
            return Ok(());
        }
        let folder: Vec<String> = self
            .levels
            .iter()
            .map(|l| l.title.borrow().clone().unwrap_or_default())
            .collect();
        self.emit_case(v, row, &folder)
    }

    /// Emits the cases that waited for the title of suite `i`. Suites inside it have ended,
    /// so every title of their paths is known.
    fn resolve(&mut self, i: usize, title: String) -> Result<(), ImportError> {
        *self.levels[i].title.borrow_mut() = Some(title);
        for d in std::mem::take(&mut self.levels[i].deferred) {
            self.deferred_bytes = self.deferred_bytes.saturating_sub(d.json.len());
            let folder: Vec<String> = d
                .path
                .iter()
                .map(|t| t.borrow().clone().unwrap_or_default())
                .collect();
            let v: Value = serde_json::from_str(&d.json).map_err(json_error)?;
            self.emit_case(&v, d.row, &folder)?;
        }
        Ok(())
    }

    /// The path of a flat suite, root first, built once per suite: parents are followed at
    /// most `maxFolderDepth + 1` levels and a suite already on the path (a cycle) ends it.
    fn path_of(&mut self, id: i64) -> Rc<[String]> {
        if let Some(p) = self.paths.get(&id) {
            return p.clone();
        }
        let max = self.opts.limits.max_folder_depth;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut cur = id;
        while out.len() <= max && seen.insert(cur) {
            let Some((t, parent)) = self.suite_titles.get(&cur) else {
                break;
            };
            out.push(t.clone());
            match parent {
                Some(p) => cur = *p,
                None => break,
            }
        }
        out.reverse();
        let p: Rc<[String]> = out.into();
        self.paths.insert(id, p.clone());
        p
    }
}

/// One case: its row is counted before it is read, then it is read within a [`Budget`].
struct CaseSeed<'s, 'v, 'a> {
    w: &'s mut Walk<'v, 'a>,
    in_suite: bool,
}

impl<'de> DeserializeSeed<'de> for CaseSeed<'_, '_, '_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        let row = self.w.next_row();
        let row = self.w.lift(row)?;
        let pos = self.w.pos.clone();
        let budget = Budget::new(&pos);
        let v = Bounded(&budget).deserialize(d)?;
        let r = if budget.over.get() {
            self.w.too_large(row)
        } else if self.in_suite {
            self.w.suite_case(&v, row)
        } else {
            self.w.root_case(&v, row)
        };
        self.w.lift(r)
    }
}

/// A list of cases: of a suite (`source: None`) or of the root.
struct CasesSeed<'s, 'v, 'a> {
    w: &'s mut Walk<'v, 'a>,
    source: Option<Source>,
}

impl<'de> DeserializeSeed<'de> for CasesSeed<'_, '_, '_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        if let (Some(s), Some(read)) = (self.source, self.w.source)
            && s != read
        {
            return d.deserialize_ignored_any(IgnoredAny).map(|_| ());
        }
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for CasesSeed<'_, '_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an array of cases")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        if self.source.is_some() {
            self.w.source = self.source;
        }
        let in_suite = self.source.is_none();
        while seq
            .next_element_seed(CaseSeed {
                w: &mut *self.w,
                in_suite,
            })?
            .is_some()
        {}
        Ok(())
    }

    skip!(scalars, ());
    skip!(null, ());
    skip!(map, ());
}

/// `{entities: […]}` of an API answer.
struct ResultSeed<'s, 'v, 'a> {
    w: &'s mut Walk<'v, 'a>,
}

impl<'de> DeserializeSeed<'de> for ResultSeed<'_, '_, '_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for ResultSeed<'_, '_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(k) = map.next_key::<String>()? {
            if k == "entities" {
                map.next_value_seed(CasesSeed {
                    w: &mut *self.w,
                    source: Some(Source::Entities),
                })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }

    skip!(scalars, ());
    skip!(null, ());
    skip!(seq, ());
}

/// `suites_flat` / `suiteList`: `[{id, title, parent_id}]`.
struct FlatSeed<'s, 'v, 'a> {
    w: &'s mut Walk<'v, 'a>,
}

impl<'de> DeserializeSeed<'de> for FlatSeed<'_, '_, '_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for FlatSeed<'_, '_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an array of suites")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        if self.w.early_suite_ids || self.w.emitted > 0 || self.w.deferred_bytes > 0 {
            return Err(self.w.fail(ImportError::invalid(
                "qase json: `suites_flat` must come before the cases",
            )));
        }
        self.w.flat_seen = true;
        self.w.paths.clear();
        let pos = self.w.pos.clone();
        loop {
            let budget = Budget::new(&pos);
            let Some(x) = seq.next_element_seed(Bounded(&budget))? else {
                return Ok(());
            };
            self.w.flat_rows += 1;
            let max = self.w.opts.limits.max_rows;
            if self.w.flat_rows.saturating_add(self.w.count) > max {
                return Err(self.w.fail(ImportError::limit("maxImportRows", max)));
            }
            if let (false, Some(id)) = (budget.over.get(), x.get("id").and_then(Value::as_i64)) {
                let title = cap_name(&text(&x, "title"), self.w.opts.limits.max_name_bytes);
                self.w
                    .suite_titles
                    .insert(id, (title, x.get("parent_id").and_then(Value::as_i64)));
            }
        }
    }

    skip!(scalars, ());
    skip!(null, ());
    skip!(map, ());
}

/// A list of nested suites; answers whether the value was not `null`.
struct SuitesSeed<'s, 'v, 'a> {
    w: &'s mut Walk<'v, 'a>,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for SuitesSeed<'_, '_, '_> {
    type Value = bool;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<bool, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for SuitesSeed<'_, '_, '_> {
    type Value = bool;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an array of suites")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<bool, A::Error> {
        while seq
            .next_element_seed(SuiteSeed {
                w: &mut *self.w,
                depth: self.depth,
            })?
            .is_some()
        {}
        Ok(true)
    }

    skip!(scalars, true);
    skip!(null, false);
    skip!(map, true);
}

/// One suite: `{title, cases, suites}` in any order. Cases read before the title wait for it.
struct SuiteSeed<'s, 'v, 'a> {
    w: &'s mut Walk<'v, 'a>,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for SuiteSeed<'_, '_, '_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for SuiteSeed<'_, '_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a suite")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        self.w.levels.push(Level {
            title: Rc::new(RefCell::new(None)),
            deferred: Vec::new(),
        });
        let i = self.w.levels.len() - 1;
        while let Some(k) = map.next_key::<String>()? {
            match k.as_str() {
                "title" => {
                    let t = map.next_value_seed(TextSeed { numbers: true })?;
                    if self.w.levels[i].title.borrow().is_none() {
                        let t = cap_name(&t.unwrap_or_default(), self.w.opts.limits.max_name_bytes);
                        let r = self.w.resolve(i, t);
                        self.w.lift(r)?;
                    }
                }
                "cases" => map.next_value_seed(CasesSeed {
                    w: &mut *self.w,
                    source: None,
                })?,
                "suites" if self.depth < 64 => {
                    map.next_value_seed(SuitesSeed {
                        w: &mut *self.w,
                        depth: self.depth + 1,
                    })?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if self.w.levels[i].title.borrow().is_none() {
            let r = self.w.resolve(i, String::new());
            self.w.lift(r)?;
        }
        self.w.levels.pop();
        Ok(())
    }

    skip!(scalars, ());
    skip!(null, ());
    skip!(seq, ());
}

/// The root: an object with the keys below, or an array of cases.
struct RootSeed<'s, 'v, 'a> {
    w: &'s mut Walk<'v, 'a>,
}

impl<'de> DeserializeSeed<'de> for RootSeed<'_, '_, '_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for RootSeed<'_, '_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a Qase JSON export")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<(), A::Error> {
        CasesSeed {
            w: self.w,
            source: Some(Source::Array),
        }
        .visit_seq(seq)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(k) = map.next_key::<String>()? {
            let slot = match k.as_str() {
                "code" => Some(0),
                "project_code" => Some(1),
                "projectCode" => Some(2),
                _ => None,
            };
            if let Some(slot) = slot {
                let v = map.next_value_seed(TextSeed { numbers: false })?;
                let r = self.w.set_code(slot, v);
                self.w.lift(r)?;
                continue;
            }
            match k.as_str() {
                "suites_flat" | "suiteList" => {
                    map.next_value_seed(FlatSeed { w: &mut *self.w })?;
                }
                "suites" => {
                    let present = map.next_value_seed(SuitesSeed {
                        w: &mut *self.w,
                        depth: 1,
                    })?;
                    self.w.saw_suites |= present;
                }
                "cases" => map.next_value_seed(CasesSeed {
                    w: &mut *self.w,
                    source: Some(Source::Cases),
                })?,
                "result" => map.next_value_seed(ResultSeed { w: &mut *self.w })?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }

    skip!(scalars, ());
    skip!(null, ());
}

/// Parses Qase JSON: `{suites: [{title, cases, suites}]}` (export), `{result: {entities}}`
/// (API), `{cases: […]}` or an array of cases; a root `code`/`project_code` prefixes keys
/// (`PROJ-45`). The file is streamed: one case at a time is held (within [`Budget`]), the
/// nesting depth and string lengths are checked as bytes are read, and nothing else of the
/// document is kept. Cases of a suite read before its `title` wait for it as compact JSON.
/// Root keys apply to the cases after them: a `code` or a flat suite list after the cases it
/// would change fails the file.
pub fn parse_json(
    r: Box<dyn BufRead + '_>,
    opts: &ParseOptions,
    visit: &mut Visit<'_>,
) -> Result<(), ImportError> {
    let pos = Rc::new(Cell::new(0));
    let hit = Rc::new(Cell::new(None));
    let scan = Scan {
        inner: r,
        buf: vec![0; SCAN_BUFFER].into_boxed_slice(),
        start: 0,
        end: 0,
        pos: pos.clone(),
        hit: hit.clone(),
        depth: 0,
        in_string: false,
        escape: false,
        hex_left: 0,
        string_len: 0,
        decoded_len: 0,
        max_string: (opts.limits.max_cell_bytes as u64).saturating_mul(6),
        max_cell: opts.limits.max_cell_bytes as u64,
    };
    let mut w = Walk {
        opts,
        visit,
        codes: [None, None, None],
        count: 0,
        emitted: 0,
        suite_titles: HashMap::new(),
        paths: HashMap::new(),
        flat_rows: 0,
        deferred_bytes: 0,
        flat_seen: false,
        shared_seen: HashSet::new(),
        early_suite_ids: false,
        levels: Vec::new(),
        pos,
        err: None,
        saw_suites: false,
        source: None,
    };
    let mut de = serde_json::Deserializer::from_reader(scan);
    let r = RootSeed { w: &mut w }
        .deserialize(&mut de)
        .and_then(|()| de.end());
    if let Some(e) = w.err.take() {
        return Err(e);
    }
    if let Some((limit, max)) = hit.get() {
        return Err(ImportError::limit(limit, max));
    }
    if let Err(e) = r {
        return Err(if e.is_io() {
            io_error(e.into())
        } else {
            json_error(e)
        });
    }
    if w.source.is_none() && !w.saw_suites {
        return Err(ImportError::invalid(
            "not a Qase JSON export: no suites or cases",
        ));
    }
    Ok(())
}

fn json_error(e: serde_json::Error) -> ImportError {
    ImportError::invalid(format!("qase json: invalid JSON: {e}"))
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
