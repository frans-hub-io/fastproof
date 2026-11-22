//! Steps from text cells: numbered lists (`1. Open`, nesting by tabs or `1.2.`), quoted step
//! content (Qase CSV V2), and tab-indented scenarios (TestOps).

use crate::ImportedStep;

/// One parsed item: nesting depth (1 = top) and text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub depth: usize,
    pub text: String,
}

/// Strips a leading `1.`, `1)`, `1.2.` or `1.2` + space; returns the depth the numbering
/// gives (components) and the rest.
fn strip_number(line: &str) -> Option<(usize, &str)> {
    let b = line.as_bytes();
    let mut i = 0;
    let mut parts = 0;
    loop {
        let s = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == s || i - s > 6 {
            return None;
        }
        parts += 1;
        if i < b.len() && b[i] == b'.' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
            i += 1;
            continue;
        }
        break;
    }
    let terminated = i < b.len() && matches!(b[i], b'.' | b')');
    if terminated {
        i += 1;
    } else if parts == 1 {
        return None;
    }
    if i < b.len() && !b[i].is_ascii_whitespace() {
        return None;
    }
    Some((parts, line[i..].trim_start()))
}

/// Reads a quoted step (`"text with ""quotes""` possibly over lines) from `lines` starting at
/// the first line's remainder; returns the text and the lines consumed after the first.
fn quoted(first: &str, rest: &[&str]) -> Option<(String, usize)> {
    let body = first.strip_prefix('"')?;
    let mut text = String::new();
    let mut consumed = 0;
    let mut cur = body;
    loop {
        let b = cur.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'"' {
                if i + 1 < b.len() && b[i + 1] == b'"' {
                    text.push('"');
                    i += 2;
                    continue;
                }
                return Some((text, consumed));
            }
            let ch = cur[i..].chars().next()?;
            text.push(ch);
            i += ch.len_utf8();
        }
        if consumed >= rest.len() {
            return Some((text, consumed));
        }
        text.push('\n');
        cur = rest[consumed];
        consumed += 1;
    }
}

/// Items of a numbered list. Lines without a number continue the previous item; nesting
/// comes from leading tabs or from `1.2.` numbering. With `quotes`, an item whose text starts
/// with `"` runs to the closing quote (Qase CSV V2).
pub fn numbered(text: &str, quotes: bool) -> Vec<Item> {
    let text = text.replace("\r\n", "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<Item> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i];
        i += 1;
        let tabs = raw.len() - raw.trim_start_matches('\t').len();
        let line = raw.trim();
        if line.is_empty() {
            if let Some(last) = out.last_mut()
                && i < lines.len()
                && strip_number(lines[i].trim()).is_none()
                && !lines[i].trim().is_empty()
            {
                last.text.push('\n');
            }
            continue;
        }
        match strip_number(line) {
            Some((parts, rest)) => {
                let depth = (tabs + 1).max(parts);
                let t = if quotes && rest.starts_with('"') {
                    let tail_lines: Vec<&str> = lines[i..].to_vec();
                    match quoted(rest, &tail_lines) {
                        Some((t, used)) => {
                            i += used;
                            t
                        }
                        None => rest.to_string(),
                    }
                } else {
                    rest.to_string()
                };
                out.push(Item { depth, text: t });
            }
            None => match out.last_mut() {
                Some(last) => {
                    if !last.text.is_empty() {
                        last.text.push('\n');
                    }
                    last.text.push_str(line);
                }
                None => {
                    let t = if quotes && line.starts_with('"') {
                        let tail_lines: Vec<&str> = lines[i..].to_vec();
                        match quoted(line, &tail_lines) {
                            Some((t, used)) => {
                                i += used;
                                t
                            }
                            None => line.to_string(),
                        }
                    } else {
                        line.to_string()
                    };
                    out.push(Item {
                        depth: tabs + 1,
                        text: t,
                    })
                }
            },
        }
    }
    for it in &mut out {
        it.text = it.text.trim().to_string();
    }
    out
}

/// Items of a tab-indented scenario: one line per step, depth = leading tabs + 1.
pub fn tabbed(text: &str) -> Vec<Item> {
    text.replace("\r\n", "\n")
        .split('\n')
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let tabs = l.len() - l.trim_start_matches('\t').len();
            Item {
                depth: tabs + 1,
                text: l.trim().to_string(),
            }
        })
        .collect()
}

/// Builds nested steps from items; `expected` and `data` are matched to the items by
/// position. A depth jump of more than one level is clamped.
pub fn tree(items: &[Item], expected: &[Item], data: &[Item]) -> Vec<ImportedStep> {
    let mut root: Vec<ImportedStep> = Vec::new();
    let mut path: Vec<usize> = Vec::new();
    for (n, it) in items.iter().enumerate() {
        let depth = it.depth.clamp(1, path.len() + 1);
        path.truncate(depth - 1);
        let step = ImportedStep {
            action: it.text.clone(),
            expected: expected.get(n).map(|e| e.text.clone()).unwrap_or_default(),
            data: data.get(n).map(|e| e.text.clone()).unwrap_or_default(),
            ..Default::default()
        };
        let mut level = &mut root;
        for &p in &path {
            level = &mut level[p].steps;
        }
        level.push(step);
        path.push(level.len() - 1);
    }
    root
}

/// Text of a step field: HTML converted.
pub fn rich_items(items: Vec<Item>) -> Vec<Item> {
    items
        .into_iter()
        .map(|i| Item {
            depth: i.depth,
            text: crate::rich(&i.text),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_lists() {
        let it = numbered(
            "1. Open\n2. Type\nmore text\n\t3. Nested\n2.1. Also nested",
            false,
        );
        assert_eq!(
            it,
            vec![
                Item {
                    depth: 1,
                    text: "Open".into()
                },
                Item {
                    depth: 1,
                    text: "Type\nmore text".into()
                },
                Item {
                    depth: 2,
                    text: "Nested".into()
                },
                Item {
                    depth: 2,
                    text: "Also nested".into()
                },
            ]
        );
        let it = numbered("1. \"Click \"\"OK\"\"\nand wait\"\n2. \"Done\"", true);
        assert_eq!(it[0].text, "Click \"OK\"\nand wait");
        assert_eq!(it[1].text, "Done");
        assert_eq!(numbered("no numbers", false)[0].text, "no numbers");
        assert_eq!(numbered("2024 was a year", false).len(), 1);
        let t = tree(&it, &numbered("1. ok", false), &[]);
        assert_eq!(t[0].expected, "ok");
        let t = tree(&tabbed("a\n\tb\n\t\tc\nd"), &[], &[]);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].steps[0].steps[0].action, "c");
    }
}
