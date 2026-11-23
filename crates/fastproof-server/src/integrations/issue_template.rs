//! Titles and descriptions of new tracker issues from `{{placeholder}}` templates.
//!
//! The template text is trusted (project settings); every value from a result is not (test
//! names, messages and traces are written by uploaders). Values are escaped for the markup of
//! the tracker so that they cannot add links, mentions, images or markup: Markdown (GitHub,
//! GitLab), Jira wiki markup (Jira Server/Data Center), Atlassian Document Format (Jira Cloud,
//! where text nodes are inert) and HTML (Azure DevOps). Messages and traces go into code blocks,
//! and the only links are FastProof's own pages.

use serde_json::{Value, json};

/// Inline placeholders.
pub const INLINE: [&str; 11] = [
    "name",
    "fullName",
    "status",
    "messageLine",
    "launch",
    "branch",
    "environment",
    "fingerprint",
    "count",
    "defect",
    "project",
];
/// Placeholders that become links to FastProof pages.
pub const LINKS: [&str; 2] = ["launchUrl", "resultUrl"];
/// Placeholders that become blocks; they stand alone on their line.
pub const BLOCKS: [&str; 3] = ["message", "trace", "tests"];
/// Other inline placeholders: who creates the issue.
pub const USER: &str = "user";

/// Default title.
pub const DEFAULT_TITLE: &str = "{{name}}: {{messageLine}}";
/// Default description.
pub const DEFAULT_BODY: &str = "Test: {{fullName}}\nStatus: {{status}}\nLaunch: {{launch}} {{launchUrl}}\n\
Result: {{resultUrl}}\nBranch: {{branch}}\nEnvironment: {{environment}}\n\
Results with this failure in the launch: {{count}}\n\n{{message}}\n\n{{trace}}\n\n{{tests}}\n\n\
Created from FastProof by {{user}}.";

const MAX_VALUE: usize = 300;
const MAX_MESSAGE: usize = 4000;
const MAX_TRACE_LINES: usize = 60;
const MAX_TRACE: usize = 6000;
/// Test names listed, at most.
pub const MAX_TESTS: usize = 20;
const ZWSP: char = '\u{200B}';

/// What a template is filled with.
#[derive(Clone, Debug, Default)]
pub struct IssueContext {
    pub project: String,
    pub name: String,
    pub full_name: String,
    pub status: String,
    pub message: Option<String>,
    pub trace: Option<String>,
    pub launch: String,
    pub launch_url: Option<String>,
    pub result_url: Option<String>,
    pub branch: Option<String>,
    pub environment: Option<String>,
    pub fingerprint: Option<String>,
    pub count: usize,
    /// Names of the tests behind the issue (a cluster), at most [`MAX_TESTS`] are shown.
    pub tests: Vec<String>,
    pub defect: Option<String>,
    pub user: String,
}

/// Text in a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
    /// From the template.
    Text(String),
    /// From a result.
    Value(String),
    /// A FastProof page.
    Link(String),
}

/// A block of the description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// Lines of a paragraph.
    Para(Vec<Vec<Inline>>),
    Code(String),
    List(Vec<String>),
}

enum Part<'a> {
    Text(&'a str),
    Var(&'a str),
}

fn parts(line: &str) -> Result<Vec<Part<'_>>, String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(i) = rest.find("{{") {
        if i > 0 {
            out.push(Part::Text(&rest[..i]));
        }
        let after = &rest[i + 2..];
        let j = after
            .find("}}")
            .ok_or_else(|| "a {{ is not closed".to_string())?;
        out.push(Part::Var(after[..j].trim()));
        rest = &after[j + 2..];
    }
    if !rest.is_empty() {
        out.push(Part::Text(rest));
    }
    Ok(out)
}

fn known(v: &str) -> bool {
    INLINE.contains(&v) || LINKS.contains(&v) || BLOCKS.contains(&v) || v == USER
}

/// Checks a template: known placeholders, blocks alone on their line (none in a title), size.
pub fn check(t: &str, max: usize, title: bool) -> Result<(), String> {
    if t.trim().is_empty() || t.len() > max {
        return Err(format!("1 to {max} bytes"));
    }
    if t.contains('\0') {
        return Err("no NUL".into());
    }
    if title && t.contains('\n') {
        return Err("one line".into());
    }
    for line in t.lines() {
        let ps = parts(line)?;
        let n = ps.len();
        for p in &ps {
            if let Part::Var(v) = p {
                if !known(v) {
                    return Err(format!("unknown placeholder {{{{{v}}}}}"));
                }
                if BLOCKS.contains(v) && (title || n != 1) {
                    return Err(format!(
                        "{{{{{v}}}}} is a block: put it alone on its line of the description"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn inline_value(c: &IssueContext, v: &str) -> String {
    let s = match v {
        "name" => c.name.clone(),
        "fullName" => c.full_name.clone(),
        "status" => c.status.clone(),
        "messageLine" => c
            .message
            .as_deref()
            .and_then(|m| m.lines().find(|l| !l.trim().is_empty()))
            .unwrap_or_default()
            .to_string(),
        "launch" => c.launch.clone(),
        "branch" => c.branch.clone().unwrap_or_default(),
        "environment" => c.environment.clone().unwrap_or_default(),
        "fingerprint" => c.fingerprint.clone().unwrap_or_default(),
        "count" => c.count.to_string(),
        "defect" => c.defect.clone().unwrap_or_default(),
        "project" => c.project.clone(),
        USER => c.user.clone(),
        _ => String::new(),
    };
    cut(&one_line(&s), MAX_VALUE)
}

fn trace(t: &str) -> String {
    let lines: Vec<&str> = t.lines().collect();
    let mut out = lines
        .iter()
        .take(MAX_TRACE_LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if lines.len() > MAX_TRACE_LINES {
        out.push_str(&format!("\n… {} more lines", lines.len() - MAX_TRACE_LINES));
    }
    cut(&out, MAX_TRACE)
}

fn clean_block(s: &str) -> String {
    s.chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// The title: one line of at most 255 characters.
pub fn render_title(template: &str, c: &IssueContext) -> String {
    let mut out = String::new();
    for p in parts(template).unwrap_or_default() {
        match p {
            Part::Text(t) => out.push_str(t),
            Part::Var(v) if LINKS.contains(&v) => {
                let u = if v == "launchUrl" {
                    &c.launch_url
                } else {
                    &c.result_url
                };
                out.push_str(u.as_deref().unwrap_or_default());
            }
            Part::Var(v) => out.push_str(&inline_value(c, v)),
        }
    }
    let t = one_line(&out);
    let t = t.trim_matches(|ch: char| ch == ':' || ch.is_whitespace());
    let t = if t.is_empty() { c.name.as_str() } else { t };
    t.chars().take(255).collect()
}

/// The description as blocks.
pub fn render(template: &str, c: &IssueContext) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para: Vec<Vec<Inline>> = Vec::new();
    let flush = |para: &mut Vec<Vec<Inline>>, blocks: &mut Vec<Block>| {
        if !para.is_empty() {
            blocks.push(Block::Para(std::mem::take(para)));
        }
    };
    for line in template.lines() {
        let ps = parts(line).unwrap_or_default();
        if line.trim().is_empty() {
            flush(&mut para, &mut blocks);
            continue;
        }
        if let [Part::Var(v)] = ps.as_slice()
            && BLOCKS.contains(v)
        {
            flush(&mut para, &mut blocks);
            match *v {
                "message" => {
                    if let Some(m) = c.message.as_deref().filter(|m| !m.trim().is_empty()) {
                        blocks.push(Block::Code(cut(&clean_block(m), MAX_MESSAGE)));
                    }
                }
                "trace" => {
                    if let Some(t) = c.trace.as_deref().filter(|t| !t.trim().is_empty()) {
                        blocks.push(Block::Code(trace(&clean_block(t))));
                    }
                }
                _ => {
                    if c.tests.len() > 1 {
                        let mut items: Vec<String> = c
                            .tests
                            .iter()
                            .take(MAX_TESTS)
                            .map(|n| cut(&one_line(n), MAX_VALUE))
                            .collect();
                        if c.count > MAX_TESTS {
                            items.push(format!("… and {} more", c.count - MAX_TESTS));
                        }
                        blocks.push(Block::List(items));
                    }
                }
            }
            continue;
        }
        let mut out = Vec::new();
        let mut vars = 0;
        let mut filled = 0;
        for p in ps {
            match p {
                Part::Text(t) => out.push(Inline::Text(t.to_string())),
                Part::Var(v) if LINKS.contains(&v) => {
                    vars += 1;
                    let u = if v == "launchUrl" {
                        &c.launch_url
                    } else {
                        &c.result_url
                    };
                    if let Some(u) = u {
                        filled += 1;
                        out.push(Inline::Link(u.clone()));
                    }
                }
                Part::Var(v) => {
                    vars += 1;
                    let s = inline_value(c, v);
                    if !s.is_empty() {
                        filled += 1;
                        out.push(Inline::Value(s));
                    }
                }
            }
        }
        if vars > 0 && filled == 0 {
            continue;
        }
        para.push(out);
    }
    flush(&mut para, &mut blocks);
    blocks
}

fn fence(code: &str) -> String {
    let mut run = 0;
    let mut longest = 0;
    for ch in code.chars() {
        if ch == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

/// Markdown (GitHub, GitLab).
pub fn markdown(blocks: &[Block]) -> String {
    let mut out: Vec<String> = Vec::new();
    for b in blocks {
        match b {
            Block::Para(lines) => {
                let text: Vec<String> = lines
                    .iter()
                    .map(|l| {
                        l.iter()
                            .map(|i| match i {
                                Inline::Text(t) => t.clone(),
                                Inline::Value(v) => super::vcs::md(v),
                                Inline::Link(u) => format!("<{u}>"),
                            })
                            .collect::<String>()
                    })
                    .collect();
                out.push(text.join("  \n"));
            }
            Block::Code(c) => {
                let f = fence(c);
                out.push(format!("{f}text\n{c}\n{f}"));
            }
            Block::List(items) => out.push(
                items
                    .iter()
                    .map(|n| format!("- {}", super::vcs::md(n)))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
    }
    out.join("\n\n")
}

fn wiki_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '*' | '_' | '?' | '-' | '+' | '^' | '~' | '{' | '}' | '[' | ']' | '|' | '!' | '#'
            | '(' | ')' | '@' => {
                out.push('\\');
                out.push(c);
            }
            ':' if chars.get(i + 1) == Some(&'/') => {
                out.push(':');
                out.push(ZWSP);
            }
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Jira wiki markup (Jira Server and Data Center).
pub fn jira_wiki(blocks: &[Block]) -> String {
    let mut out: Vec<String> = Vec::new();
    for b in blocks {
        match b {
            Block::Para(lines) => out.push(
                lines
                    .iter()
                    .map(|l| {
                        l.iter()
                            .map(|i| match i {
                                Inline::Text(t) => t.clone(),
                                Inline::Value(v) => wiki_escape(v),
                                Inline::Link(u) => format!("[{u}]"),
                            })
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Block::Code(c) => out.push(format!(
                "{{noformat}}\n{}\n{{noformat}}",
                c.replace("{noformat}", "{ noformat}")
            )),
            Block::List(items) => out.push(
                items
                    .iter()
                    .map(|n| format!("* {}", wiki_escape(n)))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
    }
    out.join("\n\n")
}

fn adf_text(t: &str) -> Option<Value> {
    (!t.is_empty()).then(|| json!({"type": "text", "text": t}))
}

/// Atlassian Document Format (Jira Cloud): text nodes are never parsed for markup.
pub fn adf(blocks: &[Block]) -> Value {
    let mut content = Vec::new();
    for b in blocks {
        match b {
            Block::Para(lines) => {
                let mut nodes = Vec::new();
                for (n, l) in lines.iter().enumerate() {
                    if n > 0 {
                        nodes.push(json!({"type": "hardBreak"}));
                    }
                    for i in l {
                        match i {
                            Inline::Text(t) | Inline::Value(t) => nodes.extend(adf_text(t)),
                            Inline::Link(u) => nodes.push(json!({"type": "text", "text": u,
                                "marks": [{"type": "link", "attrs": {"href": u}}]})),
                        }
                    }
                }
                if !nodes.is_empty() {
                    content.push(json!({"type": "paragraph", "content": nodes}));
                }
            }
            Block::Code(c) => {
                content.push(json!({"type": "codeBlock", "content": adf_text(c).into_iter().collect::<Vec<_>>()}))
            }
            Block::List(items) => content.push(json!({"type": "bulletList", "content": items.iter()
                .map(|n| json!({"type": "listItem", "content": [{"type": "paragraph",
                    "content": adf_text(n).into_iter().collect::<Vec<_>>()}]}))
                .collect::<Vec<_>>()})),
        }
    }
    json!({"type": "doc", "version": 1, "content": content})
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if c.is_control() && c != '\n' && c != '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// HTML (Azure DevOps rich text fields); the template text is escaped too.
pub fn html(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        match b {
            Block::Para(lines) => {
                out.push_str("<div>");
                let ls: Vec<String> = lines
                    .iter()
                    .map(|l| {
                        l.iter()
                            .map(|i| match i {
                                Inline::Text(t) | Inline::Value(t) => html_escape(t),
                                Inline::Link(u) => {
                                    let e = html_escape(u);
                                    format!("<a href=\"{e}\">{e}</a>")
                                }
                            })
                            .collect::<String>()
                    })
                    .collect();
                out.push_str(&ls.join("<br>"));
                out.push_str("</div>");
            }
            Block::Code(c) => {
                out.push_str("<pre>");
                out.push_str(&html_escape(c));
                out.push_str("</pre>");
            }
            Block::List(items) => {
                out.push_str("<ul>");
                for n in items {
                    out.push_str("<li>");
                    out.push_str(&html_escape(n));
                    out.push_str("</li>");
                }
                out.push_str("</ul>");
            }
        }
    }
    out
}

/// The description as plain text (previews in the UI).
pub fn plain(blocks: &[Block]) -> String {
    let mut out: Vec<String> = Vec::new();
    for b in blocks {
        match b {
            Block::Para(lines) => out.push(
                lines
                    .iter()
                    .map(|l| {
                        l.iter()
                            .map(|i| match i {
                                Inline::Text(t) | Inline::Value(t) | Inline::Link(t) => t.as_str(),
                            })
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Block::Code(c) => out.push(c.clone()),
            Block::List(items) => out.push(
                items
                    .iter()
                    .map(|n| format!("- {n}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
    }
    out.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> IssueContext {
        IssueContext {
            project: "SHOP".into(),
            name: "checkout [pays]".into(),
            full_name: "shop.Checkout.pays @admin".into(),
            status: "failed".into(),
            message: Some("expected 200, got 500\nsee http://evil.example.net/x ```".into()),
            trace: Some(
                (1..=80)
                    .map(|n| format!("at line {n}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            launch: "#12 nightly".into(),
            launch_url: Some("https://fastproof.example.com/p/SHOP/launches/1".into()),
            result_url: Some("https://fastproof.example.com/p/SHOP/results/2".into()),
            branch: Some("main".into()),
            environment: None,
            fingerprint: Some("0123456789abcdef".into()),
            count: 3,
            tests: vec!["a".into(), "b <img src=x>".into(), "c".into()],
            defect: None,
            user: "alice".into(),
        }
    }

    #[test]
    fn checks_templates() {
        assert!(check(DEFAULT_TITLE, 255, true).is_ok());
        assert!(check(DEFAULT_BODY, 4096, false).is_ok());
        assert!(check("{{nope}}", 255, true).is_err());
        assert!(check("{{message}}", 255, true).is_err());
        assert!(check("Error: {{trace}}", 4096, false).is_err());
        assert!(check("{{name", 255, true).is_err());
        assert!(check("a\nb", 255, true).is_err());
    }

    #[test]
    fn title_is_one_line() {
        let t = render_title(DEFAULT_TITLE, &ctx());
        assert_eq!(t, "checkout [pays]: expected 200, got 500");
        let mut c = ctx();
        c.message = None;
        assert_eq!(render_title(DEFAULT_TITLE, &c), "checkout [pays]");
    }

    #[test]
    fn markdown_escapes_values_and_fences_code() {
        let b = render(DEFAULT_BODY, &ctx());
        let md = markdown(&b);
        assert!(md.contains("````text\nexpected 200"), "{md}");
        assert!(md.contains("\\@\u{200B}admin"), "{md}");
        assert!(md.contains("&lt;img") && !md.contains("<img"), "{md}");
        assert!(md.contains("<https://fastproof.example.com/p/SHOP/results/2>"));
        assert!(md.contains("… 20 more lines"), "{md}");
        assert!(
            !md.contains("Environment:"),
            "empty lines are dropped: {md}"
        );
        assert!(md.contains("Created from FastProof by alice."));
    }

    #[test]
    fn jira_wiki_escapes_markup() {
        let w = jira_wiki(&render(DEFAULT_BODY, &ctx()));
        assert!(w.contains("checkout") || w.contains("Checkout"));
        assert!(w.contains("\\@admin"), "{w}");
        assert!(w.contains("{noformat}\nexpected 200"), "{w}");
        assert!(w.contains("[https://fastproof.example.com/p/SHOP/results/2]"));
        let mut c = ctx();
        c.message = Some("x {noformat} y".into());
        assert!(jira_wiki(&render("{{message}}", &c)).contains("x { noformat} y"));
        assert!(wiki_escape("see http://x").contains(&format!(":{ZWSP}//")));
    }

    #[test]
    fn adf_has_only_text_nodes_from_values() {
        let d = adf(&render(DEFAULT_BODY, &ctx()));
        let s = d.to_string();
        assert_eq!(d["type"], "doc");
        assert!(s.contains("\"codeBlock\""));
        assert!(s.contains("\"bulletList\""));
        let links = s.matches("\"link\"").count();
        assert_eq!(links, 2, "only FastProof pages are links: {s}");
        assert!(!s.contains("\"text\":\"\""), "no empty text nodes");
    }

    #[test]
    fn html_escapes_everything() {
        let h = html(&render(DEFAULT_BODY, &ctx()));
        assert!(h.contains("b &lt;img src=x&gt;"));
        assert!(!h.contains("<img"));
        assert_eq!(h.matches("<a href=").count(), 2);
        assert!(h.contains("<pre>expected 200"));
    }

    #[test]
    fn single_result_has_no_test_list() {
        let mut c = ctx();
        c.tests = vec!["a".into()];
        c.count = 1;
        assert!(
            !render(DEFAULT_BODY, &c)
                .iter()
                .any(|b| matches!(b, Block::List(_)))
        );
        assert!(plain(&render(DEFAULT_BODY, &c)).contains("Status: failed"));
    }
}
