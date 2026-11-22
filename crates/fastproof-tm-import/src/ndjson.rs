//! The FastProof NDJSON export: a header line `{"format": "fastproof-cases", "version": 1,
//! "project", "exportedAt", "fields": [field definitions]}`, then one [`ImportedCase`] per
//! line (without `row`). Full fidelity: folders, steps with their ids, links, fields, tags,
//! keys, status, code identities and attachments (by zip entry name).

use std::io::BufRead;

use fastproof_ingest::{JsonBounds, parse_json_bounded};
use serde::{Deserialize, Serialize};

use crate::text::io_error;
use crate::{
    ImportError, ImportedCase, ImportedFieldDef, Item, MAX_JSON_DEPTH, ParseOptions, RowError,
    Visit, emit, emit_error,
};

/// `format` of the header line.
pub const FORMAT: &str = "fastproof-cases";
/// Version of the format.
pub const VERSION: u32 = 1;
/// Bytes of one line, at most.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

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
        max_values: 10_000_000,
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
    if header.format != FORMAT || header.version != VERSION {
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
    let mut count = 0usize;
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
        count += 1;
        if count > opts.limits.max_rows {
            return Err(ImportError::limit("maxImportRows", opts.limits.max_rows));
        }
        match parse_json_bounded::<ImportedCase>(&buf, &bounds(opts), "case") {
            Ok(mut c) => {
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
            Err(e) => emit_error(RowError::new(line_no, "invalid_row", e.to_string()), visit)?,
        }
    }
    Ok(())
}
