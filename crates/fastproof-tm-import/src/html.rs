//! HTML to Markdown on an allowlist: `b`/`strong`, `i`/`em`, `code`, `pre`, `ul`/`ol`/`li`,
//! `a`, `img`, `table`, `br`, `p`. Every other tag is dropped and its content kept as text
//! (the content of `script`, `style`, `template` and comments is dropped). Links keep only
//! `http`, `https` and `mailto` URLs; images keep their source (the importer resolves file
//! names of the attachment zip and the renderer shows only `attachment:` images). Text is
//! escaped so it cannot become Markdown syntax. Linear in the input: lists nest at most
//! [`MAX_LIST_DEPTH`] levels (deeper ones continue at that level), table rows are not padded,
//! and the output stops at [`MAX_OUTPUT_FACTOR`] times the input plus 4 KiB.

/// Nesting of lists kept in the Markdown.
pub const MAX_LIST_DEPTH: usize = 8;
/// Output bytes per input byte, at most (plus 4 KiB).
pub const MAX_OUTPUT_FACTOR: usize = 8;

const ALLOWED: &[&str] = &[
    "b", "strong", "i", "em", "code", "pre", "ul", "ol", "li", "a", "img", "table", "tr", "td",
    "th", "br", "p",
];
const BLOCK: &[&str] = &[
    "div",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "section",
    "article",
    "header",
    "footer",
    "thead",
    "tbody",
    "tfoot",
    "hr",
];
const DROP: &[&str] = &[
    "script", "style", "template", "head", "title", "noscript", "iframe", "object",
];

/// Whether text looks like HTML (a known tag or an entity).
pub fn looks_like_html(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' {
            let mut j = i + 1;
            if j < b.len() && b[j] == b'/' {
                j += 1;
            }
            let start = j;
            while j < b.len() && b[j].is_ascii_alphanumeric() {
                j += 1;
            }
            if j > start && (j == b.len() || matches!(b[j], b'>' | b' ' | b'/' | b'\t' | b'\n')) {
                let name = s[start..j].to_ascii_lowercase();
                if ALLOWED.contains(&name.as_str())
                    || BLOCK.contains(&name.as_str())
                    || DROP.contains(&name.as_str())
                    || matches!(name.as_str(), "span" | "u" | "s" | "font" | "sub" | "sup")
                {
                    return true;
                }
            }
        } else if b[i] == b'&' {
            let rest = &s[i..s.len().min(i + 10)];
            if ["&amp;", "&lt;", "&gt;", "&nbsp;", "&quot;", "&#"]
                .iter()
                .any(|e| rest.starts_with(e))
            {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn decode_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        "ndash" => Some('–'),
        "mdash" => Some('—'),
        "hellip" => Some('…'),
        "laquo" => Some('«'),
        "raquo" => Some('»'),
        "copy" => Some('©'),
        _ => {
            let n = name.strip_prefix('#')?;
            let v = if let Some(h) = n.strip_prefix(['x', 'X']) {
                u32::from_str_radix(h, 16).ok()?
            } else {
                n.parse().ok()?
            };
            char::from_u32(v).filter(|c| *c != '\0')
        }
    }
}

/// Decodes character references.
pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find(';') {
            Some(j) if j <= 10 => match decode_entity(&after[..j]) {
                Some(c) => {
                    out.push(c);
                    rest = &after[j + 1..];
                }
                None => {
                    out.push('&');
                    rest = after;
                }
            },
            _ => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn escape_text(s: &str, out: &mut String) {
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '|' | '#'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
}

fn url_ok(u: &str) -> bool {
    let l = u.trim().to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://") || l.starts_with("mailto:")
}

fn clean_url(u: &str) -> String {
    let mut out = String::new();
    for c in decode_entities(u.trim()).chars() {
        match c {
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

struct Tag {
    name: String,
    closing: bool,
    attrs: Vec<(String, String)>,
}

impl Tag {
    fn attr(&self, k: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }
}

/// Parses a tag starting after `<`; returns the tag and the bytes consumed (through `>`),
/// `Err(true)` when the input ends inside the tag, `Err(false)` when it is no tag.
fn parse_tag(s: &str) -> Result<(Tag, usize), bool> {
    let b = s.as_bytes();
    let mut i = 0;
    let closing = b.first() == Some(&b'/');
    if closing {
        i += 1;
    }
    let start = i;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-') {
        i += 1;
    }
    if i == start {
        return Err(false);
    }
    let name = s[start..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        if i >= b.len() {
            return Err(true);
        }
        if b[i] == b'>' {
            return Ok((
                Tag {
                    name,
                    closing,
                    attrs,
                },
                i + 1,
            ));
        }
        let ks = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        let key = s[ks..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut val = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                i += 1;
                let vs = i;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                val = s[vs..i.min(b.len())].to_string();
                i += 1;
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                    i += 1;
                }
                val = s[vs..i].to_string();
            }
        }
        if !key.is_empty() && attrs.len() < 32 {
            attrs.push((key, val));
        }
    }
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    cell: Option<String>,
    bytes: usize,
}

struct Conv {
    out: String,
    lists: Vec<(bool, usize)>,
    deeper_lists: usize,
    pre: usize,
    code: usize,
    link: Option<(String, usize)>,
    table: Option<Table>,
    drop: Option<String>,
}

impl Conv {
    /// Bytes written so far (the output, the table being read and its open cell).
    fn size(&self) -> usize {
        self.out.len()
            + self
                .table
                .as_ref()
                .map(|t| t.bytes + t.cell.as_ref().map_or(0, String::len))
                .unwrap_or(0)
    }

    fn sink(&mut self) -> &mut String {
        if let Some(t) = &mut self.table
            && let Some(c) = &mut t.cell
        {
            return c;
        }
        &mut self.out
    }

    fn newline(&mut self, n: usize) {
        if self.table.as_ref().is_some_and(|t| t.cell.is_some()) {
            self.sink().push(' ');
            return;
        }
        let trimmed = self.out.trim_end_matches([' ', '\t']).len();
        self.out.truncate(trimmed);
        if self.out.is_empty() {
            return;
        }
        let have = self.out.len() - self.out.trim_end_matches('\n').len();
        for _ in have..n {
            self.out.push('\n');
        }
    }

    fn text(&mut self, t: &str) {
        let t = decode_entities(t);
        if self.pre > 0 {
            let s = self.sink();
            s.push_str(&t);
            return;
        }
        let mut collapsed = String::with_capacity(t.len());
        let mut space = false;
        for c in t.chars() {
            if c.is_whitespace() {
                space = true;
            } else {
                if space {
                    collapsed.push(' ');
                }
                space = false;
                collapsed.push(c);
            }
        }
        if space {
            collapsed.push(' ');
        }
        let at_line_start = {
            let s = self.sink();
            s.is_empty() || s.ends_with('\n')
        };
        let piece = if at_line_start {
            collapsed.trim_start()
        } else {
            collapsed.as_str()
        };
        if self.code > 0 {
            let p = piece.replace('`', "'");
            self.sink().push_str(&p);
        } else {
            let mut e = String::new();
            escape_text(piece, &mut e);
            self.sink().push_str(&e);
        }
    }

    fn tag(&mut self, t: Tag) {
        let name = t.name.as_str();
        if let Some(d) = &self.drop {
            if t.closing && d == name {
                self.drop = None;
            }
            return;
        }
        if DROP.contains(&name) {
            if !t.closing {
                self.drop = Some(name.to_string());
            }
            return;
        }
        match (name, t.closing) {
            ("b" | "strong", _) => {
                if self.pre == 0 && self.code == 0 {
                    self.sink().push_str("**");
                }
            }
            ("i" | "em", _) => {
                if self.pre == 0 && self.code == 0 {
                    self.sink().push('*');
                }
            }
            ("code", false) => {
                if self.pre == 0 {
                    self.code += 1;
                    if self.code == 1 {
                        self.sink().push('`');
                    }
                }
            }
            ("code", true) => {
                if self.pre == 0 && self.code > 0 {
                    self.code -= 1;
                    if self.code == 0 {
                        self.sink().push('`');
                    }
                }
            }
            ("pre", false) => {
                self.newline(2);
                self.out.push_str("```\n");
                self.pre += 1;
            }
            ("pre", true) => {
                if self.pre > 0 {
                    self.pre -= 1;
                    if !self.out.ends_with('\n') {
                        self.out.push('\n');
                    }
                    self.out.push_str("```");
                    self.newline(2);
                }
            }
            ("br", _) => {
                if self.pre > 0 {
                    self.out.push('\n');
                } else if self.table.as_ref().is_some_and(|t| t.cell.is_some()) {
                    self.sink().push(' ');
                } else {
                    let trimmed = self.out.trim_end_matches(' ').len();
                    self.out.truncate(trimmed);
                    if !self.out.is_empty() {
                        self.out.push_str("  \n");
                    }
                }
            }
            ("p", _) => self.newline(2),
            ("ul" | "ol", false) => {
                if self.lists.len() >= MAX_LIST_DEPTH {
                    self.deeper_lists += 1;
                    return;
                }
                let start = t.attr("start").and_then(|v| v.parse().ok()).unwrap_or(1);
                self.newline(if self.lists.is_empty() { 2 } else { 1 });
                self.lists.push((name == "ol", start));
            }
            ("ul" | "ol", true) => {
                if self.deeper_lists > 0 {
                    self.deeper_lists -= 1;
                    return;
                }
                self.lists.pop();
                self.newline(if self.lists.is_empty() { 2 } else { 1 });
            }
            ("li", false) => {
                self.newline(1);
                let depth = self.lists.len().saturating_sub(1);
                let indent = "   ".repeat(depth);
                let marker = match self.lists.last_mut() {
                    Some((true, n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "- ".to_string(),
                };
                self.out.push_str(&indent);
                self.out.push_str(&marker);
            }
            ("li", true) => {}
            ("a", false) => {
                let href = t.attr("href").unwrap_or("").to_string();
                let at = self.sink().len();
                self.sink().push('[');
                self.link = Some((href, at));
            }
            ("a", true) => {
                if let Some((href, at)) = self.link.take() {
                    if url_ok(&href) {
                        let u = clean_url(&href);
                        self.sink().push_str(&format!("]({u})"));
                    } else {
                        let s = self.sink();
                        if at < s.len() {
                            s.remove(at);
                        }
                    }
                }
            }
            ("img", false) => {
                let src = clean_url(t.attr("src").unwrap_or(""));
                if src.is_empty()
                    || src.to_ascii_lowercase().starts_with("data:")
                    || src.to_ascii_lowercase().starts_with("javascript:")
                {
                    return;
                }
                let mut alt = String::new();
                escape_text(&decode_entities(t.attr("alt").unwrap_or("")), &mut alt);
                self.sink().push_str(&format!("![{alt}]({src})"));
            }
            ("table", false) => {
                self.newline(2);
                self.table = Some(Table::default());
            }
            ("table", true) => {
                if let Some(t) = self.table.take() {
                    self.out.push_str(&render_table(t));
                    self.newline(2);
                }
            }
            ("tr", false) => {
                if let Some(t) = &mut self.table {
                    t.rows.push(Vec::new());
                }
            }
            ("td" | "th", false) => {
                if let Some(t) = &mut self.table {
                    if t.rows.is_empty() {
                        t.rows.push(Vec::new());
                    }
                    t.cell = Some(String::new());
                }
            }
            ("td" | "th", true) => {
                if let Some(t) = &mut self.table
                    && let Some(c) = t.cell.take()
                    && let Some(r) = t.rows.last_mut()
                {
                    let c = c.trim().to_string();
                    t.bytes += c.len() + 9;
                    r.push(c);
                }
            }
            (n, _) if BLOCK.contains(&n) => self.newline(2),
            _ => {}
        }
    }
}

fn render_table(t: Table) -> String {
    let rows: Vec<Vec<String>> = t.rows.into_iter().filter(|r| !r.is_empty()).collect();
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return String::new();
    }
    let mut out = String::new();
    for (i, r) in rows.iter().enumerate() {
        out.push('|');
        let n = if i == 0 { cols } else { r.len() };
        for c in 0..n {
            out.push(' ');
            out.push_str(r.get(c).map(String::as_str).unwrap_or(""));
            out.push_str(" |");
        }
        out.push('\n');
        if i == 0 {
            out.push('|');
            for _ in 0..cols {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
    out
}

/// Converts HTML to Markdown (see the module documentation).
pub fn to_markdown(html: &str) -> String {
    to_markdown_max(
        html,
        html.len()
            .saturating_mul(MAX_OUTPUT_FACTOR)
            .saturating_add(4096),
    )
}

/// [`to_markdown`] with at most `max` bytes of output: the conversion stops there.
pub fn to_markdown_max(html: &str, max: usize) -> String {
    let mut c = Conv {
        out: String::with_capacity(html.len().min(max)),
        lists: Vec::new(),
        deeper_lists: 0,
        pre: 0,
        code: 0,
        link: None,
        table: None,
        drop: None,
    };
    let mut rest = html;
    while !rest.is_empty() {
        if c.size() > max {
            break;
        }
        let Some(i) = rest.find('<') else {
            if c.drop.is_none() {
                c.text(rest);
            }
            break;
        };
        if i > 0 && c.drop.is_none() {
            c.text(&rest[..i]);
        }
        let after = &rest[i + 1..];
        if let Some(body) = after.strip_prefix("!--") {
            rest = match body.find("-->") {
                Some(j) => &body[j + 3..],
                None => "",
            };
            continue;
        }
        if after.starts_with('!') || after.starts_with('?') {
            rest = match after.find('>') {
                Some(j) => &after[j + 1..],
                None => "",
            };
            continue;
        }
        match parse_tag(after) {
            Ok((t, used)) => {
                c.tag(t);
                rest = &after[used..];
            }
            Err(unterminated) => {
                if c.drop.is_none() {
                    c.text("<");
                    if unterminated {
                        c.text(after);
                        break;
                    }
                }
                if unterminated {
                    break;
                }
                rest = after;
            }
        }
    }
    if let Some(t) = c.table.take() {
        c.out.push_str(&render_table(t));
    }
    if c.pre > 0 {
        c.out.push_str("\n```");
    }
    if c.out.len() > max {
        let mut i = max;
        while !c.out.is_char_boundary(i) {
            i -= 1;
        }
        c.out.truncate(i);
    }
    let lines: Vec<&str> = c.out.lines().map(|l| l.trim_end_matches(' ')).collect();
    let mut s = String::new();
    let mut blank = 0;
    for l in lines {
        if l.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        s.push_str(l);
        s.push('\n');
    }
    s.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist() {
        assert_eq!(
            to_markdown("<p>Hello <b>bold</b> and <em>it</em></p><p>Two</p>"),
            "Hello **bold** and *it*\n\nTwo"
        );
        assert_eq!(
            to_markdown("<ul><li>one</li><li>two</li></ul>"),
            "- one\n- two"
        );
        assert_eq!(
            to_markdown("<ol><li>a</li><li>b<ol><li>c</li></ol></li></ol>"),
            "1. a\n2. b\n   1. c"
        );
        assert_eq!(to_markdown("a<br>b<br/>c"), "a\nb\nc");
        assert_eq!(
            to_markdown("<a href=\"https://example.com/x y\">site</a>"),
            "[site](https://example.com/x%20y)"
        );
        assert_eq!(to_markdown("<a href=\"javascript:alert(1)\">x</a>"), "x");
        assert_eq!(
            to_markdown("<img src=\"shot.png\" alt=\"Shot\">"),
            "![Shot](shot.png)"
        );
        assert_eq!(to_markdown("<img src=\"data:image/png;base64,AA\">"), "");
        assert_eq!(
            to_markdown("<pre><code>let x = 1;\n&lt;y&gt;</code></pre>"),
            "```\nlet x = 1;\n<y>\n```"
        );
        assert_eq!(to_markdown("use <code>a*b</code>"), "use `a*b`");
        assert_eq!(
            to_markdown(
                "<table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>x|y</td></tr></table>"
            ),
            "| A | B |\n| --- | --- |\n| 1 | x\\|y |"
        );
        assert_eq!(to_markdown("<script>alert(1)</script>safe"), "safe");
        assert_eq!(
            to_markdown("<div onclick=\"x\"><span style=\"c\">t</span></div>"),
            "t"
        );
        assert_eq!(
            to_markdown("1 * 2 = [x] &amp; &#60;b&#x3e;"),
            "1 \\* 2 = \\[x\\] & \\<b\\>"
        );
        assert_eq!(to_markdown("<!-- c --><h1>Title</h1>text"), "Title\n\ntext");
        assert_eq!(to_markdown("a < b"), "a \\< b");
        assert!(looks_like_html("<p>x</p>"));
        assert!(looks_like_html("a &amp; b"));
        assert!(!looks_like_html("a < b and x > y"));
        assert!(!looks_like_html("plain"));
    }

    #[test]
    fn hostile_input_is_linear() {
        let s = "<".repeat(100_000) + &"<b>".repeat(50_000) + &"<b x='".repeat(50_000);
        let t = std::time::Instant::now();
        let _ = to_markdown(&s);
        assert!(t.elapsed().as_secs() < 5);
        let _ = to_markdown("<a href='x");
        let _ = to_markdown("<table><td>");
        let _ = to_markdown("<pre>open");
    }
}
