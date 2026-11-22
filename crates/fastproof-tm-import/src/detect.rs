//! Format detection and the preview of the first rows of a file.

use std::collections::BTreeMap;

use crate::csv::CsvReader;
use crate::generic::suggested_mapping;
use crate::text::decode_prefix;
use crate::{Format, ImportError, ParseLimits, ParseOptions, StepsLayout, norm_header};

/// Rows a preview shows.
pub const PREVIEW_ROWS: usize = 20;
/// Characters of a preview cell.
pub const PREVIEW_CELL_CHARS: usize = 500;

/// The first rows of a file and how it would be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub format: Format,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub suggested_mapping: BTreeMap<String, String>,
    pub steps_layout: StepsLayout,
}

fn csv_format(headers: &[String]) -> Format {
    let n: Vec<String> = headers.iter().map(|h| norm_header(h)).collect();
    let has = |k: &str| n.iter().any(|h| h == k);
    if headers
        .first()
        .is_some_and(|h| h.trim().eq_ignore_ascii_case("v2.id"))
        || has("suitewithoutcases")
        || has("stepsactions")
    {
        return Format::QaseCsv;
    }
    if has("title")
        && (has("section") || has("sectionhierarchy") || has("sectiondepth") || has("stepsstep"))
    {
        return Format::TestrailCsv;
    }
    if has("allureid") || (has("name") && has("scenario")) {
        return Format::TestopsCsv;
    }
    Format::GenericCsv
}

/// The format of a file from its first bytes.
pub fn detect(head: &[u8]) -> Format {
    let text = decode_prefix(&head[..head.len().min(256 * 1024)]);
    let t = text.trim_start();
    if t.starts_with('{') || t.starts_with('[') {
        let first = t.lines().next().unwrap_or("");
        if first.contains("\"fastproof-cases\"") {
            return Format::FastproofNdjson;
        }
        return Format::QaseJson;
    }
    let mut r = match CsvReader::new(Box::new(t.as_bytes()), ParseLimits::default()) {
        Ok(r) => r,
        Err(_) => return Format::GenericCsv,
    };
    match r.next_record() {
        Ok(Some((_, Ok(h)))) => csv_format(&h),
        _ => Format::GenericCsv,
    }
}

fn clip(s: &str) -> String {
    if s.chars().count() <= PREVIEW_CELL_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(PREVIEW_CELL_CHARS).collect();
    out.push('…');
    out
}

/// Previews the first bytes of a file (the caller reads at most 5 MB): the format (detected
/// unless given), the header, the first 20 rows, the suggested column mapping and the
/// detected steps layout.
pub fn preview(head: &[u8], format: Option<Format>) -> Result<Preview, ImportError> {
    let format = format.unwrap_or_else(|| detect(head));
    if !format.is_csv() {
        let headers: Vec<String> = ["name", "folder", "steps"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut rows = Vec::new();
        if format == Format::FastproofNdjson {
            let text = decode_prefix(head);
            let complete = match text.rfind('\n') {
                Some(i) => &text[..i + 1],
                None => "",
            };
            let opts = ParseOptions::default();
            let _ = crate::parse(format, complete.as_bytes(), &opts, &mut |item| {
                if let crate::Item::Case(c) = item {
                    rows.push(vec![
                        clip(&c.name),
                        clip(&c.folder.join(" / ")),
                        c.steps.len().to_string(),
                    ]);
                }
                rows.len() < PREVIEW_ROWS
            });
        } else {
            let opts = ParseOptions::default();
            let _ = crate::parse(format, head, &opts, &mut |item| {
                if let crate::Item::Case(c) = item {
                    rows.push(vec![
                        clip(&c.name),
                        clip(&c.folder.join(" / ")),
                        c.steps.len().to_string(),
                    ]);
                }
                rows.len() < PREVIEW_ROWS
            });
        }
        return Ok(Preview {
            format,
            headers,
            rows,
            suggested_mapping: BTreeMap::new(),
            steps_layout: StepsLayout::Cells,
        });
    }
    let text = decode_prefix(head);
    let mut r = CsvReader::new(Box::new(text.as_bytes()), ParseLimits::default())?;
    let headers = match r.next_record() {
        Ok(Some((_, Ok(h)))) => h
            .into_iter()
            .map(|h| h.trim().to_string())
            .collect::<Vec<_>>(),
        Ok(Some((_, Err(e)))) => {
            return Err(ImportError::invalid(format!(
                "the header row: {}",
                e.message
            )));
        }
        _ => return Err(ImportError::invalid("the file is empty")),
    };
    let mut rows = Vec::new();
    let mut sample = Vec::new();
    while sample.len() < 200 {
        match r.next_record() {
            Ok(Some((_, Ok(rec)))) => {
                if rows.len() < PREVIEW_ROWS {
                    rows.push(rec.iter().map(|c| clip(c)).collect());
                }
                sample.push(rec);
            }
            Ok(Some((_, Err(_)))) => continue,
            _ => break,
        }
    }
    let col = |names: &[&str]| {
        headers
            .iter()
            .position(|h| names.contains(&norm_header(h).as_str()))
    };
    let rows_layout = |key: Option<usize>, name: Option<usize>, step: Option<usize>| -> bool {
        let mut prev: Option<&str> = None;
        for rec in &sample {
            let k = key.and_then(|i| rec.get(i)).map(|s| s.trim()).unwrap_or("");
            let n = name
                .and_then(|i| rec.get(i))
                .map(|s| s.trim())
                .unwrap_or("");
            let s = step
                .and_then(|i| rec.get(i))
                .map(|s| s.trim())
                .unwrap_or("");
            if (k.is_empty() && n.is_empty() && !s.is_empty()) || (!k.is_empty() && prev == Some(k))
            {
                return true;
            }
            prev = Some(k);
        }
        false
    };
    let (mapping, layout) = match format {
        Format::GenericCsv => {
            let m = suggested_mapping(&headers);
            let target = |t: &str| {
                m.iter()
                    .find(|(_, v)| *v == t)
                    .and_then(|(k, _)| headers.iter().position(|h| h == k))
            };
            let rows = rows_layout(target("key"), target("name"), target("steps"));
            (m, rows)
        }
        Format::TestrailCsv => {
            let fields = custom(
                &headers,
                &[
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
                ],
            );
            let rows = rows_layout(col(&["id"]), col(&["title"]), col(&["stepsstep", "step"]));
            (fields, rows)
        }
        Format::QaseCsv => (crate::qase::custom_columns(&headers), false),
        _ => (
            custom(
                &headers,
                &[
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
                ],
            ),
            false,
        ),
    };
    Ok(Preview {
        format,
        headers,
        rows,
        suggested_mapping: mapping,
        steps_layout: if layout {
            StepsLayout::Rows
        } else {
            StepsLayout::Cells
        },
    })
}

fn custom(headers: &[String], system: &[&str]) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter(|h| !h.trim().is_empty() && !system.contains(&norm_header(h).as_str()))
        .map(|h| (h.clone(), format!("field:{}", h.trim())))
        .collect()
}
