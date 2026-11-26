//! Parameters of foreign tools as [`ImportedParameters`]: names are made safe for `${name}`
//! placeholders, value lists become rows (the cartesian product, `generator.mode = all`), and
//! the tool's own placeholders in step texts become `${name}`.

use std::collections::{BTreeMap, HashSet};

use crate::{ImportedParameterRow, ImportedParameters, ImportedStep, ParseLimits};

/// The parameter limits a parser checks while it builds rows, so that a source cannot make it
/// hold more than the limits allow (the rows are checked again with the case).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub columns: usize,
    pub rows: usize,
    pub value_bytes: usize,
}

impl Default for Caps {
    fn default() -> Self {
        Caps::from(&ParseLimits::default())
    }
}

impl From<&ParseLimits> for Caps {
    fn from(l: &ParseLimits) -> Self {
        Caps {
            columns: l.max_parameter_columns,
            rows: l.max_parameter_rows,
            value_bytes: l.max_parameter_value_bytes,
        }
    }
}

/// Which limit a source's parameters exceed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Over {
    Columns,
    Rows,
    Value,
}

/// Small parameters that exceed the same limit as the source's, so the case check refuses
/// the case with the right message without the parser building the whole set.
pub fn over_limit(o: Over, c: &Caps) -> ImportedParameters {
    match o {
        Over::Columns => ImportedParameters {
            columns: (0..=c.columns).map(|i| format!("p{i}")).collect(),
            ..Default::default()
        },
        Over::Rows => ImportedParameters {
            columns: vec!["p".into()],
            rows: (0..=c.rows)
                .map(|i| ImportedParameterRow {
                    id: row_id(i),
                    ..Default::default()
                })
                .collect(),
            generator: None,
        },
        Over::Value => ImportedParameters {
            columns: vec!["p".into()],
            rows: vec![ImportedParameterRow {
                id: row_id(0),
                values: BTreeMap::from([("p".to_string(), "x".repeat(c.value_bytes + 1))]),
                enabled: true,
            }],
            generator: None,
        },
    }
}

/// A name made of ASCII letters, digits and `_`, not starting with a digit, at most 64 bytes.
pub fn sanitize_name(s: &str) -> String {
    let mut out: String = s
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if out.is_empty() {
        out.push_str("param");
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
        out.truncate(64);
    }
    out
}

/// Sanitized names for `names`, unique (`a`, `a_2`, …), in order.
pub fn unique_names(names: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    names
        .iter()
        .map(|n| {
            let base = sanitize_name(n);
            let mut name = base.clone();
            let mut i = 2;
            while !seen.insert(name.clone()) {
                let suffix = format!("_{i}");
                let keep = 64usize.saturating_sub(suffix.len()).min(base.len());
                name = format!("{}{suffix}", &base[..keep]);
                i += 1;
            }
            name
        })
        .collect()
}

fn row_id(i: usize) -> String {
    format!("r_import{i:06}")
}

/// Columns with their value lists as rows: every combination. `None` without columns or
/// values; parameters past `caps` come back as [`over_limit`] (the case is then refused).
pub fn cartesian(columns: &[(String, Vec<String>)], caps: &Caps) -> Option<ImportedParameters> {
    if columns.is_empty() || columns.iter().any(|(_, v)| v.is_empty()) {
        return None;
    }
    if columns.len() > caps.columns {
        return Some(over_limit(Over::Columns, caps));
    }
    if columns
        .iter()
        .any(|(n, v)| n.len() > caps.value_bytes || v.iter().any(|x| x.len() > caps.value_bytes))
    {
        return Some(over_limit(Over::Value, caps));
    }
    let product = columns
        .iter()
        .try_fold(1usize, |n, (_, v)| n.checked_mul(v.len()))
        .unwrap_or(usize::MAX);
    if product > caps.rows {
        return Some(over_limit(Over::Rows, caps));
    }
    let cap = caps.rows;
    let names: Vec<String> = columns.iter().map(|(n, _)| n.clone()).collect();
    let mut rows: Vec<BTreeMap<String, String>> = vec![BTreeMap::new()];
    for (name, values) in columns {
        let mut next = Vec::new();
        'outer: for r in &rows {
            for v in values {
                if next.len() >= cap {
                    break 'outer;
                }
                let mut r = r.clone();
                r.insert(name.clone(), v.clone());
                next.push(r);
            }
        }
        rows = next;
    }
    Some(ImportedParameters {
        columns: names,
        rows: rows
            .into_iter()
            .enumerate()
            .map(|(i, values)| ImportedParameterRow {
                id: row_id(i),
                values,
                enabled: true,
            })
            .collect(),
        generator: None,
    })
}

/// Rows given explicitly (`rows` of name → value), columns in first-seen order.
pub fn from_rows(rows: Vec<BTreeMap<String, String>>) -> Option<ImportedParameters> {
    if rows.is_empty() {
        return None;
    }
    let mut columns: Vec<String> = Vec::new();
    for r in &rows {
        for k in r.keys() {
            if !columns.contains(k) {
                columns.push(k.clone());
            }
        }
    }
    Some(ImportedParameters {
        columns,
        rows: rows
            .into_iter()
            .enumerate()
            .map(|(i, values)| ImportedParameterRow {
                id: row_id(i),
                values,
                enabled: true,
            })
            .collect(),
        generator: None,
    })
}

/// Replaces `{{from}}` and `{{ from }}` with `${to}` in `text`.
pub fn rewrite_text(text: &mut String, renames: &[(String, String)]) {
    if !text.contains("{{") {
        return;
    }
    for (from, to) in renames {
        for pat in [format!("{{{{{from}}}}}"), format!("{{{{ {from} }}}}")] {
            if text.contains(&pat) {
                *text = text.replace(&pat, &format!("${{{to}}}"));
            }
        }
    }
}

fn rewrite_steps(steps: &mut [ImportedStep], renames: &[(String, String)]) {
    for s in steps {
        rewrite_text(&mut s.action, renames);
        rewrite_text(&mut s.expected, renames);
        rewrite_text(&mut s.data, renames);
        rewrite_steps(&mut s.steps, renames);
    }
}

/// Renames the placeholders of a case's texts.
pub fn rewrite_case(
    description: &mut String,
    preconditions: &mut String,
    steps: &mut [ImportedStep],
    renames: &[(String, String)],
) {
    if renames.iter().all(|(a, b)| a == b) {
        return;
    }
    rewrite_text(description, renames);
    rewrite_text(preconditions, renames);
    rewrite_steps(steps, renames);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe_and_unique() {
        assert_eq!(sanitize_name("User role"), "User_role");
        assert_eq!(sanitize_name("1st"), "_1st");
        assert_eq!(sanitize_name("  "), "param");
        assert_eq!(sanitize_name(&"x".repeat(100)).len(), 64);
        let n = unique_names(&["a b".into(), "a_b".into(), "a-b".into()]);
        assert_eq!(n, vec!["a_b", "a_b_2", "a_b_3"]);
    }

    #[test]
    fn the_cartesian_product_is_bounded() {
        let cols = vec![
            ("a".to_string(), vec!["1".to_string(), "2".to_string()]),
            ("b".to_string(), vec!["x".to_string(), "y".to_string()]),
        ];
        let caps = Caps::default();
        let p = cartesian(&cols, &caps).unwrap();
        assert_eq!(p.rows.len(), 4);
        assert_eq!(p.columns, vec!["a", "b"]);
        let l = ParseLimits::default();
        let refused = |p: ImportedParameters| crate::check_parameters(&p, &l).unwrap_err();
        let big: Vec<(String, Vec<String>)> = (0..10)
            .map(|i| (format!("c{i}"), vec!["1".into(), "2".into(), "3".into()]))
            .collect();
        let p = cartesian(&big, &caps).unwrap();
        assert_eq!(p.rows.len(), 1001, "3^10 rows are not built");
        assert!(refused(p).contains("rows"));
        let wide: Vec<(String, Vec<String>)> = (0..21)
            .map(|i| (format!("c{i}"), vec!["1".into()]))
            .collect();
        assert!(refused(cartesian(&wide, &caps).unwrap()).contains("columns"));
        let long = vec![("a".to_string(), vec!["x".repeat(1025)])];
        assert!(refused(cartesian(&long, &caps).unwrap()).contains("longer"));
        assert!(cartesian(&[], &caps).is_none());
    }

    #[test]
    fn placeholders_are_renamed() {
        let mut t = "Log in as {{User role}} and {{ User role }}".to_string();
        rewrite_text(&mut t, &[("User role".into(), "User_role".into())]);
        assert_eq!(t, "Log in as ${User_role} and ${User_role}");
    }
}
