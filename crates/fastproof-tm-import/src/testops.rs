//! Allure TestOps CSV. TestOps does not publish its export columns; this follows the generic
//! CSV of the official migration script `tom`
//! (https://qameta.github.io/docs-3p-migration-script/file2api/generic-csv.html): `allure_id`,
//! `name`, `description`, `precondition`, `scenario` (a line per step, a tab per nesting
//! level), `expected_result`, `tags`, `status`, `layer`, `links`/`useful_links`,
//! `issue_tracker`, plus `full_name` and `automated` when present. Headers are matched without
//! case, spaces and punctuation, so `Allure ID` and `Expected result` work too. `allure_id`
//! becomes the key `testops` and the source number; other columns become fields.

use std::io::BufRead;

use crate::csv::{Table, cell};
use crate::generic::{Target, links_of};
use crate::steps::{rich_items, tabbed, tree};
use crate::{
    ImportError, ImportedCase, ImportedKey, ImportedValue, ParseOptions, Visit, emit, emit_error,
    folder_path, map_status, norm_header, rich, split_list,
};

const SYSTEM: &[&str] = &[
    "allureid",
    "id",
    "name",
    "description",
    "precondition",
    "preconditions",
    "scenario",
    "steps",
    "expectedresult",
    "tags",
    "tag",
    "status",
    "links",
    "usefullinks",
    "issuetracker",
    "issues",
    "fullname",
    "automated",
    "folder",
    "path",
    "createdby",
    "createddate",
    "lastmodifiedby",
    "lastmodifieddate",
    "workflow",
];

fn field_name(h: &str) -> String {
    match norm_header(h).as_str() {
        "layer" => "layer".into(),
        "testedfeature" | "feature" => "feature".into(),
        "testedstory" | "story" => "story".into(),
        "epic" => "epic".into(),
        "owner" | "supervisedby" => "owner".into(),
        "component" => "component".into(),
        "suite" => "suite".into(),
        "severity" => "severity".into(),
        "priority" => "priority".into(),
        _ => h.trim().to_string(),
    }
}

/// Parses an Allure TestOps CSV.
pub fn parse(
    r: Box<dyn BufRead + '_>,
    opts: &ParseOptions,
    visit: &mut Visit<'_>,
) -> Result<(), ImportError> {
    let mut t = Table::open(r, opts.limits)?;
    let find = |names: &[&str]| -> Option<usize> {
        t.headers
            .iter()
            .position(|h| names.contains(&norm_header(h).as_str()))
    };
    let id = find(&["allureid", "id"]);
    let name = find(&["name"]);
    if name.is_none() {
        return Err(ImportError::invalid("not a TestOps export: no name column"));
    }
    let desc = find(&["description"]);
    let pre = find(&["precondition", "preconditions"]);
    let scenario = find(&["scenario", "steps"]);
    let expected = find(&["expectedresult"]);
    let tags = find(&["tags", "tag"]);
    let status = find(&["status", "workflow"]);
    let links = find(&["links", "usefullinks"]);
    let issues = find(&["issuetracker", "issues"]);
    let full_name = find(&["fullname"]);
    let automated = find(&["automated"]);
    let folder = find(&["folder", "path"]);
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
    while let Some((row, rec)) = t.reader.next_record()? {
        let rec = match rec {
            Ok(r) => r,
            Err(e) => {
                emit_error(e, visit)?;
                continue;
            }
        };
        let mut c = ImportedCase {
            row,
            name: cell(&rec, name).trim().to_string(),
            ..Default::default()
        };
        let aid = cell(&rec, id).trim();
        if !aid.is_empty() {
            c.keys.push(ImportedKey {
                kind: "testops".into(),
                value: aid.to_string(),
            });
            c.number = aid.parse().ok().filter(|n: &i64| *n > 0);
        }
        c.description = rich(cell(&rec, desc));
        c.preconditions = rich(cell(&rec, pre));
        c.steps = tree(&rich_items(tabbed(cell(&rec, scenario))), &[], &[]);
        let exp = rich(cell(&rec, expected));
        if !exp.is_empty() {
            match c.steps.last_mut() {
                Some(s) if s.expected.is_empty() => s.expected = exp,
                _ => {
                    c.description = [
                        c.description.clone(),
                        format!("**Expected result**\n\n{exp}"),
                    ]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n")
                }
            }
        }
        c.tags = split_list(cell(&rec, tags));
        c.status = map_status(cell(&rec, status));
        c.links = links_of(cell(&rec, links), "link");
        c.links.extend(links_of(cell(&rec, issues), "issue"));
        let f = cell(&rec, full_name).trim();
        if !f.is_empty() {
            c.full_names.push(f.to_string());
        }
        c.automated = matches!(
            cell(&rec, automated).trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes"
        );
        let fp = cell(&rec, folder);
        if !fp.trim().is_empty() {
            c.folder = if fp.contains('>') {
                folder_path(fp, ">")
            } else {
                folder_path(fp, "/")
            };
        }
        for (i, n) in &fields {
            let v = cell(&rec, Some(*i)).trim();
            if !v.is_empty() {
                c.fields
                    .insert(n.clone(), ImportedValue::Text(v.to_string()));
            }
        }
        emit(c, &opts.limits, visit)?;
    }
    Ok(())
}
