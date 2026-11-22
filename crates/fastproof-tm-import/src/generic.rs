//! Generic CSV with a column mapping `{column → target}`. Targets: `name`, `description`,
//! `preconditions`, `steps`, `expected`, `data`, `folder` (`A / B` or `A > B`), `tags`,
//! `status`, `key` (external key), `number`, `links`, `fullName`, `automated`,
//! `attachments` (file names in the attachment zip), `field:<name>` and `ignore`. Steps are
//! numbered lists in one cell (`cells`) or one step per row (`rows`: a row without a name and
//! key continues the case above).

use std::collections::BTreeMap;
use std::io::BufRead;

use crate::csv::{Table, cell};
use crate::steps::{Item, numbered, rich_items, tree};
use crate::{
    ImportError, ImportedAttachment, ImportedCase, ImportedKey, ImportedLink, ImportedStep,
    ImportedValue, ParseOptions, StepsLayout, Visit, emit, emit_error, folder_path, map_status,
    norm_header, rich, split_list,
};

/// What a column becomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Name,
    Description,
    Preconditions,
    Steps,
    Expected,
    Data,
    Folder,
    Tags,
    Status,
    Key,
    Number,
    Links,
    FullName,
    Automated,
    Attachments,
    Field(String),
    Ignore,
}

impl Target {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "name" => Self::Name,
            "description" => Self::Description,
            "preconditions" => Self::Preconditions,
            "steps" => Self::Steps,
            "expected" => Self::Expected,
            "data" => Self::Data,
            "folder" => Self::Folder,
            "tags" => Self::Tags,
            "status" => Self::Status,
            "key" => Self::Key,
            "number" => Self::Number,
            "links" => Self::Links,
            "fullName" => Self::FullName,
            "automated" => Self::Automated,
            "attachments" => Self::Attachments,
            "ignore" => Self::Ignore,
            f => {
                let k = f.strip_prefix("field:")?;
                if k.is_empty() || k.len() > 200 {
                    return None;
                }
                Self::Field(k.to_string())
            }
        })
    }

    pub fn as_string(&self) -> String {
        match self {
            Self::Name => "name".into(),
            Self::Description => "description".into(),
            Self::Preconditions => "preconditions".into(),
            Self::Steps => "steps".into(),
            Self::Expected => "expected".into(),
            Self::Data => "data".into(),
            Self::Folder => "folder".into(),
            Self::Tags => "tags".into(),
            Self::Status => "status".into(),
            Self::Key => "key".into(),
            Self::Number => "number".into(),
            Self::Links => "links".into(),
            Self::FullName => "fullName".into(),
            Self::Automated => "automated".into(),
            Self::Attachments => "attachments".into(),
            Self::Field(k) => format!("field:{k}"),
            Self::Ignore => "ignore".into(),
        }
    }
}

/// The target a header most likely means.
pub fn suggest(header: &str) -> Target {
    match norm_header(header).as_str() {
        "title" | "name" | "summary" | "testcase" | "casename" | "testname" => Target::Name,
        "description" | "desc" | "details" => Target::Description,
        "preconditions" | "precondition" | "preconds" | "prerequisites" => Target::Preconditions,
        "steps" | "step" | "action" | "actions" | "stepsactions" | "stepsstep" | "scenario"
        | "teststeps" => Target::Steps,
        "expected"
        | "expectedresult"
        | "expectedresults"
        | "stepsresult"
        | "stepsresults"
        | "stepsexpectedresult"
        | "result" => Target::Expected,
        "data" | "testdata" | "stepsdata" | "stepsadditionalinfo" => Target::Data,
        "folder" | "section" | "sectionhierarchy" | "path" | "suite" | "module" => Target::Folder,
        "tags" | "labels" | "tag" => Target::Tags,
        "status" | "state" => Target::Status,
        "id" | "key" | "caseid" | "externalid" | "allureid" | "v2id" => Target::Key,
        "links" | "link" | "url" | "usefullinks" => Target::Links,
        "fullname" => Target::FullName,
        "automated" => Target::Automated,
        "attachments" | "files" => Target::Attachments,
        "" => Target::Ignore,
        _ => Target::Field(header.trim().to_string()),
    }
}

/// The suggested mapping of a header row (every column).
pub fn suggested_mapping(headers: &[String]) -> BTreeMap<String, String> {
    let mut used_name = false;
    let mut out = BTreeMap::new();
    for h in headers {
        let mut t = suggest(h);
        if t == Target::Name {
            if used_name {
                t = Target::Field(h.trim().to_string());
            }
            used_name = true;
        }
        out.insert(h.clone(), t.as_string());
    }
    out
}

/// Columns with their targets.
pub struct Mapping {
    cols: Vec<(usize, Target)>,
}

impl Mapping {
    pub fn new(
        headers: &[String],
        mapping: &BTreeMap<String, String>,
    ) -> Result<Self, ImportError> {
        let mut cols = Vec::new();
        for (col, target) in mapping {
            let Some(i) = headers.iter().position(|h| h == col) else {
                return Err(ImportError::invalid(format!("mapping: no column {col:?}")));
            };
            let t = Target::parse(target).ok_or_else(|| {
                ImportError::invalid(format!("mapping: unknown target {target:?}"))
            })?;
            cols.push((i, t));
        }
        Ok(Self { cols })
    }

    pub fn first(&self, t: &Target) -> Option<usize> {
        self.cols.iter().find(|(_, x)| x == t).map(|(i, _)| *i)
    }

    pub fn has(&self, t: &Target) -> bool {
        self.first(t).is_some()
    }
}

fn bool_cell(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "y" | "automated"
    )
}

/// URLs of a links cell (`url`, `name: url`, `[name](url)`), http(s) only.
pub fn links_of(s: &str, kind: &str) -> Vec<ImportedLink> {
    let mut out = Vec::new();
    for part in s.split(['\n', ',']) {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let (name, url) = if let Some(i) = p.find("http://").or_else(|| p.find("https://")) {
            let name = p[..i]
                .trim()
                .trim_end_matches([':', '(', '[', ']', ' '])
                .trim_start_matches('[');
            let url = p[i..]
                .trim_end_matches([')', ']'])
                .split_whitespace()
                .next()
                .unwrap_or("");
            (name.to_string(), url.to_string())
        } else {
            continue;
        };
        out.push(ImportedLink {
            name,
            url,
            kind: kind.to_string(),
        });
    }
    out
}

/// Fills a case from the mapped cells of one row (the steps are handled by the caller).
fn fill(c: &mut ImportedCase, m: &Mapping, rec: &[String], key_kind: &str) {
    for (i, t) in &m.cols {
        let v = cell(rec, Some(*i)).trim();
        if v.is_empty() {
            continue;
        }
        match t {
            Target::Name => c.name = v.to_string(),
            Target::Description => c.description = rich(v),
            Target::Preconditions => c.preconditions = rich(v),
            Target::Folder => {
                c.folder = if v.contains('>') {
                    folder_path(v, ">")
                } else {
                    folder_path(v, "/")
                }
            }
            Target::Tags => c.tags = split_list(v),
            Target::Status => c.status = map_status(v),
            Target::Key => c.keys.push(ImportedKey {
                kind: key_kind.to_string(),
                value: v.to_string(),
            }),
            Target::Number => c.number = v.trim_start_matches(['C', 'c', '#']).parse().ok(),
            Target::Links => c.links.extend(links_of(v, "link")),
            Target::FullName => c.full_names.push(v.to_string()),
            Target::Automated => c.automated = bool_cell(v),
            Target::Attachments => {
                for a in split_list(v) {
                    c.attachments.push(ImportedAttachment {
                        name: a.clone(),
                        source: a,
                        old_id: None,
                    });
                }
            }
            Target::Field(k) => {
                c.fields
                    .insert(k.clone(), ImportedValue::Text(v.to_string()));
            }
            Target::Steps | Target::Expected | Target::Data | Target::Ignore => {}
        }
    }
}

fn row_step(m: &Mapping, rec: &[String]) -> Option<ImportedStep> {
    let action = rich(cell(rec, m.first(&Target::Steps)));
    let expected = rich(cell(rec, m.first(&Target::Expected)));
    let data = rich(cell(rec, m.first(&Target::Data)));
    if action.is_empty() && expected.is_empty() && data.is_empty() {
        return None;
    }
    Some(ImportedStep {
        action,
        expected,
        data,
        ..Default::default()
    })
}

fn cell_steps(m: &Mapping, rec: &[String]) -> Vec<ImportedStep> {
    let items = |t: &Target| -> Vec<Item> { rich_items(numbered(cell(rec, m.first(t)), false)) };
    tree(
        &items(&Target::Steps),
        &items(&Target::Expected),
        &items(&Target::Data),
    )
}

/// Parses a generic CSV.
pub fn parse(
    r: Box<dyn BufRead + '_>,
    opts: &ParseOptions,
    visit: &mut Visit<'_>,
) -> Result<(), ImportError> {
    let mut t = Table::open(r, opts.limits)?;
    let mapping = opts
        .mapping
        .clone()
        .unwrap_or_else(|| suggested_mapping(&t.headers));
    let m = Mapping::new(&t.headers, &mapping)?;
    if !m.has(&Target::Name) {
        return Err(ImportError::invalid("the mapping has no name column"));
    }
    let layout = opts.steps_layout.unwrap_or(StepsLayout::Cells);
    let key_col = m.first(&Target::Key);
    let name_col = m.first(&Target::Name);
    let mut cur: Option<ImportedCase> = None;
    while let Some((row, rec)) = t.reader.next_record()? {
        let rec = match rec {
            Ok(r) => r,
            Err(e) => {
                emit_error(e, visit)?;
                continue;
            }
        };
        match layout {
            StepsLayout::Cells => {
                let mut c = ImportedCase {
                    row,
                    ..Default::default()
                };
                fill(&mut c, &m, &rec, "external");
                c.steps = cell_steps(&m, &rec);
                emit(c, &opts.limits, visit)?;
            }
            StepsLayout::Rows => {
                let key = cell(&rec, key_col).trim();
                let name = cell(&rec, name_col).trim();
                let same = cur.as_ref().is_some_and(|c| {
                    (key.is_empty() && name.is_empty())
                        || (!key.is_empty() && c.keys.first().is_some_and(|k| k.value == key))
                });
                if !same {
                    if let Some(c) = cur.take() {
                        emit(c, &opts.limits, visit)?;
                    }
                    let mut c = ImportedCase {
                        row,
                        ..Default::default()
                    };
                    fill(&mut c, &m, &rec, "external");
                    cur = Some(c);
                }
                if let (Some(c), Some(s)) = (cur.as_mut(), row_step(&m, &rec)) {
                    if c.steps.len() > opts.limits.max_steps {
                        continue;
                    }
                    c.steps.push(s);
                }
            }
        }
    }
    if let Some(c) = cur.take() {
        emit(c, &opts.limits, visit)?;
    }
    Ok(())
}
