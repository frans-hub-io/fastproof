//! The FastProof NDJSON export: a header line `{"format": "fastproof-cases", "version": 2,
//! "project", "exportedAt", "fields": [field definitions]}`, then one record per line, each
//! with a `type`: `sharedStep` ([`ImportedSharedStep`]), `requirement` ([`ImportedRequirement`])
//! and `case` ([`ImportedCase`], without `row`). Shared steps and requirements come before the
//! cases that use them (parents before children). Full fidelity: folders, steps with their
//! ids and shared step references, links, fields, tags, keys, status, parameters, requirement
//! links, code identities and attachments (by zip entry name). Version 1 files (no `type`, only
//! cases) still import.

use std::io::BufRead;

use fastproof_ingest::{JsonBounds, parse_json_bounded};
use serde::{Deserialize, Serialize};

use crate::text::io_error;
use crate::{
    ImportError, ImportedCase, ImportedFieldDef, ImportedRequirement, ImportedSharedStep, Item,
    MAX_JSON_DEPTH, ParseOptions, RowError, Visit, emit, emit_error, emit_shared,
};

/// `format` of the header line.
pub const FORMAT: &str = "fastproof-cases";
/// Version of the format a export writes.
pub const VERSION: u32 = 2;
/// The oldest version an import reads (cases only, no `type`).
pub const MIN_VERSION: u32 = 1;
/// Bytes of one line, at most.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
/// JSON values of one line, at most. A case at every limit (steps, fields with all their
/// options, parameter rows) has far fewer; tiny values such as `{}` in a list cost about a
/// hundred times their bytes once parsed, so the line size alone does not bound memory.
pub const MAX_LINE_VALUES: u64 = 250_000;

/// The header line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub format: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exported_at: Option<String>,
    #[serde(default)]
    pub fields: Vec<ImportedFieldDef>,
}

/// One line of a version 2 file.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum Record {
    #[serde(rename = "sharedStep")]
    SharedStep(ImportedSharedStep),
    #[serde(rename = "requirement")]
    Requirement(ImportedRequirement),
    #[serde(rename = "case")]
    Case(Box<ImportedCase>),
}

/// Reads one line of at most `max` bytes; `Ok(None)` at the end, `Err(too long)` skips it.
pub fn read_line(
    r: &mut dyn BufRead,
    max: usize,
    buf: &mut Vec<u8>,
) -> Result<Option<bool>, ImportError> {
    buf.clear();
    let mut too_long = false;
    let mut any = false;
    loop {
        let chunk = r.fill_buf().map_err(io_error)?;
        if chunk.is_empty() {
            return Ok(if any { Some(!too_long) } else { None });
        }
        any = true;
        let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        let content = &chunk[..if done { take - 1 } else { take }];
        if buf.len() + content.len() > max {
            too_long = true;
        } else if !too_long {
            buf.extend_from_slice(content);
        }
        r.consume(take);
        if done {
            return Ok(Some(!too_long));
        }
    }
}

fn bounds(opts: &ParseOptions) -> JsonBounds {
    JsonBounds {
        max_depth: MAX_JSON_DEPTH,
        max_values: MAX_LINE_VALUES,
        max_string_bytes: (opts.limits.max_cell_bytes as u64).saturating_mul(6),
    }
}

/// Parses a FastProof NDJSON export.
pub fn parse(
    mut r: Box<dyn BufRead + '_>,
    opts: &ParseOptions,
    visit: &mut Visit<'_>,
) -> Result<(), ImportError> {
    let mut buf = Vec::new();
    let mut line_no: u64 = 0;
    let header: Header = loop {
        match read_line(&mut *r, MAX_LINE_BYTES, &mut buf)? {
            None => return Err(ImportError::invalid("the file is empty")),
            Some(false) => return Err(ImportError::invalid("the header line is too long")),
            Some(true) => {
                line_no += 1;
                if buf.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                break parse_json_bounded(&buf, &bounds(opts), "header")
                    .map_err(|e| ImportError::invalid(format!("the header line: {e}")))?;
            }
        }
    };
    if header.format != FORMAT || !(MIN_VERSION..=VERSION).contains(&header.version) {
        return Err(ImportError::invalid(format!(
            "not a FastProof export (format {:?} version {})",
            header.format, header.version
        )));
    }
    if !header.fields.is_empty() {
        if header.fields.len() > 1000 {
            return Err(ImportError::limit("maxFields", 1000u64));
        }
        if !visit(Item::FieldDefs(header.fields)) {
            return Err(ImportError::Stopped);
        }
    }
    let v2 = header.version >= 2;
    let (mut count, mut shared, mut reqs, mut lines) = (0usize, 0usize, 0usize, 0usize);
    let line_cap = opts.limits.max_rows
        + if v2 {
            opts.limits.max_shared_steps + opts.limits.max_requirements
        } else {
            0
        };
    while let Some(ok) = read_line(&mut *r, MAX_LINE_BYTES, &mut buf)? {
        line_no += 1;
        if !ok {
            emit_error(
                RowError::new(line_no, "invalid_row", "the line is longer than 16 MiB"),
                visit,
            )?;
            continue;
        }
        if buf.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        lines += 1;
        if lines > line_cap {
            return Err(ImportError::limit("maxImportRows", opts.limits.max_rows));
        }
        let record = if v2 {
            parse_json_bounded::<Record>(&buf, &bounds(opts), "record")
        } else {
            parse_json_bounded::<ImportedCase>(&buf, &bounds(opts), "case")
                .map(|c| Record::Case(Box::new(c)))
        };
        match record {
            Ok(Record::Case(c)) => {
                let mut c = *c;
                count += 1;
                if count > opts.limits.max_rows {
                    return Err(ImportError::limit("maxImportRows", opts.limits.max_rows));
                }
                c.row = line_no;
                let too_big = [&c.name, &c.description, &c.preconditions]
                    .iter()
                    .any(|s| s.len() > opts.limits.max_cell_bytes);
                if too_big {
                    emit_error(
                        RowError::new(
                            line_no,
                            "limit_reached",
                            "a text is longer than maxImportCellBytes",
                        ),
                        visit,
                    )?;
                    continue;
                }
                emit(c, &opts.limits, visit)?;
            }
            Ok(Record::SharedStep(mut x)) => {
                shared += 1;
                if shared > opts.limits.max_shared_steps {
                    return Err(ImportError::limit(
                        "maxSharedSteps",
                        opts.limits.max_shared_steps,
                    ));
                }
                x.row = line_no;
                if x.description.len() > opts.limits.max_cell_bytes {
                    emit_error(
                        RowError::new(
                            line_no,
                            "limit_reached",
                            "a text is longer than maxImportCellBytes",
                        ),
                        visit,
                    )?;
                    continue;
                }
                emit_shared(x, &opts.limits, visit)?;
            }
            Ok(Record::Requirement(mut x)) => {
                reqs += 1;
                if reqs > opts.limits.max_requirements {
                    return Err(ImportError::limit(
                        "maxRequirements",
                        opts.limits.max_requirements,
                    ));
                }
                x.row = line_no;
                let item = if x.title.trim().is_empty() {
                    Item::Error(RowError::new(
                        line_no,
                        "invalid_case",
                        "the requirement has no title",
                    ))
                } else if x.description.len() > opts.limits.max_cell_bytes {
                    Item::Error(RowError::new(
                        line_no,
                        "limit_reached",
                        "a text is longer than maxImportCellBytes",
                    ))
                } else {
                    Item::Requirement(Box::new(x))
                };
                if !visit(item) {
                    return Err(ImportError::Stopped);
                }
            }
            Err(e) => emit_error(RowError::new(line_no, "invalid_row", e.to_string()), visit)?,
        }
    }
    Ok(())
}
