//! A finished test item and its steps as one native [`ReportResult`] (one attempt), design §4.

use std::collections::HashMap;

use fastproof_model::{
    Label, Link, Parameter, ReportAttachment, ReportError, ReportResult, ReportSource,
    ReportStatus, ReportStep, Status,
};
use serde::{Deserialize, Serialize};

use crate::model::{ExternalIssue, ItemAttribute, ItemParameter};
use crate::status::{INTERRUPTED, item_status, worst};
use crate::{PATH_SEPARATOR, cut};

/// Bytes of an error message, at most (its first line).
pub const MAX_ERROR_MESSAGE_BYTES: usize = 4096;
/// Bytes of an error trace, at most.
pub const MAX_ERROR_TRACE_BYTES: usize = 1024 * 1024;
/// Steps of one result, at most (the rest is dropped with a warning).
pub const MAX_STEPS: usize = 10_000;

/// An attachment of an item: the native attachment id and how to show it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attached {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

/// The error text of an item from its ERROR log lines: the last one wins.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorText {
    pub message: String,
    pub trace: String,
}

impl ErrorText {
    /// The error of an ERROR line: the first non-empty line as the message (at most
    /// [`MAX_ERROR_MESSAGE_BYTES`]), the whole text as the trace (at most
    /// [`MAX_ERROR_TRACE_BYTES`]).
    pub fn of(text: &str) -> Self {
        let first = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or_default();
        Self {
            message: cut(first, MAX_ERROR_MESSAGE_BYTES),
            trace: cut(text, MAX_ERROR_TRACE_BYTES),
        }
    }
}

/// A stored item, as assembly needs it.
#[derive(Clone, Debug, Default)]
pub struct Item {
    pub uuid: String,
    pub parent_uuid: Option<String>,
    pub name: String,
    pub code_ref: Option<String>,
    pub test_case_id: Option<String>,
    pub description: Option<String>,
    pub parameters: Vec<ItemParameter>,
    pub attributes: Vec<ItemAttribute>,
    pub suite_path: Vec<String>,
    pub start_ms: i64,
    pub stop_ms: Option<i64>,
    /// The ReportPortal status of the finish request.
    pub status: Option<String>,
    pub attachments: Vec<Attached>,
    pub error: Option<ErrorText>,
    /// Order of the start among the launch's items.
    pub num_id: i64,
}

/// How to assemble.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The item never finished (the launch finished, or went idle): broken `interrupted`.
    pub interrupted: bool,
    /// A failed fixture: broken, named `<container>: <fixture>`, label `kind: fixture`.
    pub fixture_of: Option<String>,
    /// Label `rp.issue` with this defect group (the finishing caller may not triage).
    pub issue_label: Option<String>,
    /// External issues of the finish request, as issue links.
    pub external_issues: Vec<ExternalIssue>,
    /// The identity of the original item when a retry carries none of its own:
    /// `(codeRef, testCaseId, suite path)`.
    pub retry_of: Option<(Option<String>, Option<String>, Vec<String>)>,
}

/// The full name of an item: its code reference, else the container path and its name.
pub fn full_name(code_ref: Option<&str>, suite_path: &[String], name: &str) -> String {
    match code_ref.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => c.to_string(),
        None => {
            let mut parts: Vec<&str> = suite_path.iter().map(String::as_str).collect();
            parts.push(name);
            parts.join(PATH_SEPARATOR)
        }
    }
}

const SOURCE_EXTENSIONS: [&str; 19] = [
    ".py", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".java", ".kt", ".cs", ".rb", ".go",
    ".robot", ".feature", ".php", ".scala", ".groovy", ".swift", ".rs",
];

/// The source file of a code reference such as `tests/test_a.py:TestX.test_y` or
/// `e2e/cart.spec.ts/Cart/adds`: a relative path without `..`, ending in a source extension.
pub fn source_file(code_ref: &str) -> Option<String> {
    let c = code_ref.trim();
    let mut best: Option<usize> = None;
    for ext in SOURCE_EXTENSIONS {
        let mut from = 0;
        while let Some(i) = c[from..].find(ext) {
            let end = from + i + ext.len();
            if matches!(
                c[end..].chars().next(),
                None | Some('/' | ':' | '#' | '[' | ' ')
            ) {
                best = Some(best.map_or(end, |b| b.min(end)));
                break;
            }
            from = end;
        }
    }
    let file = &c[..best?];
    if file.starts_with('/')
        || file.starts_with('\\')
        || file.contains("\\")
        || file.contains(':')
        || file.split('/').any(|s| s == ".." || s == ".")
        || file.chars().any(char::is_control)
        || file.len() > 1024
    {
        return None;
    }
    Some(file.to_string())
}

fn report_status(s: Status) -> ReportStatus {
    ReportStatus::from(s)
}

fn parameters(p: &[ItemParameter]) -> Vec<Parameter> {
    p.iter()
        .map(|x| {
            let name = x.key.clone().unwrap_or_else(|| "param".into());
            let mut p = Parameter::new(name, x.value.clone().unwrap_or_default());
            if x.value.is_none() {
                p.value = None;
            }
            p
        })
        .collect()
}

fn attachments(a: &[Attached]) -> Vec<ReportAttachment> {
    a.iter()
        .map(|x| ReportAttachment {
            id: x.id.clone(),
            name: x.name.clone(),
            content_type: x.content_type.clone(),
        })
        .collect()
}

/// Labels of the suite path: `parentSuite`, `suite`, `subSuite` (the rest joined).
fn suite_labels(path: &[String]) -> Vec<Label> {
    match path {
        [] => vec![],
        [a] => vec![Label::new("suite", a.clone())],
        [a, b] => vec![
            Label::new("parentSuite", a.clone()),
            Label::new("suite", b.clone()),
        ],
        [a, b, rest @ ..] => vec![
            Label::new("parentSuite", a.clone()),
            Label::new("suite", b.clone()),
            Label::new("subSuite", rest.join(PATH_SEPARATOR)),
        ],
    }
}

/// Labels of item attributes: `key:value` is a label, a value alone a `tag`; system
/// attributes are dropped.
pub fn attribute_labels(a: &[ItemAttribute]) -> Vec<Label> {
    a.iter()
        .filter(|x| !x.system)
        .filter_map(|x| {
            let v = x.value.clone()?;
            Some(match &x.key {
                Some(k) => Label::new(k.clone(), v),
                None => Label::new("tag", v),
            })
        })
        .collect()
}

fn status_of(item: &Item, interrupted: bool, steps: &[Status]) -> (Status, bool) {
    if interrupted && item.status.is_none() {
        return (Status::Broken, true);
    }
    match item_status(item.status.as_deref()) {
        Some(s) => s,
        None => (
            worst(steps.iter().copied()).unwrap_or(Status::Passed),
            false,
        ),
    }
}

struct StepTree<'a> {
    children: HashMap<&'a str, Vec<&'a Item>>,
    interrupted: bool,
    count: usize,
    dropped: usize,
}

impl<'a> StepTree<'a> {
    fn build(&mut self, parent: &str, depth: usize) -> (Vec<ReportStep>, Vec<Status>) {
        let mut out = Vec::new();
        let mut statuses = Vec::new();
        let Some(kids) = self.children.get(parent).cloned() else {
            return (out, statuses);
        };
        for s in kids {
            if self.count >= MAX_STEPS || depth > crate::MAX_DEPTH as usize {
                self.dropped += 1;
                continue;
            }
            self.count += 1;
            let (steps, inner) = self.build(&s.uuid, depth + 1);
            let unfinished = s.stop_ms.is_none() && s.status.is_none();
            let (status, interrupted) = if unfinished && !self.interrupted {
                (
                    worst(inner.iter().copied()).unwrap_or(Status::Unknown),
                    false,
                )
            } else {
                status_of(s, self.interrupted && unfinished, &inner)
            };
            statuses.push(status);
            let error = match (&s.error, interrupted) {
                (Some(e), _) => Some(ReportError {
                    message: Some(e.message.clone()),
                    trace: Some(e.trace.clone()),
                    ..Default::default()
                }),
                (None, true) => Some(ReportError {
                    message: Some(INTERRUPTED.into()),
                    ..Default::default()
                }),
                _ => None,
            };
            let stop = s.stop_ms.map(|t| t.max(s.start_ms));
            out.push(ReportStep {
                name: if s.name.is_empty() {
                    "step".into()
                } else {
                    s.name.clone()
                },
                status,
                error,
                start: Some(s.start_ms),
                stop,
                duration_ms: stop.map(|t| t - s.start_ms),
                parameters: parameters(&s.parameters),
                attachments: attachments(&s.attachments),
                steps,
            });
        }
        (out, statuses)
    }
}

/// The result of `item` with its `steps` (every item under it that is a step of it, any
/// depth, in start order) under client id `rp:<uuid>`. Returns the result and warnings.
pub fn assemble(item: &Item, steps: &[Item], o: &Options) -> (ReportResult, Vec<String>) {
    let mut warnings = Vec::new();
    let mut sorted: Vec<&Item> = steps.iter().collect();
    sorted.sort_by_key(|s| (s.start_ms, s.num_id));
    let mut children: HashMap<&str, Vec<&Item>> = HashMap::new();
    for s in &sorted {
        if let Some(p) = s.parent_uuid.as_deref() {
            children.entry(p).or_default().push(s);
        }
    }
    let mut tree = StepTree {
        children,
        interrupted: o.interrupted,
        count: 0,
        dropped: 0,
    };
    let (report_steps, step_statuses) = tree.build(&item.uuid, 1);
    if tree.dropped > 0 {
        warnings.push(format!(
            "{} steps dropped: at most {MAX_STEPS} steps per result",
            tree.dropped
        ));
    }
    let (mut status, interrupted) = status_of(item, o.interrupted, &step_statuses);
    if o.fixture_of.is_some() {
        status = Status::Broken;
    }
    let (code_ref, test_case_id, suite_path) = match &o.retry_of {
        Some((c, t, p)) if item.code_ref.is_none() && item.test_case_id.is_none() => {
            (c.clone(), t.clone(), p.clone())
        }
        _ => (
            item.code_ref.clone(),
            item.test_case_id.clone(),
            item.suite_path.clone(),
        ),
    };
    let name = match &o.fixture_of {
        Some(c) if !c.is_empty() => format!("{c}: {}", item.name),
        _ => item.name.clone(),
    };
    let name = if name.trim().is_empty() {
        "unnamed".to_string()
    } else {
        name
    };
    let full = full_name(code_ref.as_deref(), &suite_path, &name);
    let mut labels = suite_labels(&item.suite_path);
    labels.extend(attribute_labels(&item.attributes));
    if o.fixture_of.is_some() {
        labels.push(Label::new("kind", "fixture"));
    }
    if let Some(g) = &o.issue_label {
        labels.push(Label::new("rp.issue", g.clone()));
    }
    let links: Vec<Link> = o
        .external_issues
        .iter()
        .filter_map(|x| {
            let url = x.url.as_deref()?.trim();
            if !(url.starts_with("https://") || url.starts_with("http://")) || url.len() > 2048 {
                return None;
            }
            Some(Link {
                name: x.ticket_id.clone().map(|t| cut(&t, 256)),
                url: url.to_string(),
                link_type: Some("issue".into()),
            })
        })
        .take(20)
        .collect();
    let error = match (&item.error, interrupted) {
        (Some(e), _) => Some(ReportError {
            message: Some(e.message.clone()),
            trace: Some(e.trace.clone()),
            ..Default::default()
        }),
        (None, true) => Some(ReportError {
            message: Some(INTERRUPTED.into()),
            ..Default::default()
        }),
        _ => None,
    };
    let stop = item.stop_ms.map(|t| t.max(item.start_ms));
    let r = ReportResult {
        id: crate::classify::result_key(&item.uuid),
        name,
        full_name: Some(full),
        test_case_id: test_case_id.filter(|t| !t.trim().is_empty()),
        status: report_status(status),
        error,
        start: Some(item.start_ms),
        stop,
        duration_ms: stop.map(|t| t - item.start_ms),
        retry: None,
        description: item.description.clone().filter(|d| !d.trim().is_empty()),
        description_html: None,
        parameters: parameters(&item.parameters),
        labels,
        links,
        steps: report_steps,
        attachments: attachments(&item.attachments),
        stdout: None,
        stderr: None,
        traceparent: None,
        source: code_ref
            .as_deref()
            .and_then(source_file)
            .map(|file| ReportSource {
                file: Some(file),
                line: None,
            }),
    };
    (r, warnings)
}

/// The `running` result of a test that just started.
pub fn running(item: &Item) -> ReportResult {
    let (mut r, _) = assemble(item, &[], &Options::default());
    r.status = ReportStatus::Running;
    r.stop = None;
    r.duration_ms = None;
    r.error = None;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(uuid: &str, parent: Option<&str>, name: &str, start: i64) -> Item {
        Item {
            uuid: uuid.into(),
            parent_uuid: parent.map(Into::into),
            name: name.into(),
            start_ms: start,
            stop_ms: Some(start + 10),
            status: Some("PASSED".into()),
            num_id: start,
            ..Default::default()
        }
    }

    #[test]
    fn result_with_steps_attributes_and_error() {
        let mut t = item("t", Some("s"), "pays", 1000);
        t.suite_path = vec!["checkout.spec.ts".into(), "Checkout".into()];
        t.code_ref = Some("e2e/checkout.spec.ts/Checkout/pays".into());
        t.status = Some("FAILED".into());
        t.stop_ms = Some(1500);
        t.attributes = vec![
            ItemAttribute {
                key: Some("owner".into()),
                value: Some("team-a".into()),
                system: false,
            },
            ItemAttribute {
                key: None,
                value: Some("smoke".into()),
                system: false,
            },
            ItemAttribute {
                key: Some("agent".into()),
                value: Some("x".into()),
                system: true,
            },
        ];
        t.parameters = vec![ItemParameter {
            key: Some("n".into()),
            value: Some("1".into()),
        }];
        t.error = Some(ErrorText::of("\n  AssertionError: 1 != 2\n  at x.ts:3"));
        let mut s1 = item("s1", Some("t"), "open", 1100);
        s1.attachments = vec![Attached {
            id: "0193a1b2-0000-7000-8000-000000000001".into(),
            name: Some("shot.png".into()),
            content_type: Some("image/png".into()),
        }];
        let mut s2 = item("s2", Some("s1"), "click", 1200);
        s2.status = Some("FAILED".into());
        let s3 = item("s3", Some("t"), "close", 1050);
        let (r, w) = assemble(&t, &[s1, s2, s3], &Options::default());
        assert!(w.is_empty());
        assert_eq!(r.id, "rp:t");
        assert_eq!(r.status, ReportStatus::Failed);
        assert_eq!(
            r.full_name.as_deref(),
            Some("e2e/checkout.spec.ts/Checkout/pays")
        );
        assert_eq!(
            r.source.as_ref().unwrap().file.as_deref(),
            Some("e2e/checkout.spec.ts")
        );
        assert_eq!(r.duration_ms, Some(500));
        let e = r.error.unwrap();
        assert_eq!(e.message.as_deref(), Some("AssertionError: 1 != 2"));
        assert!(e.trace.unwrap().contains("at x.ts:3"));
        assert_eq!(r.steps.len(), 2);
        assert_eq!(r.steps[0].name, "close");
        assert_eq!(r.steps[1].name, "open");
        assert_eq!(r.steps[1].steps[0].status, Status::Failed);
        assert_eq!(r.steps[1].attachments.len(), 1);
        let names: Vec<(&str, &str)> = r
            .labels
            .iter()
            .map(|l| (l.name.as_str(), l.value.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("parentSuite", "checkout.spec.ts"),
                ("suite", "Checkout"),
                ("owner", "team-a"),
                ("tag", "smoke")
            ]
        );
        assert_eq!(r.parameters[0].name, "n");
    }

    #[test]
    fn statuses_interruptions_and_fixtures() {
        let mut t = item("t", None, "x", 0);
        t.status = None;
        let mut s = item("s", Some("t"), "s", 1);
        s.status = Some("SKIPPED".into());
        let (r, _) = assemble(&t, &[s], &Options::default());
        assert_eq!(r.status, ReportStatus::Skipped);
        t.status = Some("STOPPED".into());
        let (r, _) = assemble(&t, &[], &Options::default());
        assert_eq!(r.status, ReportStatus::Broken);
        assert_eq!(r.error.unwrap().message.as_deref(), Some(INTERRUPTED));
        let mut open = item("o", None, "open", 0);
        open.status = None;
        open.stop_ms = None;
        let mut st = item("os", Some("o"), "step", 1);
        st.status = None;
        st.stop_ms = None;
        let (r, _) = assemble(
            &open,
            &[st],
            &Options {
                interrupted: true,
                ..Default::default()
            },
        );
        assert_eq!(r.status, ReportStatus::Broken);
        assert_eq!(r.steps[0].status, Status::Broken);
        assert_eq!(r.stop, None);
        let mut f = item("f", Some("c"), "setUp", 0);
        f.status = Some("FAILED".into());
        let (r, _) = assemble(
            &f,
            &[],
            &Options {
                fixture_of: Some("OrderTest".into()),
                issue_label: Some("automationBug".into()),
                external_issues: vec![
                    ExternalIssue {
                        ticket_id: Some("SHOP-1".into()),
                        url: Some("https://tracker.example.com/SHOP-1".into()),
                    },
                    ExternalIssue {
                        ticket_id: Some("bad".into()),
                        url: Some("javascript:alert(1)".into()),
                    },
                ],
                ..Default::default()
            },
        );
        assert_eq!(r.name, "OrderTest: setUp");
        assert_eq!(r.status, ReportStatus::Broken);
        assert!(
            r.labels
                .iter()
                .any(|l| l.name == "kind" && l.value == "fixture")
        );
        assert!(
            r.labels
                .iter()
                .any(|l| l.name == "rp.issue" && l.value == "automationBug")
        );
        assert_eq!(r.links.len(), 1);
        assert_eq!(r.links[0].link_type.as_deref(), Some("issue"));
    }

    #[test]
    fn full_names_and_retries() {
        let mut t = item("r2", None, "pays", 0);
        t.suite_path = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        let (r, _) = assemble(&t, &[], &Options::default());
        assert_eq!(
            r.full_name.as_deref(),
            Some("a \u{203a} b \u{203a} c \u{203a} d \u{203a} pays")
        );
        assert!(
            r.labels
                .iter()
                .any(|l| l.name == "subSuite" && l.value == "c \u{203a} d")
        );
        let (r, _) = assemble(
            &t,
            &[],
            &Options {
                retry_of: Some((Some("tests/a.py:pays".into()), None, vec![])),
                ..Default::default()
            },
        );
        assert_eq!(r.full_name.as_deref(), Some("tests/a.py:pays"));
        assert_eq!(running(&t).status, ReportStatus::Running);
    }

    #[test]
    fn source_files() {
        assert_eq!(
            source_file("tests/test_a.py:TestX.test_y").as_deref(),
            Some("tests/test_a.py")
        );
        assert_eq!(
            source_file("e2e/cart.spec.ts/Cart/adds").as_deref(),
            Some("e2e/cart.spec.ts")
        );
        assert_eq!(source_file("com.example.OrderTest.pays"), None);
        assert_eq!(source_file("../etc/passwd.py:x"), None);
        assert_eq!(source_file("/abs/a.py:x"), None);
        assert_eq!(source_file("C:\\a.py"), None);
        assert_eq!(
            source_file("features/a.feature:12").as_deref(),
            Some("features/a.feature")
        );
    }

    #[test]
    fn too_many_steps_are_dropped() {
        let t = item("t", None, "t", 0);
        let steps: Vec<Item> = (0..(MAX_STEPS + 5))
            .map(|i| item(&format!("s{i}"), Some("t"), "s", i as i64))
            .collect();
        let (r, w) = assemble(&t, &steps, &Options::default());
        assert_eq!(r.steps.len(), MAX_STEPS);
        assert_eq!(w.len(), 1);
    }
}
