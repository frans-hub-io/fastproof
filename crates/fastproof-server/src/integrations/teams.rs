//! Microsoft Teams through Workflows (Power Automate, trigger "When a Teams webhook request is
//! received"), which replaced the retired Office 365 connectors and their incoming webhooks.
//! The trigger takes a message with Adaptive Card attachments:
//! `{"type":"message","attachments":[{"contentType":"application/vnd.microsoft.card.adaptive",
//! "contentUrl":null,"content":<card>}]}` and answers `202 Accepted`.
//!
//! Text that comes from launches (project, launch and test names, branches, environments, gate
//! names) is put only into `TextRun` elements, which Adaptive Cards render as plain text, and is
//! neutralized on top of that (control characters, link syntax and `{{…}}` date functions), so it
//! cannot add links, mentions or formatting. Lists are cut so that the payload stays within
//! [`MAX_PAYLOAD_BYTES`], below the 28 KB limit of a Teams message.

use serde_json::{Value, json};

use super::gates::GateStatus;
use super::notify::Summary;

/// The Adaptive Card schema version of the card (Teams renders 1.6 and earlier).
pub const CARD_VERSION: &str = "1.5";
/// Largest payload sent; Teams refuses messages over 28 KB, envelope included.
pub const MAX_PAYLOAD_BYTES: usize = 24 * 1024;
/// New failures listed at most.
pub const TOP_FAILURES: usize = 10;
/// Rules that did not hold listed at most.
pub const TOP_RULES: usize = 5;
/// Largest text field, in characters, before the payload has to shrink.
const TEXT_MAX: usize = 300;

const ZWSP: char = '\u{200B}';

/// Plain text for a `TextRun`: control characters become spaces, link syntax (`](`) and
/// Adaptive Card date functions (`{{`) are broken with a zero-width space, and the text is cut
/// to `max` characters.
pub fn plain(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max * 4));
    let mut prev = '\0';
    for (n, c) in s.chars().enumerate() {
        if n >= max {
            out.push('…');
            break;
        }
        let c = if c.is_control() { ' ' } else { c };
        if (prev == ']' && c == '(') || (prev == '{' && c == '{') || (prev == '$' && c == '{') {
            out.push(ZWSP);
        }
        out.push(c);
        prev = c;
    }
    out
}

/// Escapes text for the Markdown of a `TextBlock` or a `Fact` (backslash before every
/// character Markdown or Teams treat specially), after [`plain`]. The card itself keeps launch
/// text out of Markdown fields; this is for callers that need to put text there.
pub fn escape_markdown(s: &str, max: usize) -> String {
    let p = plain(s, max);
    let mut out = String::with_capacity(p.len() + 8);
    for c in p.chars() {
        if matches!(
            c,
            '\\' | '*'
                | '_'
                | '['
                | ']'
                | '('
                | ')'
                | '#'
                | '`'
                | '~'
                | '>'
                | '<'
                | '!'
                | '|'
                | '-'
                | '+'
                | '.'
                | '{'
                | '}'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn run(text: String) -> Value {
    json!({"type": "TextRun", "text": text})
}

fn rich(runs: Vec<Value>) -> Value {
    json!({"type": "RichTextBlock", "inlines": runs})
}

fn fact(label: &str, value: String) -> Value {
    let mut l = run(format!("{label}: "));
    l["weight"] = json!("Bolder");
    let mut b = rich(vec![l, run(value)]);
    b["spacing"] = json!("Small");
    b
}

fn heading(text: &str) -> Value {
    json!({"type": "TextBlock", "text": text, "weight": "Bolder", "wrap": true, "spacing": "Medium"})
}

fn number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let t = format!("{v:.2}");
        t.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// `1h 2m 3s`, `45s`, `0.4s`.
pub fn duration(ms: i64) -> String {
    if ms < 1000 {
        return format!("{}s", number(ms.max(0) as f64 / 1000.0));
    }
    let s = ms / 1000;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    match (h, m) {
        (0, 0) => format!("{sec}s"),
        (0, _) => format!("{m}m {sec}s"),
        _ => format!("{h}h {m}m {sec}s"),
    }
}

fn op_text(op: super::settings::Op) -> &'static str {
    use super::settings::Op;
    match op {
        Op::Gt => ">",
        Op::Gte => ">=",
        Op::Lt => "<",
        Op::Lte => "<=",
    }
}

/// The outcome line and its colour (`Good`, `Attention`, `Warning`, `Default`).
fn outcome(s: &Summary) -> (String, &'static str) {
    let c = &s.launch.counts;
    let failing = c.failed + c.broken > 0;
    match s.gate.status {
        GateStatus::Failed => ("Quality gate failed".into(), "Attention"),
        GateStatus::Pending => ("Quality gate pending".into(), "Default"),
        GateStatus::Passed if !s.gate.results.is_empty() => ("Quality gate passed".into(), "Good"),
        GateStatus::Passed if s.launch.status == "interrupted" => ("Interrupted".into(), "Warning"),
        GateStatus::Passed if failing => ("Finished with failures".into(), "Attention"),
        GateStatus::Passed => ("Passed".into(), "Good"),
    }
}

fn container_style(color: &str) -> &'static str {
    match color {
        "Attention" => "attention",
        "Good" => "good",
        "Warning" => "warning",
        _ => "emphasis",
    }
}

fn gate_text(s: &Summary) -> String {
    let total = s.gate.results.len();
    let broken = s.gate.results.iter().filter(|r| !r.ok).count();
    match s.gate.status {
        GateStatus::Pending => "pending".into(),
        GateStatus::Failed => format!("failed, {broken} of {total} rules not met"),
        GateStatus::Passed if total == 0 => "no gate applies".into(),
        GateStatus::Passed => format!("passed, {total} rules met"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Budget {
    failures: usize,
    rules: usize,
    text: usize,
}

fn failures_url(launch_url: &str) -> String {
    let sep = if launch_url.contains('?') { '&' } else { '?' };
    format!("{launch_url}{sep}tab=failures")
}

fn card(s: &Summary, b: Budget) -> Value {
    let t = b.text;
    let c = &s.launch.counts;
    let (line, color) = outcome(s);
    let line = if s.test {
        format!("Test notification · {line}")
    } else {
        line
    };
    let mut title = run(plain(
        &format!("{} / {} #{}", s.project, s.launch.name, s.launch.number),
        t,
    ));
    title["size"] = json!("Medium");
    title["weight"] = json!("Bolder");
    if let Some(u) = &s.launch.url {
        title["selectAction"] = json!({"type": "Action.OpenUrl", "url": u});
    }
    let mut status = run(line);
    status["weight"] = json!("Bolder");
    status["color"] = json!(color);
    let mut body = vec![json!({
        "type": "Container",
        "style": container_style(color),
        "bleed": true,
        "items": [rich(vec![title]), rich(vec![status])],
    })];
    if let Some(br) = &s.launch.branch {
        body.push(fact("Branch", plain(br, t)));
    }
    if let Some(e) = &s.launch.environment {
        body.push(fact("Environment", plain(e, t)));
    }
    body.push(fact("Status", plain(&s.launch.status, 40)));
    body.push(fact(
        "Results",
        format!(
            "{} total · {} passed · {} failed · {} broken · {} skipped · {} flaky",
            c.total, c.passed, c.failed, c.broken, c.skipped, c.flaky
        ),
    ));
    if let Some(p) = s.pass_rate {
        body.push(fact(
            "Pass rate",
            format!("{}%", number((p * 10.0).round() / 10.0)),
        ));
    }
    if let Some(d) = s.launch.duration_ms {
        body.push(fact("Duration", duration(d)));
    }
    body.push(fact("Quality gate", gate_text(s)));

    let not_met: Vec<_> = s.gate.results.iter().filter(|r| !r.ok).collect();
    if !not_met.is_empty() && b.rules > 0 {
        body.push(heading("Rules not met"));
        for r in not_met.iter().take(b.rules) {
            let metric = serde_json::to_value(r.rule.metric)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let actual = r.actual.map(number).unwrap_or_else(|| "n/a".into());
            let mut line = rich(vec![run(plain(
                &format!(
                    "{}: {metric} {} {} (actual {actual})",
                    r.gate,
                    op_text(r.rule.op),
                    number(r.expected)
                ),
                t,
            ))]);
            line["spacing"] = json!("Small");
            body.push(line);
        }
        if not_met.len() > b.rules {
            body.push(more(not_met.len() - b.rules, "rules"));
        }
    }

    let nf = &s.new_failures;
    if nf.count > 0 {
        body.push(heading(&format!("New failures ({})", nf.count)));
        let mut shown = 0usize;
        if nf.items.is_empty() {
            for name in nf.names.iter().take(b.failures) {
                let mut line = rich(vec![run(plain(name, t))]);
                line["spacing"] = json!("Small");
                body.push(line);
                shown += 1;
            }
        } else {
            for f in nf.items.iter().take(b.failures) {
                let mut r = run(plain(&f.name, t));
                if let Some(u) = &f.url {
                    r["selectAction"] = json!({"type": "Action.OpenUrl", "url": u});
                }
                let mut line = rich(vec![r]);
                line["spacing"] = json!("Small");
                body.push(line);
                shown += 1;
            }
        }
        let rest = (nf.count.max(0) as usize).saturating_sub(shown);
        if rest > 0 {
            body.push(more(rest, "new failures"));
        }
    }

    let mut actions = Vec::new();
    if let Some(u) = &s.launch.url {
        actions.push(json!({"type": "Action.OpenUrl", "title": "Open launch", "url": u}));
        if c.failed + c.broken > 0 || nf.count > 0 {
            actions.push(
                json!({"type": "Action.OpenUrl", "title": "Open failures", "url": failures_url(u)}),
            );
        }
    }
    let mut card = json!({
        "type": "AdaptiveCard",
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "version": CARD_VERSION,
        "fallbackText": plain(&super::notify::headline(s), t),
        "msteams": {"width": "Full"},
        "body": body,
    });
    if !actions.is_empty() {
        card["actions"] = Value::Array(actions);
    }
    card
}

fn more(n: usize, what: &str) -> Value {
    json!({"type": "TextBlock", "text": format!("and {n} more {what} in FastProof"), "isSubtle": true,
        "wrap": true, "spacing": "Small"})
}

/// The message for the Workflows trigger.
pub fn message(card: Value) -> Value {
    json!({
        "type": "message",
        "attachments": [{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "contentUrl": null,
            "content": card,
        }],
    })
}

/// The request body: the message with the largest card that fits [`MAX_PAYLOAD_BYTES`]
/// (fewer failures, then fewer rules, then shorter texts).
pub fn body(s: &Summary) -> Vec<u8> {
    let mut b = Budget {
        failures: TOP_FAILURES,
        rules: TOP_RULES,
        text: TEXT_MAX,
    };
    loop {
        let out = serde_json::to_vec(&message(card(s, b))).unwrap_or_default();
        if out.len() <= MAX_PAYLOAD_BYTES {
            return out;
        }
        if b.failures > 0 {
            b.failures /= 2;
        } else if b.rules > 0 {
            b.rules /= 2;
        } else if b.text > 20 {
            b.text /= 2;
        } else {
            return out;
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use fastproof_store::Counts;
    use uuid::Uuid;

    use super::super::gates::{GateReport, RuleResult};
    use super::super::notify::{NewFailure, NewFailures, SummaryLaunch};
    use super::super::settings::{GateRule, Metric, Op};
    use super::*;

    fn rule(gate: &str, metric: Metric, op: Op, value: f64, actual: f64, ok: bool) -> RuleResult {
        RuleResult {
            gate: gate.into(),
            rule: GateRule {
                metric,
                op,
                value,
                baseline: None,
            },
            actual: Some(actual),
            expected: value,
            ok,
            baseline_launch_id: None,
            note: None,
        }
    }

    pub fn summary() -> Summary {
        let launch_url =
            "https://fastproof.example.com/p/shop/launches/0192f0c4-0000-7000-8000-000000000007";
        Summary {
            project: "shop".into(),
            launch: SummaryLaunch {
                id: Uuid::nil(),
                number: 7,
                name: "nightly".into(),
                status: "finished".into(),
                branch: Some("main".into()),
                environment: Some("staging-chromium".into()),
                repository: None,
                commit: None,
                merge_request: None,
                ci_url: None,
                url: Some(launch_url.into()),
                duration_ms: Some(754_000),
                counts: Counts {
                    total: 120,
                    passed: 110,
                    failed: 3,
                    broken: 1,
                    skipped: 4,
                    flaky: 2,
                    ..Default::default()
                },
            },
            gate: GateReport {
                status: GateStatus::Failed,
                results: vec![
                    rule("default", Metric::Failed, Op::Lte, 0.0, 3.0, false),
                    rule("default", Metric::PassRate, Op::Gte, 95.0, 94.83, false),
                    rule("default", Metric::Flaky, Op::Lte, 5.0, 2.0, true),
                ],
                launch_id: Uuid::nil(),
                evaluated_at: Utc::now(),
            },
            pass_rate: Some(110.0 * 100.0 / 116.0),
            new_failures: NewFailures {
                count: 2,
                names: vec!["checkout.pays".into(), "cart.totals".into()],
                items: vec![
                    NewFailure {
                        name: "checkout.pays".into(),
                        result_id: Uuid::nil(),
                        url: Some("https://fastproof.example.com/p/shop/results/0192f0c4-0000-7000-8000-0000000000a1".into()),
                    },
                    NewFailure {
                        name: "cart.totals".into(),
                        result_id: Uuid::nil(),
                        url: Some("https://fastproof.example.com/p/shop/results/0192f0c4-0000-7000-8000-0000000000a2".into()),
                    },
                ],
                baseline_launch_id: None,
            },
            events: vec!["gateFailed".into()],
            test: false,
        }
    }

    const ELEMENTS: [&str; 5] = [
        "TextBlock",
        "RichTextBlock",
        "Container",
        "FactSet",
        "ColumnSet",
    ];

    /// The checks a Teams client makes before rendering: the envelope, the card's required
    /// properties, known element types, required text and URLs, and only `Action.OpenUrl`.
    pub fn check_card_shape(msg: &Value) {
        assert_eq!(msg["type"], "message");
        let atts = msg["attachments"].as_array().expect("attachments");
        assert_eq!(atts.len(), 1, "one card per message");
        assert_eq!(
            atts[0]["contentType"],
            "application/vnd.microsoft.card.adaptive"
        );
        assert!(atts[0]["contentUrl"].is_null());
        let card = &atts[0]["content"];
        assert_eq!(card["type"], "AdaptiveCard");
        assert_eq!(card["version"], CARD_VERSION);
        assert_eq!(
            card["$schema"],
            "http://adaptivecards.io/schemas/adaptive-card.json"
        );
        let body = card["body"].as_array().expect("body");
        assert!(!body.is_empty());
        fn element(e: &Value) {
            let t = e["type"].as_str().expect("element type");
            assert!(ELEMENTS.contains(&t), "unknown element {t}");
            match t {
                "TextBlock" => assert!(e["text"].as_str().is_some_and(|s| !s.is_empty())),
                "RichTextBlock" => {
                    let inl = e["inlines"].as_array().expect("inlines");
                    assert!(!inl.is_empty());
                    for r in inl {
                        assert_eq!(r["type"], "TextRun");
                        assert!(r["text"].is_string());
                        if let Some(a) = r.get("selectAction") {
                            action(a, false);
                        }
                    }
                }
                "Container" => e["items"]
                    .as_array()
                    .expect("items")
                    .iter()
                    .for_each(element),
                _ => {}
            }
        }
        fn action(a: &Value, titled: bool) {
            assert_eq!(a["type"], "Action.OpenUrl", "{a}");
            let u = a["url"].as_str().expect("url");
            assert!(u.starts_with("https://") || u.starts_with("http://"), "{u}");
            if titled {
                assert!(a["title"].as_str().is_some_and(|s| !s.is_empty()));
            }
        }
        body.iter().for_each(element);
        for a in card["actions"].as_array().into_iter().flatten() {
            action(a, true);
        }
    }

    #[test]
    fn payload_snapshot_and_shape() {
        let s = summary();
        let raw = body(&s);
        let v: Value = serde_json::from_slice(&raw).unwrap();
        check_card_shape(&v);
        let pretty = serde_json::to_string_pretty(&v).unwrap() + "\n";
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/integrations/testdata/teams_card.json"
        );
        if std::env::var_os("FASTPROOF_UPDATE_SNAPSHOTS").is_some() {
            std::fs::write(path, &pretty).unwrap();
        }
        let expected =
            std::fs::read_to_string(path).expect("run with FASTPROOF_UPDATE_SNAPSHOTS=1");
        assert_eq!(
            pretty, expected,
            "the Teams card changed; review and update the snapshot"
        );
        let card = &v["attachments"][0]["content"];
        let titles: Vec<&str> = card["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["title"].as_str().unwrap())
            .collect();
        assert_eq!(titles, ["Open launch", "Open failures"]);
        assert!(
            card["actions"][1]["url"]
                .as_str()
                .unwrap()
                .ends_with("?tab=failures")
        );
        let text = card.to_string();
        for want in [
            "shop / nightly #7",
            "Quality gate failed",
            "staging-chromium",
            "120 total · 110 passed · 3 failed · 1 broken · 4 skipped · 2 flaky",
            "94.8%",
            "12m 34s",
            "failed, 2 of 3 rules not met",
            "passRate >= 95 (actual 94.83)",
            "New failures (2)",
            "results/0192f0c4-0000-7000-8000-0000000000a1",
        ] {
            assert!(text.contains(want), "{want} missing in {text}");
        }
    }

    #[test]
    fn launch_text_is_plain() {
        let mut s = summary();
        s.launch.name =
            "pwn](https://evil.example.net) **x** <at>all</at>\n{{DATE(2026-01-01T00:00:00Z)}}"
                .into();
        s.new_failures.items[0].name = "[click](https://evil.example.net)".into();
        let v: Value = serde_json::from_slice(&body(&s)).unwrap();
        check_card_shape(&v);
        let text = v.to_string();
        assert!(!text.contains("](https://evil"), "{text}");
        assert!(!text.contains("{{DATE"), "{text}");
        assert!(
            !text.contains("\\n{"),
            "control characters are gone: {text}"
        );
        let card = &v["attachments"][0]["content"];
        let markdown_fields: Vec<&str> = card["body"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["type"] == "TextBlock")
            .map(|e| e["text"].as_str().unwrap())
            .collect();
        assert!(
            markdown_fields
                .iter()
                .all(|t| !t.contains("evil") && !t.contains("pwn")),
            "launch text never reaches a Markdown field: {markdown_fields:?}"
        );
        assert_eq!(plain("a](b", 10), "a]\u{200B}(b");
        assert_eq!(plain("x{{y", 10), "x{\u{200B}{y");
        assert_eq!(plain("abcdef", 3), "abc…");
        assert_eq!(plain("a\tb\u{0007}c", 10), "a b c");
        assert_eq!(
            escape_markdown("*a* [b](c)", 50),
            "\\*a\\* \\[b\\]\u{200B}\\(c\\)"
        );
    }

    #[test]
    fn long_lists_and_texts_are_cut_to_the_size_limit() {
        let mut s = summary();
        let long = "x".repeat(5000);
        s.new_failures.count = 5000;
        s.new_failures.items = (0..2000)
            .map(|i| NewFailure {
                name: format!("{i} {long}"),
                result_id: Uuid::nil(),
                url: Some(format!("https://fastproof.example.com/p/shop/results/{i}")),
            })
            .collect();
        s.gate.results = (0..200)
            .map(|i| {
                rule(
                    &format!("g{i}{long}"),
                    Metric::Failed,
                    Op::Lte,
                    0.0,
                    1.0,
                    false,
                )
            })
            .collect();
        s.launch.name = long.clone();
        s.launch.branch = Some(long.clone());
        s.launch.environment = Some(long.clone());
        let raw = body(&s);
        assert!(raw.len() <= MAX_PAYLOAD_BYTES, "{}", raw.len());
        let v: Value = serde_json::from_slice(&raw).unwrap();
        check_card_shape(&v);
        let text = v.to_string();
        assert!(text.contains("New failures (5000)"));
        assert!(text.contains("more new failures in FastProof"), "{text}");
        assert!(text.contains("Open launch"));

        let mut small = summary();
        small.new_failures.count = 30;
        small.new_failures.items.truncate(2);
        let v: Value = serde_json::from_slice(&body(&small)).unwrap();
        assert!(v.to_string().contains("and 28 more new failures"));
    }

    #[test]
    fn outcomes_and_numbers() {
        let mut s = summary();
        s.gate.status = GateStatus::Passed;
        s.gate.results.clear();
        assert_eq!(outcome(&s).0, "Finished with failures");
        s.launch.counts.failed = 0;
        s.launch.counts.broken = 0;
        assert_eq!(outcome(&s), ("Passed".to_string(), "Good"));
        s.launch.status = "interrupted".into();
        assert_eq!(outcome(&s).1, "Warning");
        s.launch.url = None;
        s.new_failures.count = 0;
        s.test = true;
        let v: Value = serde_json::from_slice(&body(&s)).unwrap();
        check_card_shape(&v);
        let card = &v["attachments"][0]["content"];
        assert!(
            card.get("actions").is_none(),
            "no links without FASTPROOF_PUBLIC_URL"
        );
        assert!(card.to_string().contains("Test notification · Interrupted"));
        assert_eq!(duration(400), "0.4s");
        assert_eq!(duration(45_000), "45s");
        assert_eq!(duration(3_723_000), "1h 2m 3s");
        assert_eq!(number(95.0), "95");
        assert_eq!(number(94.8333), "94.83");
        assert_eq!(
            failures_url("https://x.example.com/l?a=1"),
            "https://x.example.com/l?a=1&tab=failures"
        );
    }
}
