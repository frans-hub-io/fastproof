//! A streaming CSV reader (RFC 4180, lenient) with limits: rows, columns, bytes per cell.
//! The delimiter (`,`, `;` or tab) is taken from the header line. Cells are text; nothing is
//! ever evaluated (a cell starting with `=` stays that text).

use std::io::BufRead;

use crate::text::io_error;
use crate::{ImportError, ParseLimits, RowError};

/// A CSV record: its row number (the header is row 1) and its cells, or why it was dropped.
pub type Record = (u64, Result<Vec<String>, RowError>);

pub struct CsvReader<'a> {
    r: Box<dyn BufRead + 'a>,
    delim: u8,
    limits: ParseLimits,
    row: u64,
    skip_lf: bool,
    eof: bool,
}

fn guess_delimiter(head: &[u8]) -> u8 {
    let mut counts = [0usize; 3];
    let mut quoted = false;
    for &b in head {
        match b {
            b'"' => quoted = !quoted,
            b'\n' | b'\r' if !quoted => break,
            b',' if !quoted => counts[0] += 1,
            b';' if !quoted => counts[1] += 1,
            b'\t' if !quoted => counts[2] += 1,
            _ => {}
        }
    }
    let delims = *b",;\t";
    let mut best = 0;
    for i in 1..3 {
        if counts[i] > counts[best] {
            best = i;
        }
    }
    delims[best]
}

impl<'a> CsvReader<'a> {
    pub fn new(mut r: Box<dyn BufRead + 'a>, limits: ParseLimits) -> Result<Self, ImportError> {
        let head = r.fill_buf().map_err(io_error)?;
        let delim = guess_delimiter(head);
        Ok(Self {
            r,
            delim,
            limits,
            row: 0,
            skip_lf: false,
            eof: false,
        })
    }

    pub fn delimiter(&self) -> u8 {
        self.delim
    }

    /// The next record (blank lines are skipped), `None` at the end. More than
    /// `max_rows` records after the header fail with `maxImportRows`.
    pub fn next_record(&mut self) -> Result<Option<Record>, ImportError> {
        loop {
            let Some((row, rec)) = self.read_raw()? else {
                return Ok(None);
            };
            if row > 1 && (row - 1) as usize > self.limits.max_rows {
                return Err(ImportError::limit("maxImportRows", self.limits.max_rows));
            }
            if let Ok(cells) = &rec
                && cells.iter().all(|c| c.is_empty())
            {
                continue;
            }
            return Ok(Some((row, rec)));
        }
    }

    fn read_raw(&mut self) -> Result<Option<Record>, ImportError> {
        if self.eof {
            return Ok(None);
        }
        let max_cell = self.limits.max_cell_bytes;
        let max_cols = self.limits.max_columns;
        let mut cells: Vec<String> = Vec::new();
        let mut cell: Vec<u8> = Vec::new();
        let mut quoted = false;
        let mut quote_closed = false;
        let mut started = false;
        let mut error: Option<String> = None;
        let mut any = false;
        loop {
            let buf = self.r.fill_buf().map_err(io_error)?;
            if buf.is_empty() {
                self.eof = true;
                if !any {
                    return Ok(None);
                }
                break;
            }
            let mut used = 0;
            let mut end = false;
            for &b in buf {
                used += 1;
                if self.skip_lf {
                    self.skip_lf = false;
                    if b == b'\n' {
                        continue;
                    }
                }
                any = true;
                if quoted {
                    if b == b'"' {
                        quoted = false;
                        quote_closed = true;
                    } else if cell.len() < max_cell {
                        cell.push(b);
                    } else if error.is_none() {
                        error = Some(format!("a cell has more than {max_cell} bytes"));
                    }
                    continue;
                }
                if b == b'"' {
                    if quote_closed {
                        quoted = true;
                        quote_closed = false;
                        if cell.len() < max_cell {
                            cell.push(b'"');
                        }
                        continue;
                    }
                    if !started {
                        quoted = true;
                        started = true;
                        continue;
                    }
                }
                if b == self.delim {
                    push_cell(&mut cells, &mut cell, max_cols, &mut error);
                    started = false;
                    quote_closed = false;
                    continue;
                }
                if b == b'\n' || b == b'\r' {
                    if b == b'\r' {
                        self.skip_lf = true;
                    }
                    end = true;
                    break;
                }
                started = true;
                if cell.len() < max_cell {
                    cell.push(b);
                } else if error.is_none() {
                    error = Some(format!("a cell has more than {max_cell} bytes"));
                }
            }
            self.r.consume(used);
            if end {
                break;
            }
        }
        push_cell(&mut cells, &mut cell, max_cols, &mut error);
        self.row += 1;
        let row = self.row;
        if quoted {
            error.get_or_insert_with(|| "a quoted cell is not closed".into());
        }
        Ok(Some((
            row,
            match error {
                Some(m) => Err(RowError::new(row, "invalid_row", m)),
                None => Ok(cells),
            },
        )))
    }
}

fn push_cell(
    cells: &mut Vec<String>,
    cell: &mut Vec<u8>,
    max_cols: usize,
    error: &mut Option<String>,
) {
    if cells.len() >= max_cols {
        error.get_or_insert_with(|| format!("more than {max_cols} columns"));
        cell.clear();
        return;
    }
    let s = match String::from_utf8(std::mem::take(cell)) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    cells.push(s);
}

/// A CSV with its header: cells by column name.
pub struct Table<'a> {
    pub reader: CsvReader<'a>,
    pub headers: Vec<String>,
    index: std::collections::HashMap<String, usize>,
}

impl<'a> Table<'a> {
    /// Reads the header. More than `max_columns` header cells fail with `maxColumns`.
    pub fn open(r: Box<dyn BufRead + 'a>, limits: ParseLimits) -> Result<Self, ImportError> {
        let mut reader = CsvReader::new(r, limits)?;
        let headers = match reader.next_record()? {
            Some((_, Ok(h))) => h,
            Some((_, Err(e))) => {
                if e.message.contains("columns") {
                    return Err(ImportError::limit("maxColumns", limits.max_columns));
                }
                return Err(ImportError::invalid(format!(
                    "the header row: {}",
                    e.message
                )));
            }
            None => return Err(ImportError::invalid("the file is empty")),
        };
        let headers: Vec<String> = headers.into_iter().map(|h| h.trim().to_string()).collect();
        let mut index = std::collections::HashMap::new();
        for (i, h) in headers.iter().enumerate() {
            index.entry(h.to_lowercase()).or_insert(i);
        }
        Ok(Self {
            reader,
            headers,
            index,
        })
    }

    /// The column of a header (case-insensitive).
    pub fn col(&self, name: &str) -> Option<usize> {
        self.index.get(&name.to_lowercase()).copied()
    }

    /// The first of several names that exists.
    pub fn col_any(&self, names: &[&str]) -> Option<usize> {
        names.iter().find_map(|n| self.col(n))
    }
}

/// The cell of a column in a record ("" when the record is short or the column absent).
pub fn cell(rec: &[String], col: Option<usize>) -> &str {
    col.and_then(|i| rec.get(i))
        .map(String::as_str)
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(s: &str, l: ParseLimits) -> Vec<Record> {
        let mut r = CsvReader::new(Box::new(s.as_bytes()), l).unwrap();
        let mut out = Vec::new();
        while let Some(x) = r.next_record().unwrap() {
            out.push(x);
        }
        out
    }

    #[test]
    fn quotes_newlines_delimiters() {
        let l = ParseLimits::default();
        let r = rows("a,b,c\r\n1,\"x, \"\"y\"\"\nz\",=SUM(A1)\n\n2,,\n", l);
        assert_eq!(r.len(), 3);
        assert_eq!(
            r[1].1.as_ref().unwrap(),
            &vec!["1", "x, \"y\"\nz", "=SUM(A1)"]
        );
        assert_eq!(r[2].0, 4);
        let r = rows("a;b\n1;2", l);
        assert_eq!(r[1].1.as_ref().unwrap(), &vec!["1", "2"]);
        let r = rows("a\tb\n1\t2\n", l);
        assert_eq!(r[1].1.as_ref().unwrap(), &vec!["1", "2"]);
    }

    #[test]
    fn limits() {
        let l = ParseLimits {
            max_cell_bytes: 4,
            max_columns: 3,
            max_rows: 2,
            ..Default::default()
        };
        let r = rows("a,b\n12345,x\n1,2,3,4\n", l);
        assert!(r[1].1.is_err());
        assert!(r[2].1.as_ref().unwrap_err().message.contains("columns"));
        let mut rd = CsvReader::new(Box::new(&b"a\n1\n2\n3\n"[..]), l).unwrap();
        let mut n = 0;
        let err = loop {
            match rd.next_record() {
                Ok(Some(_)) => n += 1,
                Ok(None) => panic!("no limit"),
                Err(e) => break e,
            }
        };
        assert_eq!(n, 3);
        assert!(matches!(
            err,
            ImportError::Limit {
                limit: "maxImportRows",
                ..
            }
        ));
        let r = rows("a\n\"open", ParseLimits::default());
        assert!(r[1].1.is_err());
    }
}
