//! TestRail CSV export (https://support.testrail.com/hc/en-us/articles/15144643126932):
//! `ID` (`C123`) → key `testrail`, `Title`, `Section Hierarchy` (`A > B > C`, else
//! `Section`) → folder, `Preconditions`, steps from `Steps (Step)` / `Steps (Expected
//! Result)` / `Steps (Additional Info)` (separated steps: one row per step with the case's
//! `ID` repeated or empty; or all steps in one cell as a numbered list), or the text template's
//! `Steps` + `Expected Result`; `Mission` and `Goals` (exploratory template) go to the
//! description; `Priority`, `Type`, `References`, `Estimate` and custom columns become fields.
//! HTML (WYSIWYG editor, TestRail 9.7+) becomes Markdown.

use std::collections::HashSet;
use std::io::BufRead;

use crate::csv::{Table, cell};
use crate::generic::Target;
use crate::steps::{numbered, rich_items, tree};
use crate::{
    ImportError, ImportedCase, ImportedKey, ImportedRequirementRef, ImportedSharedStep,
    ImportedStep, ImportedValue, ParseOptions, StepsLayout, Visit, emit, emit_error, emit_shared,
    folder_path, norm_header, rich, split_list,
};

const SYSTEM: &[&str] = &[
    "id",
    "title",
    "section",
    "sectionhierarchy",
    "sectiondepth",
    "sectiondescription",
    "suite",
    "suiteid",
    "createdby",
    "createdon",
    "updatedby",
    "updatedon",
    "template",
    "preconditions",
    "steps",
    "expectedresult",
    "stepsstep",
    "stepsexpectedresult",
    "stepsadditionalinfo",
    "stepsreferences",
    "stepssharedstepid",
    "mission",
    "goals",
    "isconverted",
    "sectionid",
];

/// Field names of the system columns that become fields.
fn field_name(header: &str) -> String {
    match norm_header(header).as_str() {
        "priority" => "priority".into(),
        "type" => "type".into(),
        "references" => "references".into(),
        "estimate" => "estimate".into(),
        "milestone" => "milestone".into(),
        "automationtype" => "automation_type".into(),
        _ => header.trim().to_string(),
    }
}

struct Cols {
    id: Option<usize>,
    title: Option<usize>,
    section: Option<usize>,
    hierarchy: Option<usize>,
    pre: Option<usize>,
    steps_text: Option<usize>,
    expected_text: Option<usize>,
    step: Option<usize>,
    step_expected: Option<usize>,
    step_data: Option<usize>,
    shared: Option<usize>,
    references: Option<usize>,
    mission: Option<usize>,
    goals: Option<usize>,
    fields: Vec<(usize, String)>,
}

#[derive(Default)]
struct RawStep {
    action: String,
    expected: String,
    data: String,
    shared: String,
}

struct Pending {
    case: ImportedCase,
    id: String,
    rows: Vec<RawStep>,
}

fn shared_key(id: &str) -> ImportedKey {
    ImportedKey {
        kind: "testrail".into(),
        value: id.to_string(),
    }
}

/// Marks the steps `[from..]` as coming from shared step `id`; the first case with a group
/// defines the shared step (name `Shared step <id>`, the steps of the group).
fn mark_shared(
    steps: &mut [ImportedStep],
    id: &str,
    seen: &mut HashSet<String>,
    opts: &ParseOptions,
    defs: &mut Vec<ImportedSharedStep>,
    row: u64,
) {
    if seen.contains(id) {
        for s in steps.iter_mut() {
            s.shared = Some(shared_key(id));
        }
        return;
    }
    if seen.len() >= opts.limits.max_shared_steps {
        return;
    }
    seen.insert(id.to_string());
    defs.push(ImportedSharedStep {
        row,
        keys: vec![shared_key(id)],
        name: format!("Shared step {id}"),
        steps: steps.to_vec(),
        ..Default::default()
    });
    for s in steps.iter_mut() {
        s.shared = Some(shared_key(id));
    }
}

fn finish(
    p: Pending,
    layout: Option<StepsLayout>,
    opts: &ParseOptions,
    seen: &mut HashSet<String>,
    visit: &mut Visit<'_>,
) -> Result<(), ImportError> {
    let mut c = p.case;
    let mut defs: Vec<ImportedSharedStep> = Vec::new();
    let split = match layout {
        Some(StepsLayout::Rows) => false,
        Some(StepsLayout::Cells) => true,
        None => p.rows.len() == 1,
    };
    if split && p.rows.len() == 1 {
        let r = &p.rows[0];
        let a = rich_items(numbered(&r.action, false));
        let e = rich_items(numbered(&r.expected, false));
        let d = rich_items(numbered(&r.data, false));
        let from = c.steps.len();
        c.steps.extend(tree(&a, &e, &d));
        if !r.shared.is_empty() {
            mark_shared(
                &mut c.steps[from..],
                &r.shared,
                seen,
                opts,
                &mut defs,
                c.row,
            );
        }
    } else {
        let mut group: Option<(String, usize)> = None;
        let mut close = |c: &mut ImportedCase, group: &mut Option<(String, usize)>| {
            if let Some((id, from)) = group.take() {
                mark_shared(&mut c.steps[from..], &id, seen, opts, &mut defs, c.row);
            }
        };
        for r in p.rows {
            if group.as_ref().is_some_and(|(id, _)| *id != r.shared) {
                close(&mut c, &mut group);
            }
            if group.is_none() && !r.shared.is_empty() {
                group = Some((r.shared.clone(), c.steps.len()));
            }
            c.steps.push(ImportedStep {
                action: rich(&r.action),
                expected: rich(&r.expected),
                data: rich(&r.data),
                ..Default::default()
            });
        }
        close(&mut c, &mut group);
    }
    for d in defs {
        emit_shared(d, &opts.limits, visit)?;
    }
    emit(c, &opts.limits, visit)
}

/// Parses a TestRail CSV export.
pub fn parse(
    r: Box<dyn BufRead + '_>,
    opts: &ParseOptions,
    visit: &mut Visit<'_>,
) -> Result<(), ImportError> {
    let mut t = Table::open(r, opts.limits)?;
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
        if !SYSTEM.contains(&norm_header(h).as_str()) && !h.trim().is_empty() {
            fields.push((i, field_name(h)));
        }
    }
    let c = Cols {
        id: t.col("ID"),
        title: t.col("Title"),
        section: t.col("Section"),
        hierarchy: t.col("Section Hierarchy"),
        pre: t.col_any(&["Preconditions", "Preconds"]),
        steps_text: t.col("Steps"),
        expected_text: t.col("Expected Result"),
        step: t.col_any(&["Steps (Step)", "Step"]),
        step_expected: t.col_any(&["Steps (Expected Result)", "Steps (Expected)"]),
        step_data: t.col("Steps (Additional Info)"),
        shared: t.col("Steps (Shared step ID)"),
        references: t.col("References"),
        mission: t.col("Mission"),
        goals: t.col("Goals"),
        fields,
    };
    if c.title.is_none() {
        return Err(ImportError::invalid(
            "not a TestRail export: no Title column",
        ));
    }
    let layout = opts.steps_layout;
    let mut seen_shared: HashSet<String> = HashSet::new();
    let mut cur: Option<Pending> = None;
    while let Some((row, rec)) = t.reader.next_record()? {
        let rec = match rec {
            Ok(r) => r,
            Err(e) => {
                emit_error(e, visit)?;
                continue;
            }
        };
        let id = cell(&rec, c.id).trim().to_string();
        let title = cell(&rec, c.title).trim();
        let continues = cur
            .as_ref()
            .is_some_and(|p| (id.is_empty() && title.is_empty()) || (!id.is_empty() && p.id == id));
        if !continues {
            if let Some(p) = cur.take() {
                finish(p, layout, opts, &mut seen_shared, visit)?;
            }
            let mut k = ImportedCase {
                row,
                name: title.to_string(),
                ..Default::default()
            };
            if !id.is_empty() {
                k.keys.push(ImportedKey {
                    kind: "testrail".into(),
                    value: id.clone(),
                });
            }
            let hierarchy = cell(&rec, c.hierarchy);
            k.folder = if !hierarchy.trim().is_empty() {
                folder_path(hierarchy, ">")
            } else {
                folder_path(cell(&rec, c.section), ">")
            };
            k.preconditions = rich(cell(&rec, c.pre));
            let mut desc = Vec::new();
            for (label, col) in [("Mission", c.mission), ("Goals", c.goals)] {
                let v = rich(cell(&rec, col));
                if !v.is_empty() {
                    desc.push(format!("**{label}**\n\n{v}"));
                }
            }
            k.description = desc.join("\n\n");
            for (i, name) in &c.fields {
                let v = cell(&rec, Some(*i)).trim();
                if !v.is_empty() {
                    k.fields.insert(name.clone(), ImportedValue::Text(rich(v)));
                }
            }
            for key in split_list(cell(&rec, c.references)) {
                if !k.requirements.iter().any(|r| r.key == key) {
                    k.requirements.push(ImportedRequirementRef {
                        key,
                        ..Default::default()
                    });
                }
            }
            let text = cell(&rec, c.steps_text);
            let mut p = Pending {
                case: k,
                id,
                rows: Vec::new(),
            };
            if !text.trim().is_empty() {
                p.case.steps.push(ImportedStep {
                    action: rich(text),
                    expected: rich(cell(&rec, c.expected_text)),
                    ..Default::default()
                });
            } else if !cell(&rec, c.expected_text).trim().is_empty() && c.step.is_none() {
                p.case.description = [
                    p.case.description.clone(),
                    format!(
                        "**Expected result**\n\n{}",
                        rich(cell(&rec, c.expected_text))
                    ),
                ]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            }
            cur = Some(p);
        }
        let Some(p) = cur.as_mut() else { continue };
        if !cell(&rec, c.shared).trim().is_empty()
            && !p.case.tags.iter().any(|t| t == "imported-shared-step")
        {
            p.case.tags.push("imported-shared-step".into());
        }
        let raw = RawStep {
            action: cell(&rec, c.step).to_string(),
            expected: cell(&rec, c.step_expected).to_string(),
            data: cell(&rec, c.step_data).to_string(),
            shared: cell(&rec, c.shared).trim().to_string(),
        };
        if !(raw.action.trim().is_empty()
            && raw.expected.trim().is_empty()
            && raw.data.trim().is_empty())
            && p.rows.len() <= opts.limits.max_steps
        {
            p.rows.push(raw);
        }
    }
    if let Some(p) = cur.take() {
        finish(p, layout, opts, &mut seen_shared, visit)?;
    }
    Ok(())
}
