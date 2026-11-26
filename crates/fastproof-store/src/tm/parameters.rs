//! Parameters of data-driven cases: validation, `${name}` placeholders and the row generator
//! (§7.5, night 2).
//!
//! `CaseBody.parameters` holds the columns, the rows (stored explicitly, each with an id
//! `r_<ulid>` and an `enabled` flag) and, optionally, the generator input that filled them.
//! A placeholder is `${column}` in the preconditions or in the action, expected result or data
//! of a step; in a case with parameters every placeholder must name a column. A run executes a
//! parameterized case once per enabled row (`iterations.rs`), and each iteration's result is
//! filed under the Allure parameters `column = value`.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use fastproof_model::Parameter;
use fastproof_model::identity::stringify_parameters;
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::{
    CaseBody, CaseParameters, CaseRow, CaseStep, ParameterMode, ParameterRow, TmError, TmLimits,
    TmResult,
};

/// Bytes of a row id, at most (`r_` included).
pub const MAX_ROW_ID_BYTES: usize = 64;
/// Bytes of a column name, at most.
pub const MAX_COLUMN_BYTES: usize = 64;
/// Values of a generator over all its columns, at most (bounds the work of `pairwise`).
pub const MAX_GENERATOR_VALUES: usize = 500;
/// Problems listed in one `invalid_case` answer, at most.
const MAX_PROBLEMS: usize = 50;

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A new row id: `r_` and a ULID (the 128 bits of a UUIDv7, whose first 48 bits are the
/// millisecond time as in a ULID, in Crockford base32).
pub fn new_row_id() -> String {
    let mut v = Uuid::now_v7().as_u128();
    let mut out = [0u8; 26];
    for slot in out.iter_mut().rev() {
        *slot = CROCKFORD[(v & 31) as usize];
        v >>= 5;
    }
    format!("r_{}", String::from_utf8_lossy(&out))
}

/// `r_` followed by 1 to 62 letters, digits, `_` or `-`.
pub fn valid_row_id(id: &str) -> bool {
    id.len() > 2
        && id.len() <= MAX_ROW_ID_BYTES
        && id.starts_with("r_")
        && id[2..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `^[A-Za-z_][A-Za-z0-9_.-]{0,63}$`.
pub fn valid_column(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_COLUMN_BYTES
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

/// Why a value is refused, if it is: too long, or a control character other than a tab or a
/// line break.
fn value_problem(v: &str, limits: &TmLimits) -> Option<String> {
    if v.len() > limits.max_parameter_value_bytes {
        return Some(format!(
            "a value has at most {} bytes",
            limits.max_parameter_value_bytes
        ));
    }
    if v.chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r')
    {
        return Some("a value has no control characters".into());
    }
    None
}

/// The placeholders of a text in order: `(start, end, name)` byte offsets of `${name}`.
/// `${` followed by anything that is not a column name and `}` is plain text.
pub fn placeholders(text: &str) -> Vec<(usize, usize, &str)> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    while let Some(off) = text[i..].find("${") {
        let start = i + off;
        let body = start + 2;
        let mut end = body;
        while end < b.len() && end - body <= MAX_COLUMN_BYTES && b[end] != b'}' {
            end += 1;
        }
        if end < b.len() && b[end] == b'}' && valid_column(&text[body..end]) {
            out.push((start, end + 1, &text[body..end]));
            i = end + 1;
        } else {
            i = body;
        }
    }
    out
}

/// `text` with every `${name}` of a known name replaced by its value. Unknown names stay.
/// `budget` is the number of bytes the replacements may add over the whole call chain; once
/// it is spent the remaining placeholders stay as they are, so a text full of placeholders
/// cannot multiply into an unbounded string.
pub fn substitute(text: &str, values: &BTreeMap<String, String>, budget: &mut usize) -> String {
    let found = placeholders(text);
    if found.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end, name) in found {
        out.push_str(&text[at..start]);
        match values.get(name) {
            Some(v) if v.len() <= *budget => {
                *budget -= v.len();
                out.push_str(v);
            }
            _ => out.push_str(&text[start..end]),
        }
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

/// The steps with their placeholders replaced (see [`substitute`]).
pub fn substitute_steps(
    steps: &[CaseStep],
    values: &BTreeMap<String, String>,
    budget: &mut usize,
) -> Vec<CaseStep> {
    steps
        .iter()
        .map(|s| CaseStep {
            id: s.id.clone(),
            action: substitute(&s.action, values, budget),
            expected: substitute(&s.expected, values, budget),
            data: substitute(&s.data, values, budget),
            attachments: s.attachments.clone(),
            steps: substitute_steps(&s.steps, values, budget),
            shared: s.shared.clone(),
        })
        .collect()
}

/// The values of a row for every column (a column the row does not name is empty).
pub fn row_values(columns: &[String], row: &ParameterRow) -> BTreeMap<String, String> {
    columns
        .iter()
        .map(|c| (c.clone(), row.values.get(c).cloned().unwrap_or_default()))
        .collect()
}

/// Whether a case runs with iterations: it has columns and at least one enabled row.
pub fn is_parameterized(p: Option<&CaseParameters>) -> bool {
    p.is_some_and(|p| !p.columns.is_empty() && p.rows.iter().any(|r| r.enabled))
}

/// The enabled rows of a case, in order.
pub fn enabled_rows(p: &CaseParameters) -> impl Iterator<Item = &ParameterRow> {
    p.rows.iter().filter(|r| r.enabled)
}

/// The values of an iteration as Allure parameters, in column order (the same
/// `parameters_hash` as an automated test with these `@Parameter`s).
pub fn allure_parameters(columns: &[String], values: &BTreeMap<String, String>) -> Vec<Parameter> {
    columns
        .iter()
        .map(|c| Parameter::new(c.clone(), values.get(c).cloned().unwrap_or_default()))
        .collect()
}

/// `[{name, value}]` in column order, as `series.parameters` and attempt details hold them.
pub fn display_parameters(columns: &[String], values: &BTreeMap<String, String>) -> Value {
    Value::Array(
        columns
            .iter()
            .map(|c| json!({"name": c, "value": values.get(c).cloned().unwrap_or_default()}))
            .collect(),
    )
}

#[derive(Default)]
struct Problems(Vec<Value>);

impl Problems {
    fn add(&mut self, path: impl Into<String>, message: impl Into<String>) {
        if self.0.len() < MAX_PROBLEMS {
            self.0
                .push(json!({"path": path.into(), "message": message.into()}));
        }
    }

    fn into_result(self) -> TmResult<()> {
        if self.0.is_empty() {
            return Ok(());
        }
        let first = self.0[0]["message"].as_str().unwrap_or("invalid case");
        let path = self.0[0]["path"].as_str().unwrap_or("");
        Err(TmError::Invalid {
            code: "invalid_case",
            message: format!("{path}: {first}"),
            details: Some(Value::Array(self.0)),
        })
    }
}

/// Checks the generator input of a case or of `POST …/parameters/generate`: 1 to
/// `max_parameter_columns` valid names, each with 1 or more distinct valid values, at most
/// [`MAX_GENERATOR_VALUES`] values in all.
fn check_generator_values(
    path: &str,
    values: &BTreeMap<String, Vec<String>>,
    limits: &TmLimits,
    p: &mut Problems,
) -> TmResult<()> {
    if values.len() > limits.max_parameter_columns {
        return Err(TmError::limit(
            "maxParameterColumns",
            limits.max_parameter_columns as u64,
        ));
    }
    let total: usize = values.values().map(Vec::len).sum();
    if total > MAX_GENERATOR_VALUES {
        return Err(TmError::limit(
            "maxGeneratorValues",
            MAX_GENERATOR_VALUES as u64,
        ));
    }
    if values.is_empty() {
        p.add(path, "name at least one parameter");
    }
    for (name, vs) in values {
        let at = format!("{path}.{name}");
        if !valid_column(name) {
            p.add(
                &at,
                "a parameter name is a letter or _ and up to 63 letters, digits, _, . or -",
            );
        }
        if vs.is_empty() {
            p.add(&at, "give at least one value");
        }
        let mut seen = HashSet::new();
        for (i, v) in vs.iter().enumerate() {
            if let Some(m) = value_problem(v, limits) {
                p.add(format!("{at}[{i}]"), m);
            }
            if !seen.insert(v.as_str()) {
                p.add(format!("{at}[{i}]"), "duplicate value");
            }
        }
    }
    Ok(())
}

/// Checks `CaseBody.parameters` against the limits and the placeholders of the body: valid,
/// distinct columns (at most `max_parameter_columns`); at most `max_parameter_rows` rows with
/// valid, distinct ids, values only for known columns (at most `max_parameter_value_bytes`
/// each) and no two enabled rows with the same values; every `${name}` in the preconditions
/// and the steps names a column; and no text grows past `max_markdown_bytes` when the longest
/// values are put in. A body without `parameters` is not checked (its `${…}` are plain text).
pub fn validate_parameters(body: &CaseBody, limits: &TmLimits) -> TmResult<()> {
    let Some(params) = &body.parameters else {
        return Ok(());
    };
    let mut p = Problems::default();
    if params.columns.len() > limits.max_parameter_columns {
        return Err(TmError::limit(
            "maxParameterColumns",
            limits.max_parameter_columns as u64,
        ));
    }
    if params.rows.len() > limits.max_parameter_rows {
        return Err(TmError::limit(
            "maxParameterRows",
            limits.max_parameter_rows as u64,
        ));
    }
    let mut columns = BTreeSet::new();
    for (i, c) in params.columns.iter().enumerate() {
        if !valid_column(c) {
            p.add(
                format!("body.parameters.columns[{i}]"),
                "a column is a letter or _ and up to 63 letters, digits, _, . or -",
            );
        }
        if !columns.insert(c.as_str()) {
            p.add(format!("body.parameters.columns[{i}]"), "duplicate column");
        }
    }
    let mut longest: BTreeMap<&str, usize> = columns.iter().map(|c| (*c, 0)).collect();
    let mut ids = HashSet::new();
    let mut sets: BTreeMap<String, usize> = BTreeMap::new();
    for (i, r) in params.rows.iter().enumerate() {
        let at = format!("body.parameters.rows[{i}]");
        if !valid_row_id(&r.id) {
            p.add(
                format!("{at}.id"),
                "row ids are r_ and up to 62 letters, digits, _ or -",
            );
        }
        if !ids.insert(r.id.as_str()) {
            p.add(format!("{at}.id"), "duplicate row id");
        }
        for (k, v) in &r.values {
            if !columns.contains(k.as_str()) {
                p.add(format!("{at}.values.{k}"), "not a column");
                continue;
            }
            if let Some(m) = value_problem(v, limits) {
                p.add(format!("{at}.values.{k}"), m);
            }
            if let Some(l) = longest.get_mut(k.as_str()) {
                *l = (*l).max(v.len());
            }
        }
        if r.enabled {
            // The Allure form `name:value,…` decides the series, and values with `,` or `:`
            // can spell the same form with different cells.
            let set = stringify_parameters(&allure_parameters(
                &params.columns,
                &row_values(&params.columns, r),
            ));
            if let Some(first) = sets.insert(set, i) {
                p.add(
                    format!("{at}.values"),
                    format!(
                        "the same values as rows[{first}] (as Allure parameters); disable or change one of them"
                    ),
                );
            }
        }
    }
    if let Some(g) = &params.generator {
        check_generator_values(
            "body.parameters.generator.values",
            &g.values,
            limits,
            &mut p,
        )?;
    }
    let mut texts: Vec<(String, &str)> = vec![("body.preconditions".into(), &body.preconditions)];
    fn walk<'a>(steps: &'a [CaseStep], prefix: &str, out: &mut Vec<(String, &'a str)>) {
        for (i, s) in steps.iter().enumerate() {
            let path = format!("{prefix}[{i}]");
            out.push((format!("{path}.action"), &s.action));
            out.push((format!("{path}.expected"), &s.expected));
            out.push((format!("{path}.data"), &s.data));
            walk(&s.steps, &format!("{path}.steps"), out);
        }
    }
    walk(&body.steps, "body.steps", &mut texts);
    for (path, text) in texts {
        let mut grown = text.len();
        for (start, end, name) in placeholders(text) {
            match longest.get(name) {
                Some(l) => grown = grown.saturating_sub(end - start).saturating_add(*l),
                None => p.add(&path, format!("unknown parameter ${{{name}}}")),
            }
        }
        if grown > limits.max_markdown_bytes {
            p.add(
                &path,
                format!(
                    "with its parameter values this text has more than {} bytes",
                    limits.max_markdown_bytes
                ),
            );
        }
    }
    p.into_result()
}

/// Refuses a case write that adds `parameters` to a case without them while the gate is
/// closed (§3): replicas of an older version cannot read such a body. A case that already has
/// parameters keeps being writable.
pub async fn require_gate(
    conn: &mut PgConnection,
    limits: &TmLimits,
    body: &CaseBody,
    current: Option<&CaseRow>,
) -> TmResult<()> {
    if body.parameters.is_some() && current.is_none_or(|c| c.body.parameters.is_none()) {
        super::gate::require(conn, limits, "case parameters").await?;
    }
    Ok(())
}

/// Rows from a generator input (`POST /cases/{n}/parameters/generate`; nothing is saved):
/// `all` is every combination, `pairwise` covers every pair of values of two parameters
/// ([`super::pairwise`]). More rows than `max_parameter_rows`: 409 `limit_reached`. Row ids
/// are new; every row is enabled.
pub fn generate(
    mode: ParameterMode,
    values: &BTreeMap<String, Vec<String>>,
    limits: &TmLimits,
) -> TmResult<Vec<ParameterRow>> {
    let mut p = Problems::default();
    check_generator_values("values", values, limits, &mut p)?;
    p.into_result()?;
    let names: Vec<&String> = values.keys().collect();
    let lists: Vec<&Vec<String>> = values.values().collect();
    let sizes: Vec<usize> = lists.iter().map(|v| v.len()).collect();
    let max = limits.max_parameter_rows;
    let too_many = || TmError::limit("maxParameterRows", max as u64);
    let idx: Vec<Vec<usize>> = match mode {
        ParameterMode::All => {
            let total = sizes
                .iter()
                .try_fold(1usize, |a, n| a.checked_mul(*n))
                .filter(|t| *t <= max)
                .ok_or_else(too_many)?;
            (0..total)
                .map(|mut n| {
                    let mut row = vec![0; sizes.len()];
                    for (i, s) in sizes.iter().enumerate().rev() {
                        row[i] = n % s;
                        n /= s;
                    }
                    row
                })
                .collect()
        }
        ParameterMode::Pairwise => super::pairwise::rows(&sizes, max).ok_or_else(too_many)?,
    };
    Ok(idx
        .into_iter()
        .map(|row| ParameterRow {
            id: new_row_id(),
            values: row
                .iter()
                .enumerate()
                .map(|(i, a)| (names[i].clone(), lists[i][*a].clone()))
                .collect(),
            enabled: true,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tm::ParameterGenerator;

    fn limits() -> TmLimits {
        TmLimits::default()
    }

    fn step(action: &str, expected: &str) -> CaseStep {
        CaseStep {
            id: "s_1".into(),
            action: action.into(),
            expected: expected.into(),
            data: String::new(),
            attachments: Vec::new(),
            steps: Vec::new(),
            shared: None,
        }
    }

    fn row(id: &str, v: &[(&str, &str)]) -> ParameterRow {
        ParameterRow {
            id: id.into(),
            values: v
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            enabled: true,
        }
    }

    fn body(columns: &[&str], rows: Vec<ParameterRow>, steps: Vec<CaseStep>) -> CaseBody {
        CaseBody {
            preconditions: format!("Open ${{{}}}", columns[0]),
            steps,
            parameters: Some(CaseParameters {
                columns: columns.iter().map(|c| c.to_string()).collect(),
                rows,
                generator: None,
            }),
            ..Default::default()
        }
    }

    fn problems(e: TmError) -> Vec<String> {
        match e {
            TmError::Invalid {
                code: "invalid_case",
                details: Some(Value::Array(d)),
                ..
            } => d
                .iter()
                .map(|x| {
                    format!(
                        "{} {}",
                        x["path"].as_str().unwrap(),
                        x["message"].as_str().unwrap()
                    )
                })
                .collect(),
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn ids_and_names() {
        let id = new_row_id();
        assert_eq!(id.len(), 28, "{id}");
        assert!(valid_row_id(&id));
        assert_ne!(new_row_id(), id);
        for bad in [
            "",
            "r_",
            "x_1",
            "r_a b",
            "r_é",
            &format!("r_{}", "a".repeat(63)),
        ] {
            assert!(!valid_row_id(bad), "{bad}");
        }
        for good in ["a", "_x", "browser.name", "B-2", &"a".repeat(64)] {
            assert!(valid_column(good), "{good}");
        }
        for bad in ["", "1a", ".a", "a b", "a$", &"a".repeat(65)] {
            assert!(!valid_column(bad), "{bad}");
        }
    }

    #[test]
    fn placeholders_and_substitution() {
        let t = "Log in as ${user} on ${ browser } with $${x} and ${1a} ${user}${";
        let names: Vec<&str> = placeholders(t).into_iter().map(|p| p.2).collect();
        assert_eq!(names, ["user", "x", "user"]);
        let v: BTreeMap<String, String> = [("user".to_string(), "ann".to_string())].into();
        let mut budget = 100;
        assert_eq!(
            substitute(t, &v, &mut budget),
            "Log in as ann on ${ browser } with $${x} and ${1a} ann${"
        );
        assert_eq!(budget, 94);
        let mut budget = 4;
        assert_eq!(
            substitute("${user}${user}${user}", &v, &mut budget),
            "ann${user}${user}"
        );
        assert_eq!(budget, 1);
        let long = format!("${{{}}}", "a".repeat(200));
        assert!(placeholders(&long).is_empty());
        assert!(placeholders("${a").is_empty());
    }

    #[test]
    fn validation_paths() {
        assert!(validate_parameters(&CaseBody::default(), &limits()).is_ok());
        let ok = body(
            &["browser", "user"],
            vec![
                row("r_1", &[("browser", "firefox"), ("user", "ann")]),
                row("r_2", &[("browser", "chromium")]),
            ],
            vec![step("Sign in as ${user}", "Hello ${user}")],
        );
        validate_parameters(&ok, &limits()).unwrap();

        let bad = body(
            &["browser", "browser", "9x"],
            vec![
                row("r_1", &[("browser", "a"), ("nope", "b")]),
                row("r_1", &[("browser", "a")]),
                row("bad", &[("browser", "x\u{7}")]),
            ],
            vec![CaseStep {
                steps: vec![step("${missing}", "")],
                ..step("ok", "")
            }],
        );
        let p = problems(validate_parameters(&bad, &limits()).unwrap_err());
        let want = [
            "body.parameters.columns[1] duplicate column",
            "body.parameters.columns[2] a column",
            "body.parameters.rows[0].values.nope not a column",
            "body.parameters.rows[1].id duplicate row id",
            "body.parameters.rows[1].values the same values as rows[0]",
            "body.parameters.rows[2].id row ids",
            "body.parameters.rows[2].values.browser a value has no control",
            "body.steps[0].steps[0].action unknown parameter ${missing}",
        ];
        for w in want {
            assert!(p.iter().any(|x| x.starts_with(w)), "{w} in {p:?}");
        }
        let spelled = body(
            &["browser", "currency"],
            vec![
                row("r_1", &[("browser", "a,currency:b"), ("currency", "")]),
                row("r_2", &[("browser", "a"), ("currency", "b,currency:")]),
            ],
            vec![],
        );
        let p = problems(validate_parameters(&spelled, &limits()).unwrap_err());
        assert!(
            p.iter()
                .any(|x| x.starts_with("body.parameters.rows[1].values the same values as rows[0]")),
            "rows that spell the same Allure parameters share a series: {p:?}"
        );
        let mut disabled = bad.clone();
        if let Some(ps) = disabled.parameters.as_mut() {
            ps.rows[1].enabled = false;
        }
        let p = problems(validate_parameters(&disabled, &limits()).unwrap_err());
        assert!(!p.iter().any(|x| x.contains("same values")), "{p:?}");
    }

    #[test]
    fn limits_apply() {
        let l = TmLimits {
            max_parameter_columns: 2,
            max_parameter_rows: 2,
            max_parameter_value_bytes: 20,
            max_markdown_bytes: 40,
            ..TmLimits::default()
        };
        let cols = body(&["a", "b", "c"], vec![], vec![]);
        assert!(matches!(
            validate_parameters(&cols, &l),
            Err(TmError::Conflict {
                code: "limit_reached",
                ..
            })
        ));
        let rows = body(
            &["a"],
            vec![row("r_1", &[]), row("r_2", &[]), row("r_3", &[])],
            vec![],
        );
        assert!(matches!(
            validate_parameters(&rows, &l),
            Err(TmError::Conflict {
                code: "limit_reached",
                ..
            })
        ));
        let long = body(
            &["browser"],
            vec![row("r_1", &[("browser", &"x".repeat(21))])],
            vec![],
        );
        let p = problems(validate_parameters(&long, &l).unwrap_err());
        assert!(p[0].contains("at most 20 bytes"), "{p:?}");
        let grows = body(
            &["a"],
            vec![row("r_1", &[("a", &"x".repeat(20))])],
            vec![step(&"${a}".repeat(4), "")],
        );
        let p = problems(validate_parameters(&grows, &l).unwrap_err());
        assert!(p.iter().any(|x| x.contains("more than 40 bytes")), "{p:?}");
        let mut g = body(&["browser"], vec![], vec![]);
        if let Some(ps) = g.parameters.as_mut() {
            ps.generator = Some(ParameterGenerator {
                mode: ParameterMode::All,
                values: [(
                    "browser".to_string(),
                    vec!["x".to_string(), "x".to_string()],
                )]
                .into(),
            });
        }
        let p = problems(validate_parameters(&g, &limits()).unwrap_err());
        assert!(p.iter().any(|x| x.contains("duplicate value")), "{p:?}");
    }

    #[test]
    fn generators() {
        let v: BTreeMap<String, Vec<String>> = [
            (
                "browser".to_string(),
                vec!["firefox".into(), "chromium".into()],
            ),
            (
                "currency".to_string(),
                vec!["EUR".into(), "USD".into(), "UAH".into()],
            ),
            ("lang".to_string(), vec!["en".into(), "uk".into()]),
        ]
        .into();
        let all = generate(ParameterMode::All, &v, &limits()).unwrap();
        assert_eq!(all.len(), 12);
        assert_eq!(all[0].values["browser"], "firefox");
        assert_eq!(all[0].values["currency"], "EUR");
        assert_eq!(all[1].values["lang"], "uk");
        let ids: HashSet<&str> = all.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids.len(), 12);
        let pw = generate(ParameterMode::Pairwise, &v, &limits()).unwrap();
        assert!(pw.len() >= 6 && pw.len() < 12, "{}", pw.len());
        let again = generate(ParameterMode::Pairwise, &v, &limits()).unwrap();
        let strip = |r: &[ParameterRow]| r.iter().map(|x| x.values.clone()).collect::<Vec<_>>();
        assert_eq!(strip(&pw), strip(&again), "deterministic");
        let small = TmLimits {
            max_parameter_rows: 10,
            ..TmLimits::default()
        };
        assert!(matches!(
            generate(ParameterMode::All, &v, &small),
            Err(TmError::Conflict {
                code: "limit_reached",
                ..
            })
        ));
        assert!(generate(ParameterMode::Pairwise, &v, &small).is_ok());
        let empty: BTreeMap<String, Vec<String>> = BTreeMap::new();
        assert!(matches!(
            generate(ParameterMode::All, &empty, &limits()),
            Err(TmError::Invalid { .. })
        ));
        let huge: BTreeMap<String, Vec<String>> =
            [("a".to_string(), (0..600).map(|i| i.to_string()).collect())].into();
        assert!(matches!(
            generate(ParameterMode::All, &huge, &limits()),
            Err(TmError::Conflict {
                code: "limit_reached",
                ..
            })
        ));
    }

    #[test]
    fn allure_hash_matches_values() {
        use fastproof_model::identity::parameters_hash;
        let cols = vec!["browser".to_string(), "currency".to_string()];
        let v: BTreeMap<String, String> = [
            ("currency".to_string(), "EUR".to_string()),
            ("browser".to_string(), "firefox".to_string()),
        ]
        .into();
        let from_code = vec![
            Parameter::new("currency", "EUR"),
            Parameter::new("browser", "firefox"),
        ];
        assert_eq!(
            parameters_hash(&allure_parameters(&cols, &v)),
            parameters_hash(&from_code)
        );
        assert_eq!(
            display_parameters(&cols, &v),
            json!([{"name": "browser", "value": "firefox"}, {"name": "currency", "value": "EUR"}])
        );
    }
}
