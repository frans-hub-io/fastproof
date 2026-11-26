//! Writers of case exports: CSV (cells that a spreadsheet would run as a formula get a `'`
//! prefix; shared steps and parameters are expanded into text) and the FastProof NDJSON
//! format that [`crate::ndjson`] reads back (shared steps, requirements, parameters and links
//! stay structured).

use std::collections::HashMap;

use crate::{
    ImportedCase, ImportedFieldDef, ImportedParameters, ImportedRequirement, ImportedSharedStep,
    ImportedStep, ImportedValue, ndjson,
};

/// Bytes of shared step text one case of a CSV export expands, at most (as a run item).
pub const MAX_CASE_EXPANDED_BYTES: usize = 1024 * 1024;
/// Bytes of shared step text one CSV export expands in all, at most.
pub const MAX_EXPORT_EXPANDED_BYTES: usize = 256 * 1024 * 1024;

/// The steps of shared steps by their number, for the CSV expansion, and what the export may
/// still expand. A reference past the budget becomes a one-line note, so a few bytes of
/// references cannot make the export many times larger than the cases.
#[derive(Clone, Debug)]
pub struct SharedLookup {
    steps: HashMap<i64, (Vec<ImportedStep>, usize)>,
    left: usize,
}

impl Default for SharedLookup {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

fn text_bytes(steps: &[ImportedStep]) -> usize {
    steps
        .iter()
        .map(|s| 16 + s.action.len() + s.expected.len() + s.data.len() + text_bytes(&s.steps))
        .sum()
}

impl SharedLookup {
    /// A lookup of these shared steps (those without a number are left out).
    pub fn new(shared: Vec<ImportedSharedStep>) -> Self {
        Self::with_budget(shared, MAX_EXPORT_EXPANDED_BYTES)
    }

    /// A lookup that expands at most `budget` bytes in all.
    pub fn with_budget(shared: Vec<ImportedSharedStep>, budget: usize) -> Self {
        Self {
            steps: shared
                .into_iter()
                .filter_map(|x| {
                    let n = x.number?;
                    let bytes = text_bytes(&x.steps);
                    Some((n, (x.steps, bytes)))
                })
                .collect(),
            left: budget,
        }
    }

    /// Bytes the export may still expand.
    pub fn left(&self) -> usize {
        self.left
    }
}

/// Prefixes a cell that starts with `=`, `+`, `-`, `@`, tab or carriage return with `'`, so
/// spreadsheets show it as text (formula injection).
pub fn guard_formula(s: &str) -> std::borrow::Cow<'_, str> {
    match s.as_bytes().first() {
        Some(b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r') => format!("'{s}").into(),
        _ => s.into(),
    }
}

/// One CSV cell: guarded, quoted when needed.
pub fn csv_cell(s: &str, out: &mut String) {
    let g = guard_formula(s);
    if g.contains([',', '"', '\n', '\r']) || g.starts_with(' ') || g.ends_with(' ') {
        out.push('"');
        out.push_str(&g.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(&g);
    }
}

/// One CSV line (with `\r\n`).
pub fn csv_line<'a>(cells: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for (i, c) in cells.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        csv_cell(c, &mut out);
    }
    out.push_str("\r\n");
    out
}

/// Fixed columns of the CSV export (field keys follow).
pub const CSV_COLUMNS: &[&str] = &[
    "Number",
    "Key",
    "Name",
    "Status",
    "Automated",
    "Folder",
    "Description",
    "Preconditions",
    "Steps",
    "Expected",
    "Data",
    "Tags",
    "Links",
    "Parameters",
];

fn numbered(
    steps: &[ImportedStep],
    depth: usize,
    pick: &dyn Fn(&ImportedStep) -> &str,
    out: &mut Vec<String>,
) {
    for (i, s) in steps.iter().enumerate() {
        let t = pick(s).replace("\r\n", "\n").replace('\n', " ");
        out.push(
            format!("{}{}. {}", "\t".repeat(depth), i + 1, t)
                .trim_end()
                .to_string(),
        );
        numbered(&s.steps, depth + 1, pick, out);
    }
}

/// Steps as numbered lists (nesting by tabs): action, expected, data.
pub fn steps_cells(steps: &[ImportedStep]) -> [String; 3] {
    let mut a = Vec::new();
    let mut e = Vec::new();
    let mut d = Vec::new();
    numbered(steps, 0, &|s| &s.action, &mut a);
    numbered(steps, 0, &|s| &s.expected, &mut e);
    numbered(steps, 0, &|s| &s.data, &mut d);
    let has = |v: &[&ImportedStep], f: &dyn Fn(&ImportedStep) -> bool| v.iter().any(|s| f(s));
    let flat = flatten(steps);
    [
        a.join("\n"),
        if has(&flat, &|s| !s.expected.is_empty()) {
            e.join("\n")
        } else {
            String::new()
        },
        if has(&flat, &|s| !s.data.is_empty()) {
            d.join("\n")
        } else {
            String::new()
        },
    ]
}

fn flatten(steps: &[ImportedStep]) -> Vec<&ImportedStep> {
    let mut out = Vec::new();
    for s in steps {
        out.push(s);
        out.extend(flatten(&s.steps));
    }
    out
}

fn value_text(v: &ImportedValue) -> String {
    match v {
        ImportedValue::Text(s) => s.clone(),
        ImportedValue::Many(m) => m.join(", "),
        ImportedValue::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("{}", *n as i64)
            } else {
                n.to_string()
            }
        }
    }
}

/// The lookup of shared steps by number for [`csv_case`].
pub fn shared_lookup(shared: &[ImportedSharedStep]) -> SharedLookup {
    SharedLookup::new(shared.to_vec())
}

/// Replaces the shared step references of the top level by the steps they stand for; a
/// reference the lookup does not know is dropped, and one past the budget (of the case and of
/// the export) becomes a note.
pub fn expand_shared(steps: &[ImportedStep], lookup: &mut SharedLookup) -> Vec<ImportedStep> {
    let mut out = Vec::with_capacity(steps.len());
    let mut case_left = MAX_CASE_EXPANDED_BYTES;
    for s in steps {
        match &s.shared {
            Some(k) if k.kind == "number" => {
                let Some(n) = k.value.parse::<i64>().ok() else {
                    continue;
                };
                let Some((inner, bytes)) = lookup.steps.get(&n) else {
                    continue;
                };
                if *bytes <= case_left && *bytes <= lookup.left {
                    case_left -= bytes;
                    lookup.left -= bytes;
                    out.extend(inner.iter().cloned().map(|mut x| {
                        x.id.clear();
                        x
                    }));
                } else {
                    out.push(ImportedStep {
                        action: format!(
                            "Shared step SS-{n} (not expanded: the export size limit for shared steps is reached)"
                        ),
                        ..Default::default()
                    });
                }
            }
            _ => out.push(s.clone()),
        }
    }
    out
}

/// The enabled rows of the parameters as text: one line per row, `name=value; name=value`.
pub fn parameters_text(p: &ImportedParameters) -> String {
    p.rows
        .iter()
        .filter(|r| r.enabled)
        .map(|r| {
            p.columns
                .iter()
                .map(|c| {
                    format!(
                        "{c}={}",
                        r.values
                            .get(c)
                            .map_or("", String::as_str)
                            .replace('\n', " ")
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The CSV header for these field keys.
pub fn csv_header(field_keys: &[String]) -> String {
    csv_line(
        CSV_COLUMNS
            .iter()
            .copied()
            .chain(field_keys.iter().map(String::as_str)),
    )
}

/// One case as a CSV line (the `Key` column is `TC-<n>`); shared steps are expanded through
/// `shared`, which counts what it expands.
pub fn csv_case(c: &ImportedCase, field_keys: &[String], shared: &mut SharedLookup) -> String {
    let steps = if c.steps.iter().any(|s| s.shared.is_some()) {
        expand_shared(&c.steps, shared)
    } else {
        c.steps.clone()
    };
    let [a, e, d] = steps_cells(&steps);
    let number = c.number.map(|n| n.to_string()).unwrap_or_default();
    let key = c.number.map(|n| format!("TC-{n}")).unwrap_or_default();
    let links = c
        .links
        .iter()
        .map(|l| {
            if l.name.is_empty() {
                l.url.clone()
            } else {
                format!("{}: {}", l.name, l.url)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut cells: Vec<String> = vec![
        number,
        key,
        c.name.clone(),
        c.status.clone().unwrap_or_default(),
        c.automated.to_string(),
        c.folder.join(" / "),
        c.description.clone(),
        c.preconditions.clone(),
        a,
        e,
        d,
        c.tags.join(", "),
        links,
        c.parameters
            .as_ref()
            .map(parameters_text)
            .unwrap_or_default(),
    ];
    for k in field_keys {
        cells.push(c.fields.get(k).map(value_text).unwrap_or_default());
    }
    csv_line(cells.iter().map(String::as_str))
}

/// The NDJSON header line.
pub fn ndjson_header(project: &str, exported_at: &str, fields: Vec<ImportedFieldDef>) -> String {
    let h = ndjson::Header {
        format: ndjson::FORMAT.into(),
        version: ndjson::VERSION,
        project: Some(project.to_string()),
        exported_at: Some(exported_at.to_string()),
        fields,
    };
    let mut s = serde_json::to_string(&h).unwrap_or_default();
    s.push('\n');
    s
}

fn line<T: serde::Serialize>(kind: &str, v: &T) -> String {
    let mut v = serde_json::to_value(v).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.remove("row");
        o.insert("type".into(), kind.into());
    }
    let mut s = serde_json::to_string(&v).unwrap_or_default();
    s.push('\n');
    s
}

/// One case as an NDJSON line (`row` left out).
pub fn ndjson_case(c: &ImportedCase) -> String {
    line("case", c)
}

/// One shared step as an NDJSON line.
pub fn ndjson_shared_step(s: &ImportedSharedStep) -> String {
    line("sharedStep", s)
}

/// One requirement as an NDJSON line.
pub fn ndjson_requirement(r: &ImportedRequirement) -> String {
    line("requirement", r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formula_injection() {
        for bad in ["=1+1", "+1", "-2", "@SUM(A1)", "\tx", "\rx"] {
            assert!(guard_formula(bad).starts_with('\''), "{bad}");
        }
        assert_eq!(guard_formula("ok"), "ok");
        assert_eq!(
            csv_line(["=HYPERLINK(\"x\")", "a,b", "plain"]),
            "\"'=HYPERLINK(\"\"x\"\")\",\"a,b\",plain\r\n"
        );
    }

    #[test]
    fn steps_round_trip_through_cells() {
        let steps = vec![ImportedStep {
            action: "Open".into(),
            expected: "Shown".into(),
            steps: vec![ImportedStep {
                action: "Sub".into(),
                ..Default::default()
            }],
            ..Default::default()
        }];
        let [a, e, d] = steps_cells(&steps);
        assert_eq!(a, "1. Open\n\t1. Sub");
        assert_eq!(e, "1. Shown\n\t1.");
        assert_eq!(d, "");
    }
}
