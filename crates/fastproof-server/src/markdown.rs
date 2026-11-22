//! Markdown of test cases rendered on the server: CommonMark with tables and strikethrough
//! (`pulldown-cmark`), raw HTML shown as text, links only `http`, `https` and `mailto`, images
//! only `attachment:<uuid>` of the project (served by the attachment content route), then
//! `ammonia` with an explicit allowlist (no `style`, `class` or data URIs). The same rendering
//! serves the UI, MCP and exports.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use fastproof_store::tm::{CaseBody, CaseHtml, CaseStep, CaseStepHtml};
use franshub_http::ApiError;
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd};
use uuid::Uuid;

/// Markdown rendered by [`render`] at most; longer input is cut (callers check the field
/// limit with [`check`] first).
pub const RENDER_CAP: usize = 1024 * 1024;

const TAGS: &[&str] = &[
    "p",
    "br",
    "hr",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "pre",
    "code",
    "em",
    "strong",
    "del",
    "ul",
    "ol",
    "li",
    "a",
    "img",
    "table",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
];

/// 400 `invalid_markdown` when a field is over `max` bytes.
pub fn check(field: &str, md: &str, max: usize) -> Result<(), ApiError> {
    if md.len() > max {
        return Err(ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_markdown",
            format!("{field} must have at most {max} bytes of Markdown"),
        )
        .with_details(serde_json::json!({"path": field, "max": max})));
    }
    Ok(())
}

/// [`check`] then [`render`].
pub fn render_checked(
    field: &str,
    md: &str,
    project_key: &str,
    max: usize,
) -> Result<String, ApiError> {
    check(field, md, max)?;
    Ok(render(md, project_key))
}

fn attachment_path(project_key: &str, id: Uuid) -> String {
    format!("/api/v1/projects/{project_key}/attachments/{id}/content")
}

fn is_attachment_path(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("/api/v1/projects/") else {
        return false;
    };
    let mut parts = rest.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(key), Some("attachments"), Some(id), Some("content"), None)
            if fastproof_blob::valid_project_key(key) && Uuid::try_parse(id).is_ok()
    )
}

fn keep_attachment_path(u: &str) -> Option<Cow<'_, str>> {
    is_attachment_path(u).then_some(Cow::Borrowed(u))
}

fn sanitizer() -> &'static ammonia::Builder<'static> {
    static B: OnceLock<ammonia::Builder<'static>> = OnceLock::new();
    B.get_or_init(|| {
        let mut b = ammonia::Builder::empty();
        b.tags(TAGS.iter().copied().collect())
            .clean_content_tags(["script", "style"].into_iter().collect())
            .generic_attributes(HashSet::new())
            .tag_attributes(HashMap::from([
                ("a", HashSet::from(["href", "title"])),
                ("img", HashSet::from(["src", "alt", "title"])),
                ("ol", HashSet::from(["start"])),
            ]))
            .url_schemes(["http", "https", "mailto"].into_iter().collect())
            .link_rel(Some("noopener noreferrer nofollow"))
            .url_relative(ammonia::UrlRelative::Custom(Box::new(keep_attachment_path)))
            .attribute_filter(|element, attribute, value| match (element, attribute) {
                ("img", "src") if !is_attachment_path(value) => None,
                ("a", "href") if value.starts_with('/') => None,
                _ => Some(value.into()),
            })
            .strip_comments(true);
        b
    })
}

/// The scheme of a link destination as a browser reads it (ASCII whitespace and control
/// characters ignored, lowercase), when it is one of the allowed ones.
fn safe_link(dest: &str) -> bool {
    let cleaned: String = dest
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && !c.is_control())
        .take(16)
        .collect::<String>()
        .to_ascii_lowercase();
    cleaned.starts_with("http://")
        || cleaned.starts_with("https://")
        || cleaned.starts_with("mailto:")
}

fn attachment_id(dest: &str) -> Option<Uuid> {
    Uuid::try_parse(dest.trim().strip_prefix("attachment:")?).ok()
}

fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&#39;"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

struct PendingImage {
    attachment: Option<Uuid>,
    title: String,
    alt: String,
    depth: usize,
}

fn clip(md: &str, cap: usize) -> &str {
    if md.len() <= cap {
        return md;
    }
    let mut end = cap;
    while !md.is_char_boundary(end) {
        end -= 1;
    }
    &md[..end]
}

/// Renders Markdown to sanitized HTML. Image sources are rewritten to the attachment content
/// route of `project_key`.
pub fn render(md: &str, project_key: &str) -> String {
    if md.trim().is_empty() {
        return String::new();
    }
    let parser = Parser::new_ext(
        clip(md, RENDER_CAP),
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    );
    let key_ok = fastproof_blob::valid_project_key(project_key);
    let mut out: Vec<Event<'_>> = Vec::new();
    let mut image: Option<PendingImage> = None;
    let mut links: Vec<bool> = Vec::new();
    for ev in parser {
        if let Some(img) = image.as_mut() {
            match ev {
                Event::Start(Tag::Image { .. }) => img.depth += 1,
                Event::End(TagEnd::Image) if img.depth > 0 => img.depth -= 1,
                Event::End(TagEnd::Image) => {
                    let img = image.take().unwrap_or_else(|| unreachable!());
                    match img.attachment.filter(|_| key_ok) {
                        Some(id) => {
                            let title = if img.title.is_empty() {
                                String::new()
                            } else {
                                format!(" title=\"{}\"", escape_attr(&img.title))
                            };
                            out.push(Event::Html(CowStr::from(format!(
                                "<img src=\"{}\" alt=\"{}\"{title}>",
                                attachment_path(project_key, id),
                                escape_attr(&img.alt)
                            ))));
                        }
                        None if img.alt.is_empty() => {}
                        None => out.push(Event::Text(CowStr::from(img.alt))),
                    }
                }
                Event::Text(t)
                | Event::Code(t)
                | Event::Html(t)
                | Event::InlineHtml(t)
                | Event::InlineMath(t)
                | Event::DisplayMath(t) => img.alt.push_str(&t),
                Event::SoftBreak | Event::HardBreak => img.alt.push(' '),
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Html(t)
            | Event::InlineHtml(t)
            | Event::InlineMath(t)
            | Event::DisplayMath(t) => out.push(Event::Text(t)),
            Event::FootnoteReference(t) => out.push(Event::Text(t)),
            Event::TaskListMarker(_) => {}
            Event::Start(Tag::Image {
                dest_url, title, ..
            }) => {
                image = Some(PendingImage {
                    attachment: attachment_id(&dest_url),
                    title: title.to_string(),
                    alt: String::new(),
                    depth: 0,
                });
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                let keep = safe_link(&dest_url);
                links.push(keep);
                if keep {
                    out.push(Event::Start(Tag::Link {
                        link_type,
                        dest_url,
                        title,
                        id,
                    }));
                }
            }
            Event::End(TagEnd::Link) => {
                if links.pop() == Some(true) {
                    out.push(Event::End(TagEnd::Link));
                }
            }
            Event::Start(Tag::HtmlBlock) | Event::End(TagEnd::HtmlBlock) => {}
            other => out.push(other),
        }
    }
    let mut html = String::with_capacity(md.len() + md.len() / 2);
    pulldown_cmark::html::push_html(&mut html, out.into_iter());
    sanitizer().clean(&html).to_string()
}

fn render_steps(steps: &[CaseStep], project_key: &str) -> Vec<CaseStepHtml> {
    steps
        .iter()
        .map(|s| CaseStepHtml {
            id: s.id.clone(),
            action: render(&s.action, project_key),
            expected: render(&s.expected, project_key),
            data: render(&s.data, project_key),
            steps: render_steps(&s.steps, project_key),
        })
        .collect()
}

/// Renders the Markdown fields of a case body.
pub fn render_body(body: &CaseBody, project_key: &str) -> CaseHtml {
    CaseHtml {
        description: render(&body.description, project_key),
        preconditions: render(&body.preconditions, project_key),
        steps: render_steps(&body.steps, project_key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "shop";
    const ID: &str = "0193a1b2-0000-7000-8000-000000000001";

    fn r(md: &str) -> String {
        render(md, KEY)
    }

    /// No executable markup survives: every tag and attribute is on the allowlist, links are
    /// http(s)/mailto with `rel`, images point at the project's attachments.
    fn assert_inert(md: &str) {
        let html = r(md);
        for t in tag_soup(&html) {
            let (name, attrs) = parse_tag(&t);
            assert!(
                TAGS.contains(&name.as_str()),
                "tag {name:?} not allowed in {html} from {md:?}"
            );
            let allowed: &[&str] = match name.as_str() {
                "a" => &["href", "title", "rel"],
                "img" => &["src", "alt", "title"],
                "ol" => &["start"],
                _ => &[],
            };
            for (k, v) in &attrs {
                assert!(
                    allowed.contains(&k.as_str()),
                    "attribute {k:?} on {name} in {html} from {md:?}"
                );
                if k == "href" {
                    assert!(
                        v.starts_with("http://")
                            || v.starts_with("https://")
                            || v.starts_with("mailto:"),
                        "link {v:?} in {html} from {md:?}"
                    );
                }
                if k == "src" {
                    assert!(
                        v.starts_with(&format!("/api/v1/projects/{KEY}/attachments/")),
                        "image {v:?} in {html} from {md:?}"
                    );
                }
            }
            if name == "a" && attrs.iter().any(|(k, _)| k == "href") {
                assert!(
                    attrs
                        .iter()
                        .any(|(k, v)| k == "rel" && v == "noopener noreferrer nofollow"),
                    "{t}"
                );
            }
        }
    }

    fn tag_soup(html: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = html;
        while let Some(i) = rest.find('<') {
            let after = &rest[i..];
            let end = after.find('>').map(|e| e + 1).unwrap_or(after.len());
            out.push(after[..end].to_string());
            rest = &after[end..];
        }
        out
    }

    /// Name and attributes of a serialized tag (values are double-quoted by the serializer).
    fn parse_tag(t: &str) -> (String, Vec<(String, String)>) {
        let inner = t.trim_start_matches('<').trim_start_matches('/');
        let inner = inner.trim_end_matches('>').trim_end_matches('/');
        let name: String = inner
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        let mut attrs = Vec::new();
        let mut rest = inner[name.len()..].trim_start();
        while !rest.is_empty() {
            let k_end = rest
                .find(|c: char| c == '=' || c.is_whitespace())
                .unwrap_or(rest.len());
            let k = rest[..k_end].to_ascii_lowercase();
            rest = &rest[k_end..];
            let mut v = String::new();
            if let Some(after) = rest.strip_prefix("=\"") {
                let close = after.find('"').expect("unterminated attribute");
                v = after[..close].to_string();
                rest = &after[close + 1..];
            }
            attrs.push((k, v));
            rest = rest.trim_start();
        }
        (name, attrs)
    }

    #[test]
    fn commonmark_tables_strikethrough() {
        assert_eq!(
            r("**bold** _it_ ~~gone~~"),
            "<p><strong>bold</strong> <em>it</em> <del>gone</del></p>\n"
        );
        let t = r("| a | b |\n|:--|--:|\n| 1 | 2 |\n");
        assert!(
            t.contains("<table>") && t.contains("<th>a</th>") && t.contains("<td>2</td>"),
            "{t}"
        );
        assert!(!t.contains("style"), "{t}");
        let code = r("```rust\nfn x() {}\n```");
        assert_eq!(code, "<pre><code>fn x() {}\n</code></pre>\n");
        assert_eq!(r("1. a\n2. b"), "<ol>\n<li>a</li>\n<li>b</li>\n</ol>\n");
        assert_eq!(r("3. c"), "<ol start=\"3\">\n<li>c</li>\n</ol>\n");
        assert_eq!(r("# T"), "<h1>T</h1>\n");
        assert_eq!(r(""), "");
        assert_eq!(r("   "), "");
    }

    #[test]
    fn links_are_limited_and_marked() {
        assert_eq!(
            r("[x](https://example.com/a?b=1&c=2)"),
            "<p><a href=\"https://example.com/a?b=1&amp;c=2\" rel=\"noopener noreferrer nofollow\">x</a></p>\n"
        );
        assert_eq!(
            r("<https://example.com>"),
            "<p><a href=\"https://example.com\" rel=\"noopener noreferrer nofollow\">https://example.com</a></p>\n"
        );
        assert!(r("[m](mailto:qa@example.com)").contains("href=\"mailto:qa@example.com\""));
        assert_eq!(r("[x](/relative)"), "<p>x</p>\n");
        assert_eq!(r("[x](#frag)"), "<p>x</p>\n");
        assert_eq!(r("[x](ftp://example.com)"), "<p>x</p>\n");
        assert_eq!(r("[ref][1]\n\n[1]: javascript:alert(1)"), "<p>ref</p>\n");
        assert_eq!(
            r("[ref][1]\n\n[1]: https://example.com \"T\""),
            "<p><a href=\"https://example.com\" title=\"T\" rel=\"noopener noreferrer nofollow\">ref</a></p>\n"
        );
    }

    #[test]
    fn images_only_from_attachments() {
        assert_eq!(
            r(&format!("![shot](attachment:{ID})")),
            format!(
                "<p><img src=\"/api/v1/projects/{KEY}/attachments/{ID}/content\" alt=\"shot\"></p>\n"
            )
        );
        assert_eq!(
            r("![alt text](https://example.com/x.png)"),
            "<p>alt text</p>\n"
        );
        assert_eq!(r("![](https://example.com/x.png)"), "<p></p>\n");
        assert_eq!(r("![a](data:image/png;base64,AAAA)"), "<p>a</p>\n");
        assert_eq!(r("![a](attachment:not-a-uuid)"), "<p>a</p>\n");
        assert_eq!(
            r(&format!("![a \"q\" <b>](attachment:{ID} \"t\\\"x\")")),
            format!(
                "<p><img src=\"/api/v1/projects/{KEY}/attachments/{ID}/content\" alt=\"a &quot;q&quot; &lt;b&gt;\" title=\"t&quot;x\"></p>\n"
            )
        );
        let nested = r(&format!("[![in](attachment:{ID})](https://example.com)"));
        assert!(
            nested.starts_with("<p><a href=\"https://example.com\""),
            "{nested}"
        );
        assert!(
            nested.contains("<img src=\"/api/v1/projects/shop/attachments/"),
            "{nested}"
        );
        assert_eq!(
            r("[![in](https://example.com/i.png)](javascript:alert(1))"),
            "<p>in</p>\n"
        );
        assert_eq!(
            render(&format!("![a](attachment:{ID})"), "Bad Key"),
            "<p>a</p>\n"
        );
    }

    #[test]
    fn raw_html_is_text() {
        assert_eq!(
            r("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
        assert_eq!(
            r("a <b onclick=x>b</b>"),
            "<p>a &lt;b onclick=x&gt;b&lt;/b&gt;</p>\n"
        );
    }

    #[test]
    fn xss_corpus() {
        let corpus = [
            "<script>alert('XSS')</script>",
            "<IMG SRC=\"javascript:alert('XSS');\">",
            "<IMG SRC=javascript:alert('XSS')>",
            "<IMG SRC=JaVaScRiPt:alert('XSS')>",
            "<IMG SRC=`javascript:alert(\"RSnake says, 'XSS'\")`>",
            "<a onmouseover=\"alert(document.cookie)\">xxs link</a>",
            "<IMG \"\"\"><SCRIPT>alert(\"XSS\")</SCRIPT>\"\\>",
            "<IMG SRC=# onmouseover=\"alert('xxs')\">",
            "<IMG SRC=/ onerror=\"alert(String.fromCharCode(88,83,83))\"></img>",
            "<img src=x onerror=\"&#0000106&#0000097&#0000118&#0000097&#0000115&#0000099&#0000114&#0000105&#0000112&#0000116&#0000058&#0000097&#0000108&#0000101&#0000114&#0000116&#0000040&#0000039&#0000088&#0000083&#0000083&#0000039&#0000041\">",
            "<IMG SRC=&#106;&#97;&#118;&#97;&#115;&#99;&#114;&#105;&#112;&#116;&#58;&#97;&#108;&#101;&#114;&#116;&#40;&#39;&#88;&#83;&#83;&#39;&#41;>",
            "<IMG SRC=\"jav\tascript:alert('XSS');\">",
            "<IMG SRC=\"jav&#x09;ascript:alert('XSS');\">",
            "<IMG SRC=\" &#14;  javascript:alert('XSS');\">",
            "<SCRIPT/XSS SRC=\"http://xss.rocks/xss.js\"></SCRIPT>",
            "<BODY onload!#$%&()*~+-_.,:;?@[/|\\]^`=alert(\"XSS\")>",
            "<<SCRIPT>alert(\"XSS\");//\\<</SCRIPT>",
            "<iframe src=http://xss.rocks/scriptlet.html <",
            "<svg/onload=alert('XSS')>",
            "<svg><script>alert(1)</script></svg>",
            "<math><mtext><table><mglyph><style><img src=x onerror=alert(1)>",
            "<noscript><p title=\"</noscript><img src=x onerror=alert(1)>\">",
            "<form><math><mtext></form><form><mglyph><style></math><img src onerror=alert(1)>",
            "<STYLE>li {list-style-image: url(\"javascript:alert('XSS')\");}</STYLE><UL><LI>XSS</br>",
            "<DIV STYLE=\"background-image: url(javascript:alert('XSS'))\">",
            "<TABLE BACKGROUND=\"javascript:alert('XSS')\">",
            "<META HTTP-EQUIV=\"refresh\" CONTENT=\"0;url=data:text/html base64,PHNjcmlwdD5hbGVydCgnWFNTJyk8L3NjcmlwdD4K\">",
            "<OBJECT TYPE=\"text/x-scriptlet\" DATA=\"http://xss.rocks/scriptlet.html\"></OBJECT>",
            "<!--[if gte IE 4]><SCRIPT>alert('XSS');</SCRIPT><![endif]-->",
            "<a href=\"javascript:alert(1)\">x</a>",
            "[x](javascript:alert(1))",
            "[x](JaVaScRiPt:alert(1))",
            "[x](javascript&#58;alert(1))",
            "[x](&#106;avascript:alert(1))",
            "[x](java\tscript:alert(1))",
            "[x](<java script:alert(1)>)",
            "[x]( javascript:alert(1))",
            "[x](vbscript:msgbox(1))",
            "[x](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)",
            "[x](https://example.com\"onmouseover=\"alert(1))",
            "[x](https://example.com \"title\\\" onmouseover=alert(1)\")",
            "<javascript:alert(1)>",
            "<JAVASCRIPT:alert(1)>",
            "<data:text/html,<script>alert(1)</script>>",
            "![x](javascript:alert(1))",
            "![x\" onerror=\"alert(1)](attachment:0193a1b2-0000-7000-8000-000000000001)",
            "![x](attachment:0193a1b2-0000-7000-8000-000000000001\" onerror=\"alert(1))",
            "[![x](javascript:alert(1))](javascript:alert(2))",
            "[a](https://example.com)<img src=x onerror=alert(1)>",
            "`<img src=x onerror=alert(1)>`",
            "```\n<script>alert(1)</script>\n```",
            "| a |\n|---|\n| <img src=x onerror=alert(1)> |",
            "~~<script>alert(1)</script>~~",
            "> <iframe src=\"javascript:alert(1)\"></iframe>",
            "[ref]\n\n[ref]: javascript:alert(1)",
            "[ref]\n\n[ref]: <javascript:alert(1)> 'x'",
            "<details open ontoggle=alert(1)>",
            "<a href=\"&#x6A;avascript:alert(1)\">x</a>",
            "\u{0}<script>alert(1)</script>",
            "<style>@import 'http://xss.rocks/xss.css';</style>",
            "<base href=\"javascript:alert('XSS');//\">",
            "<x onclick=alert(1)>click",
            "<img src=\"x\" onerror=\"alert(1)\" class=\"c\" style=\"s\">",
        ];
        for md in corpus {
            assert_inert(md);
        }
    }

    #[test]
    fn mutation_xss_stays_text() {
        let html = r("<noscript><p title=\"</noscript><img src=x onerror=alert(1)>\">");
        assert!(!html.contains("<img"), "{html}");
        assert!(!html.contains("<noscript"), "{html}");
        let html = r("<svg><p><style><img src=x onerror=alert(1)></style>");
        assert!(!html.contains("<img") && !html.contains("<svg"), "{html}");
    }

    #[test]
    fn size_check_and_body() {
        assert!(check("body.description", "abc", 3).is_ok());
        let e = check("body.description", "abcd", 3).unwrap_err();
        assert_eq!(e.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(e.body().code, "invalid_markdown");
        assert!(render_checked("x", "abcd", KEY, 3).is_err());
        let big = "a".repeat(RENDER_CAP + 10);
        assert!(render(&big, KEY).len() <= RENDER_CAP + 16);
        let body = CaseBody {
            description: "**d**".into(),
            preconditions: "<b>p</b>".into(),
            steps: vec![CaseStep {
                id: "s1".into(),
                action: "click [x](javascript:alert(1))".into(),
                expected: "ok".into(),
                data: String::new(),
                attachments: vec![],
                steps: vec![CaseStep {
                    id: "s1a".into(),
                    action: format!("![s](attachment:{ID})"),
                    ..Default::default()
                }],
            }],
            links: vec![],
        };
        let h = render_body(&body, KEY);
        assert_eq!(h.description, "<p><strong>d</strong></p>\n");
        assert_eq!(h.preconditions, "<p>&lt;b&gt;p&lt;/b&gt;</p>\n");
        assert_eq!(h.steps[0].action, "<p>click x</p>\n");
        assert_eq!(h.steps[0].data, "");
        assert!(h.steps[0].steps[0].action.contains("/attachments/"));
        assert_eq!(h.steps[0].steps[0].id, "s1a");
    }
}
