//! A minimal XLSX writer: a zip of a few XML parts, rows streamed into the sheet entry. Text goes
//! out as inline strings, so a cell that starts with `=` is never a formula.

use std::fs::File;
use std::io::{BufWriter, Write};

use zip::ZipWriter;
use zip::write::SimpleFileOptions;

/// Excel's limit of a cell, in UTF-16 code units.
pub const MAX_CELL_UNITS: usize = 32_767;

pub enum Cell<'a> {
    Text(&'a str),
    Num(i64),
    Empty,
}

/// Escapes `s` for XML, dropping characters XML 1.0 forbids and stopping before `max` UTF-16 units.
pub fn xml_escape(s: &str, max_units: usize) -> String {
    let mut o = String::with_capacity(s.len().min(max_units) + 8);
    let mut units = 0usize;
    for c in s.chars() {
        let c = if super::html::is_format_control(c) {
            '\u{fffd}'
        } else {
            c
        };
        let keep = matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}');
        if !keep {
            continue;
        }
        if units + c.len_utf16() > max_units {
            break;
        }
        units += c.len_utf16();
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c => o.push(c),
        }
    }
    o
}

pub fn column(i: usize) -> String {
    let mut n = i + 1;
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push(b'A' + (n % 26) as u8);
        n /= 26;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

pub fn row_xml(row: usize, cells: &[Cell<'_>], bold: bool) -> String {
    let mut o = String::with_capacity(256);
    o.push_str(&format!("<row r=\"{row}\">"));
    for (i, c) in cells.iter().enumerate() {
        let r = format!("{}{row}", column(i));
        let style = if bold { " s=\"1\"" } else { "" };
        match c {
            Cell::Text(t) if !t.is_empty() => o.push_str(&format!(
                "<c r=\"{r}\" t=\"inlineStr\"{style}><is><t xml:space=\"preserve\">{}</t></is></c>",
                xml_escape(t, MAX_CELL_UNITS)
            )),
            Cell::Num(n) => o.push_str(&format!("<c r=\"{r}\"{style}><v>{n}</v></c>")),
            _ => {}
        }
    }
    o.push_str("</row>");
    o
}

pub struct Workbook {
    zip: ZipWriter<BufWriter<File>>,
    opts: SimpleFileOptions,
}

const XML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

impl Workbook {
    pub fn create(file: File, sheets: &[&str]) -> anyhow::Result<Self> {
        let mut zip = ZipWriter::new(BufWriter::new(file));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        zip.start_file("[Content_Types].xml", opts)?;
        let mut ct = format!(
            "{XML}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>"
        );
        for i in 0..sheets.len() {
            ct.push_str(&format!("<Override PartName=\"/xl/worksheets/sheet{}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>", i + 1));
        }
        ct.push_str("</Types>");
        zip.write_all(ct.as_bytes())?;
        zip.start_file("_rels/.rels", opts)?;
        write!(
            zip,
            "{XML}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>"
        )?;
        zip.start_file("xl/workbook.xml", opts)?;
        write!(
            zip,
            "{XML}<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>"
        )?;
        for (i, n) in sheets.iter().enumerate() {
            write!(
                zip,
                "<sheet name=\"{}\" sheetId=\"{}\" r:id=\"rId{}\"/>",
                xml_escape(n, 31),
                i + 1,
                i + 1
            )?;
        }
        write!(zip, "</sheets></workbook>")?;
        zip.start_file("xl/_rels/workbook.xml.rels", opts)?;
        write!(
            zip,
            "{XML}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">"
        )?;
        for i in 0..sheets.len() {
            write!(
                zip,
                "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{}.xml\"/>",
                i + 1,
                i + 1
            )?;
        }
        write!(
            zip,
            "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>",
            sheets.len() + 1
        )?;
        zip.start_file("xl/styles.xml", opts)?;
        write!(
            zip,
            "{XML}<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><fonts count=\"2\"><font><sz val=\"11\"/><name val=\"Calibri\"/></font><font><b/><sz val=\"11\"/><name val=\"Calibri\"/></font></fonts><fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill></fills><borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs><cellXfs count=\"2\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/><xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\"/></cellXfs></styleSheet>"
        )?;
        Ok(Self { zip, opts })
    }

    /// Starts the next sheet. `widths` are character widths; `header_rows` stay visible and the
    /// autofilter covers `filter_range` when given.
    pub fn start_sheet(
        &mut self,
        index: usize,
        widths: &[f64],
        freeze: bool,
    ) -> anyhow::Result<()> {
        self.zip
            .start_file(format!("xl/worksheets/sheet{index}.xml"), self.opts)?;
        write!(
            self.zip,
            "{XML}<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetViews><sheetView workbookViewId=\"0\"{}>",
            if index == 1 { " tabSelected=\"1\"" } else { "" }
        )?;
        if freeze {
            write!(
                self.zip,
                "<pane ySplit=\"1\" topLeftCell=\"A2\" activePane=\"bottomLeft\" state=\"frozen\"/>"
            )?;
        }
        write!(self.zip, "</sheetView></sheetViews><cols>")?;
        for (i, w) in widths.iter().enumerate() {
            write!(
                self.zip,
                "<col min=\"{0}\" max=\"{0}\" width=\"{w}\" customWidth=\"1\"/>",
                i + 1
            )?;
        }
        write!(self.zip, "</cols><sheetData>")?;
        Ok(())
    }

    pub fn rows(&mut self, xml: &str) -> anyhow::Result<()> {
        self.zip.write_all(xml.as_bytes())?;
        Ok(())
    }

    pub fn end_sheet(&mut self, autofilter: Option<&str>) -> anyhow::Result<()> {
        write!(self.zip, "</sheetData>")?;
        if let Some(r) = autofilter {
            write!(self.zip, "<autoFilter ref=\"{r}\"/>")?;
        }
        write!(self.zip, "</worksheet>")?;
        Ok(())
    }

    pub fn finish(self) -> anyhow::Result<()> {
        self.zip.finish()?.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_and_escapes() {
        assert_eq!(column(0), "A");
        assert_eq!(column(25), "Z");
        assert_eq!(column(26), "AA");
        assert_eq!(
            xml_escape("a<b>&\u{0}\u{1}\"", 100),
            "a&lt;b&gt;&amp;&quot;"
        );
        let r = row_xml(2, &[Cell::Text("=1+1"), Cell::Num(5), Cell::Empty], false);
        assert!(r.contains("t=\"inlineStr\"") && r.contains("<v>5</v>"));
        assert!(!r.contains("<f>"));
    }
}
