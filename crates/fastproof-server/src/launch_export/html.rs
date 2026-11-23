//! The self-contained HTML report of a launch: one file, no scripts, no external fetches. Every
//! value that came from a test run (names, messages, labels) is escaped; URLs are shown as text,
//! never as links.

use chrono::SecondsFormat;
use fastproof_store::{Launch, ResultSummary};

pub const MAX_NAME_CHARS: usize = 1000;
pub const MAX_MESSAGE_CHARS: usize = 500;

pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c if c.is_control() && c != '\n' && c != '\t' => o.push('\u{fffd}'),
            c => o.push(c),
        }
    }
    o
}

pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('\u{2026}');
        t
    }
}

pub fn duration(ms: Option<i64>) -> String {
    match ms {
        None => String::new(),
        Some(m) if m < 1000 => format!("{m} ms"),
        Some(m) if m < 60_000 => format!("{:.1} s", m as f64 / 1000.0),
        Some(m) => format!("{} min {} s", m / 60_000, (m % 60_000) / 1000),
    }
}

const STYLE: &str = r#"
:root{color-scheme:light dark;--bg:#fff;--fg:#16191d;--muted:#5b6670;--line:#d5dbe1;--card:#f5f7f9;--passed:#1a7f4b;--failed:#c62828;--broken:#b45309;--skipped:#5b6670;--unknown:#6d4bb3}
@media (prefers-color-scheme:dark){:root{--bg:#14171a;--fg:#e6eaee;--muted:#9aa5b0;--line:#2f373f;--card:#1c2126;--passed:#4cc38a;--failed:#ff7b72;--broken:#e8a24a;--skipped:#9aa5b0;--unknown:#b79df0}}
*{box-sizing:border-box}
body{margin:0;padding:24px 16px;background:var(--bg);color:var(--fg);font:14px/1.45 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
main{max-width:1280px;margin:0 auto}
h1{font-size:22px;margin:0 0 4px}
h2{font-size:16px;margin:28px 0 8px}
.muted{color:var(--muted)}
.meta{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:8px;margin:16px 0}
.meta div{background:var(--card);border:1px solid var(--line);border-radius:6px;padding:8px 10px;overflow-wrap:anywhere}
.meta b{display:block;font-size:12px;font-weight:600;color:var(--muted)}
.counts{display:flex;flex-wrap:wrap;gap:8px;margin:12px 0}
.counts div{background:var(--card);border:1px solid var(--line);border-radius:6px;padding:6px 12px;min-width:90px}
.counts b{display:block;font-size:20px}
.bar{display:flex;height:12px;border-radius:6px;overflow:hidden;background:var(--line);margin:8px 0}
.bar span{display:block;height:100%}
.passed{color:var(--passed)}.failed{color:var(--failed)}.broken{color:var(--broken)}.skipped{color:var(--skipped)}.unknown,.running{color:var(--unknown)}
.bg-passed{background:var(--passed)}.bg-failed{background:var(--failed)}.bg-broken{background:var(--broken)}.bg-skipped{background:var(--skipped)}.bg-unknown{background:var(--unknown)}
.scroll{overflow-x:auto}
table{border-collapse:collapse;width:100%}
th,td{border-bottom:1px solid var(--line);padding:6px 8px;text-align:left;vertical-align:top;overflow-wrap:anywhere}
th{font-size:12px;color:var(--muted);position:sticky;top:0;background:var(--bg)}
td.name{overflow-wrap:anywhere;min-width:220px}
td.msg{white-space:pre-wrap;overflow-wrap:anywhere;min-width:200px;font-family:ui-monospace,Menlo,Consolas,monospace;font-size:12px}
td.st{font-weight:600;white-space:nowrap}
td.num{white-space:nowrap;text-align:right}
.tag{display:inline-block;border:1px solid var(--line);border-radius:4px;padding:0 5px;font-size:11px;margin-right:4px;color:var(--muted)}
footer{margin-top:24px;font-size:12px;color:var(--muted)}
@media print{
@page{size:A4 landscape;margin:12mm}
:root{color-scheme:light;--bg:#fff;--fg:#000;--muted:#444;--line:#bbb;--card:#fff;--passed:#1a7f4b;--failed:#c62828;--broken:#b45309;--skipped:#444;--unknown:#6d4bb3}
body{padding:0;font-size:11px}
th{position:static}
thead{display:table-header-group}
tr{break-inside:avoid}
.scroll{overflow:visible}
.bar,.bar span{print-color-adjust:exact;-webkit-print-color-adjust:exact}
}
@media (max-width:600px){body{padding:12px 8px}}
"#;

pub fn head(l: &Launch, project: &str, filter_text: &str) -> String {
    let mut o = String::with_capacity(8192);
    o.push_str("<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">");
    o.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">");
    o.push_str("<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; img-src data:\">");
    o.push_str("<meta name=\"referrer\" content=\"no-referrer\">");
    o.push_str(&format!(
        "<title>{}</title><style>{STYLE}</style></head><body><main>",
        esc(&format!("{project} #{} {}", l.number, clip(&l.name, 120)))
    ));
    o.push_str(&format!(
        "<h1>{}</h1><div class=\"muted\">{} &middot; launch #{} &middot; <span class=\"{}\">{}</span></div>",
        esc(&clip(&l.name, 300)),
        esc(project),
        l.number,
        match l.status.as_str() {
            "finished" => "passed",
            "interrupted" => "broken",
            _ => "unknown",
        },
        esc(&l.status)
    ));
    let mut meta: Vec<(&str, String)> = vec![(
        "Started",
        l.started_at.to_rfc3339_opts(SecondsFormat::Secs, true),
    )];
    if let Some(f) = l.finished_at {
        meta.push(("Finished", f.to_rfc3339_opts(SecondsFormat::Secs, true)));
    }
    if l.duration_ms.is_some() {
        meta.push(("Duration", duration(l.duration_ms)));
    }
    if let Some(e) = &l.environment {
        if let Some(k) = &e.key {
            meta.push(("Environment", k.clone()));
        }
        for (k, v) in e.values.iter().take(20) {
            meta.push(("Environment value", format!("{k}={v}")));
        }
    }
    if let Some(v) = &l.vcs {
        for (n, x) in [
            ("Repository", &v.repository),
            ("Branch", &v.branch),
            ("Commit", &v.commit),
        ] {
            if let Some(x) = x {
                meta.push((n, x.clone()));
            }
        }
    }
    if let Some(c) = &l.ci {
        for (n, x) in [
            ("CI", &c.provider),
            ("Pipeline", &c.pipeline_id),
            ("CI job", &c.job_id),
            ("CI URL", &c.url),
            ("Merge request", &c.merge_request),
        ] {
            if let Some(x) = x {
                meta.push((n, x.clone()));
            }
        }
    }
    for a in l.attributes.iter().take(50) {
        meta.push(("Attribute", format!("{}={}", a.key, a.value)));
    }
    o.push_str("<div class=\"meta\">");
    for (k, v) in meta {
        o.push_str(&format!(
            "<div><b>{}</b>{}</div>",
            esc(k),
            esc(&clip(&v, 500))
        ));
    }
    o.push_str("</div>");
    let c = &l.counts;
    o.push_str("<h2>Summary</h2><div class=\"counts\">");
    for (n, v, cls) in [
        ("Total", c.total, ""),
        ("Passed", c.passed, "passed"),
        ("Failed", c.failed, "failed"),
        ("Broken", c.broken, "broken"),
        ("Skipped", c.skipped, "skipped"),
        ("Unknown", c.unknown, "unknown"),
        ("Flaky", c.flaky, ""),
        ("Muted", c.muted, ""),
    ] {
        o.push_str(&format!("<div class=\"{cls}\"><b>{v}</b>{n}</div>"));
    }
    o.push_str("</div><div class=\"bar\" role=\"img\" aria-label=\"status distribution\">");
    if c.total > 0 {
        for (v, cls) in [
            (c.passed, "passed"),
            (c.failed, "failed"),
            (c.broken, "broken"),
            (c.skipped, "skipped"),
            (c.unknown, "unknown"),
        ] {
            if v > 0 {
                o.push_str(&format!(
                    "<span class=\"bg-{cls}\" style=\"width:{:.3}%\"></span>",
                    v as f64 * 100.0 / c.total as f64
                ));
            }
        }
    }
    o.push_str("</div>");
    if !filter_text.is_empty() {
        o.push_str(&format!(
            "<p class=\"muted\">Filtered results: {}. The counts above are those of the whole launch.</p>",
            esc(&clip(filter_text, 500))
        ));
    }
    o.push_str("<h2>Results</h2><div class=\"scroll\"><table><thead><tr><th>Status</th><th>Test</th><th>Suite</th><th>Duration</th><th>Attempts</th><th>Triage</th><th>Message</th></tr></thead><tbody>");
    o
}

pub fn row(r: &ResultSummary) -> String {
    let status = match r.status.as_str() {
        "passed" | "failed" | "broken" | "skipped" | "unknown" | "running" => r.status.as_str(),
        _ => "unknown",
    };
    let mut tags = String::new();
    if r.flaky_in_run || r.flaky {
        tags.push_str("<span class=\"tag\">flaky</span>");
    }
    if r.muted {
        tags.push_str("<span class=\"tag\">muted</span>");
    }
    if let Some(t) = &r.transition {
        tags.push_str(&format!("<span class=\"tag\">{}</span>", esc(&clip(t, 30))));
    }
    if let Some(d) = &r.defect {
        tags.push_str(&format!(
            "<span class=\"tag\">{}</span>",
            esc(&clip(&d.name, 60))
        ));
    }
    format!(
        "<tr><td class=\"st {status}\">{status}</td><td class=\"name\">{}</td><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td>{tags}</td><td class=\"msg\">{}</td></tr>\n",
        esc(&clip(&r.full_name, MAX_NAME_CHARS)),
        esc(&clip(&r.suite_path.join(" / "), MAX_NAME_CHARS)),
        esc(&duration(r.duration_ms)),
        r.attempt_count,
        esc(&clip(r.message.as_deref().unwrap_or(""), MAX_MESSAGE_CHARS)),
    )
}

pub fn tail(written: i64, version: &str, generated: &str) -> String {
    format!(
        "</tbody></table></div><footer>{written} results &middot; generated by FastProof {} at {}</footer></main></body></html>\n",
        esc(version),
        esc(generated)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup_and_controls() {
        assert_eq!(
            esc("<img src=x onerror=alert(1)>\"'&\u{0}\u{7}"),
            "&lt;img src=x onerror=alert(1)&gt;&quot;&#39;&amp;\u{fffd}\u{fffd}"
        );
    }

    #[test]
    fn clips_on_characters() {
        assert_eq!(clip("abcdef", 3), "abc\u{2026}");
        assert_eq!(clip("\u{e9}\u{e9}", 2), "\u{e9}\u{e9}");
    }
}
