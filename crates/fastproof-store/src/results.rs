//! Reading results: lists, the launch tree, and the detail with attempts, steps and fixtures.

use std::collections::{BTreeMap, HashMap, HashSet};

use base64::Engine as _;
use chrono::{DateTime, Utc};
use fastproof_model::{
    AttachmentKind, AttachmentRef, Parameter, ParameterMode, Status, Step, StepKind,
};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{Postgres, QueryBuilder, Row};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::ingest::{AttemptDetail, AttemptLogs, ContainerFixtures};
use crate::launches::like_escape;
use crate::{
    AttachmentView, AttemptView, Counts, Defect, ErrorView, HistoryStore, LabelView, Launch,
    LinkView, Page, ParameterView, ResultDetail, ResultSummary, SeriesPoint, StepView, Store,
    TreeDefect, TreeNode, TreeTest, codec, json_no_nul, no_nul, ts_ms,
};

/// Filters of the result list.
#[derive(Clone, Debug, Default)]
pub struct ResultFilter {
    pub statuses: Vec<String>,
    pub q: Option<String>,
    pub suite: Option<String>,
    pub label: Option<(String, String)>,
    pub flaky: Option<bool>,
    pub fingerprint: Option<String>,
    /// Only manual (projected from run items) or only automated results.
    pub source: Option<crate::tm::ResultSource>,
    /// The explorer filters the list shares with the tree (their `statuses`, `q`, `flaky` and
    /// `source` are not used here).
    pub extra: TreeFilter,
    pub before: Option<String>,
    pub limit: i64,
}

/// Grouping of the launch tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupBy {
    Suite,
    Feature,
    Package,
    /// The values of these labels, one level each; a result without a label sits in the
    /// [`NONE_GROUP`] group of that level (when a deeper label follows or no label is set).
    Labels(Vec<String>),
}

/// Levels of a label grouping, at most.
pub const MAX_GROUP_LEVELS: usize = 5;
/// Bytes of a label name in a grouping or a filter, at most.
pub const MAX_LABEL_NAME_BYTES: usize = 64;
/// The group name (and path segment) of results without the label of a level.
pub const NONE_GROUP: &str = "";

/// Whether `name` can be a label name in a grouping or a filter.
pub fn valid_label_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_LABEL_NAME_BYTES
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '@' | '/'))
}

/// The environment of the results to keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvFilter {
    Key(String),
    /// Results of series without an environment key.
    None,
}

/// Attachment kinds as bits of `results.attachment_kinds`.
pub const KIND_IMAGE: i32 = 1;
pub const KIND_VIDEO: i32 = 2;
pub const KIND_TRACE: i32 = 4;
pub const KIND_IMAGE_DIFF: i32 = 8;
pub const KIND_HTTP: i32 = 16;
pub const KIND_TEXT: i32 = 32;
pub const KIND_LOG: i32 = 64;
pub const KIND_JSON: i32 = 128;
pub const KIND_OTHER: i32 = 256;
/// Screenshots: images and image diffs.
pub const KIND_SCREENSHOT: i32 = KIND_IMAGE | KIND_IMAGE_DIFF;

/// The bit of one attachment kind.
pub fn kind_bit(k: AttachmentKind) -> i32 {
    match k {
        AttachmentKind::Image => KIND_IMAGE,
        AttachmentKind::Video => KIND_VIDEO,
        AttachmentKind::Trace => KIND_TRACE,
        AttachmentKind::ImageDiff => KIND_IMAGE_DIFF,
        AttachmentKind::HttpExchange => KIND_HTTP,
        AttachmentKind::Text => KIND_TEXT,
        AttachmentKind::Log => KIND_LOG,
        AttachmentKind::Json => KIND_JSON,
        AttachmentKind::Other => KIND_OTHER,
    }
}

/// The attachment filters the explorer offers, with their bits.
pub const ATTACHMENT_FILTERS: [(&str, i32); 3] = [
    ("trace", KIND_TRACE),
    ("video", KIND_VIDEO),
    ("screenshot", KIND_SCREENSHOT),
];

/// The kinds of the attachments of a result, its steps and fixtures, as bits.
pub fn attachment_mask(r: &fastproof_model::ParsedResult) -> i32 {
    fn steps(list: &[Step], m: &mut i32) {
        for s in list {
            for a in &s.attachments {
                *m |= kind_bit(a.kind);
            }
            steps(&s.steps, m);
        }
    }
    let mut m = r.attachments.iter().fold(0, |m, a| m | kind_bit(a.kind));
    steps(&r.steps, &mut m);
    steps(&r.fixtures.befores, &mut m);
    steps(&r.fixtures.afters, &mut m);
    m
}

/// The launch the "new failures" filter compares with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Baseline {
    /// `None` when there is no previous launch: every failure is new.
    pub launch: Option<(Uuid, DateTime<Utc>)>,
}

/// Filters of the launch tree (and of the result list, see [`ResultFilter::extra`]).
#[derive(Clone, Debug, Default)]
pub struct TreeFilter {
    pub statuses: Vec<String>,
    /// Substring of the test name, full name or suite path.
    pub q: Option<String>,
    /// Only results flaky in this run.
    pub flaky: Option<bool>,
    /// Only manual or only automated results.
    pub source: Option<crate::tm::ResultSource>,
    /// Substring of the error message.
    pub message: Option<String>,
    /// Only results with more than one attempt.
    pub retried: bool,
    pub muted: Option<bool>,
    /// Only failed or broken, not muted results whose series did not fail in the baseline.
    pub new_failures: Option<Baseline>,
    /// Label name and accepted values: any value of one label, every label.
    pub labels: Vec<(String, Vec<String>)>,
    pub min_duration_ms: Option<i64>,
    pub max_duration_ms: Option<i64>,
    /// Defect type ids; `none` keeps results without a defect.
    pub defects: Vec<String>,
    /// Any of these attachment bits.
    pub attachments: i32,
    pub env: Option<EnvFilter>,
}

impl TreeFilter {
    /// Whether the filter keeps every result.
    pub fn is_empty(&self) -> bool {
        self.statuses.is_empty()
            && self.q.is_none()
            && self.flaky != Some(true)
            && self.source.is_none()
            && self.message.is_none()
            && !self.retried
            && self.muted.is_none()
            && self.new_failures.is_none()
            && self.labels.is_empty()
            && self.min_duration_ms.is_none()
            && self.max_duration_ms.is_none()
            && self.defects.is_empty()
            && self.attachments == 0
            && self.env.is_none()
    }
}

/// What the launch tree holds.
#[derive(Clone, Debug)]
pub struct TreeOptions {
    pub by: GroupBy,
    /// Leaves (`tests`) instead of `resultIds`.
    pub include_tests: bool,
    pub filter: TreeFilter,
    /// A result whose group is inlined first (a deep link to it).
    pub reveal: Option<Uuid>,
}

impl TreeOptions {
    pub fn groups(by: GroupBy) -> Self {
        Self {
            by,
            include_tests: false,
            filter: TreeFilter::default(),
            reveal: None,
        }
    }
}

/// The select of the tree rows: of the labels, only those the grouping reads (they are most of
/// a row's bytes).
fn tree_select(by: &GroupBy) -> QueryBuilder<Postgres> {
    let names = by.label_names();
    let mut qb: QueryBuilder<Postgres> =
        QueryBuilder::new("select r.id, r.name, r.full_name, r.suite_path, ");
    if names.is_empty() {
        qb.push("'[]'::jsonb as labels");
    } else {
        let cond = (0..names.len())
            .map(|i| format!("@.name == $n{i}"))
            .collect::<Vec<_>>()
            .join(" || ");
        let vars: serde_json::Map<String, serde_json::Value> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (format!("n{i}"), serde_json::Value::String(no_nul(n))))
            .collect();
        qb.push("jsonb_path_query_array(r.labels, ")
            .push_bind(format!("$[*] ? ({cond})"))
            .push("::jsonpath, ")
            .push_bind(serde_json::Value::Object(vars))
            .push(") as labels");
    }
    qb.push(
        ", r.status, r.flaky_in_run, r.muted, r.duration_ms, r.attempt_count, r.flaky, \
         r.transition, r.defect from results r ",
    );
    qb
}

impl GroupBy {
    /// The labels the grouping places results by.
    fn label_names(&self) -> Vec<String> {
        let fixed: &[&str] = match self {
            GroupBy::Suite => &["parentSuite", "subSuite"],
            GroupBy::Feature => &["epic", "feature", "story"],
            GroupBy::Package => &["package"],
            GroupBy::Labels(names) => return names.clone(),
        };
        fixed.iter().map(|n| (*n).to_string()).collect()
    }
}

struct TreeRow {
    id: Uuid,
    name: String,
    full_name: String,
    suite_path: Vec<String>,
    labels: Vec<LabelView>,
    status: String,
    flaky_in_run: bool,
    muted: bool,
    duration_ms: Option<i64>,
    attempt_count: i32,
    flaky: bool,
    transition: Option<String>,
    defect: Option<TreeDefect>,
}

fn tree_row(r: &PgRow) -> Result<TreeRow, sqlx::Error> {
    let labels: serde_json::Value = r.try_get("labels")?;
    let defect: Option<serde_json::Value> = r.try_get("defect")?;
    Ok(TreeRow {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        full_name: r.try_get("full_name")?,
        suite_path: r.try_get("suite_path")?,
        labels: serde_json::from_value(labels).unwrap_or_default(),
        status: r.try_get("status")?,
        flaky_in_run: r.try_get("flaky_in_run")?,
        muted: r.try_get("muted")?,
        duration_ms: r.try_get("duration_ms")?,
        attempt_count: r.try_get("attempt_count")?,
        flaky: r.try_get("flaky")?,
        transition: r.try_get("transition")?,
        defect: defect.and_then(|d| serde_json::from_value::<TreeDefect>(d).ok()),
    })
}

impl TreeRow {
    fn leaf(self, name: String) -> TreeTest {
        TreeTest {
            id: self.id,
            name,
            status: self.status,
            duration_ms: self.duration_ms,
            attempt_count: (self.attempt_count > 1).then_some(self.attempt_count),
            flaky_in_run: self.flaky_in_run,
            flaky: self.flaky,
            muted: self.muted,
            transition: self.transition,
            defect: self.defect,
        }
    }
}

fn push_tree_filter(qb: &mut QueryBuilder<Postgres>, f: &TreeFilter) {
    if !f.statuses.is_empty() {
        qb.push(" and r.status = any(")
            .push_bind(f.statuses.clone())
            .push(")");
    }
    if let Some(q) = &f.q {
        let like = format!("%{}%", like_escape(q));
        qb.push(" and (r.name ilike ")
            .push_bind(like.clone())
            .push(" or r.full_name ilike ")
            .push_bind(like.clone())
            .push(" or array_to_string(r.suite_path, ' ') ilike ")
            .push_bind(like)
            .push(")");
    }
    if let Some(fl) = f.flaky {
        qb.push(" and r.flaky_in_run = ").push_bind(fl);
    }
    match f.source {
        Some(crate::tm::ResultSource::Manual) => {
            qb.push(" and r.run_item_id is not null");
        }
        Some(crate::tm::ResultSource::Automated) => {
            qb.push(" and r.run_item_id is null");
        }
        None => {}
    }
    push_extra_filter(qb, f);
}

/// Results without a value of the label `name` (a superset where a result has the label
/// twice, the first value empty).
fn push_label_missing(qb: &mut QueryBuilder<Postgres>, name: &str) {
    qb.push("(not r.labels @> ")
        .push_bind(json_no_nul(serde_json::json!([{ "name": name }])))
        .push(" or r.labels @> ")
        .push_bind(json_no_nul(
            serde_json::json!([{ "name": name, "value": "" }]),
        ))
        .push(")");
}

/// The filters the tree and the result list share (all but status, text, flaky and source).
fn push_extra_filter(qb: &mut QueryBuilder<Postgres>, f: &TreeFilter) {
    if let Some(m) = &f.message {
        qb.push(" and r.message ilike ")
            .push_bind(format!("%{}%", like_escape(m)));
    }
    if f.retried {
        qb.push(" and r.attempt_count > 1");
    }
    if let Some(m) = f.muted {
        qb.push(" and r.muted = ").push_bind(m);
    }
    if let Some(b) = &f.new_failures {
        qb.push(" and r.status in ('failed', 'broken') and not r.muted");
        if let Some((id, created)) = b.launch {
            qb.push(" and not exists (select 1 from results b where b.launch_id = ")
                .push_bind(id)
                .push(" and b.launch_created_at = ")
                .push_bind(created)
                .push(" and b.series_id = r.series_id and b.status in ('failed', 'broken'))");
        }
    }
    for (name, values) in &f.labels {
        qb.push(" and (");
        for (i, v) in values.iter().enumerate() {
            if i > 0 {
                qb.push(" or ");
            }
            if v.is_empty() {
                push_label_missing(qb, name);
            } else {
                qb.push("r.labels @> ").push_bind(json_no_nul(
                    serde_json::json!([{ "name": name, "value": v }]),
                ));
            }
        }
        qb.push(")");
    }
    if let Some(d) = f.min_duration_ms {
        qb.push(" and r.duration_ms >= ").push_bind(d);
    }
    if let Some(d) = f.max_duration_ms {
        qb.push(" and r.duration_ms <= ").push_bind(d);
    }
    if !f.defects.is_empty() {
        let none = f.defects.iter().any(|d| d == "none");
        let ids: Vec<String> = f
            .defects
            .iter()
            .filter(|d| *d != "none")
            .map(|d| no_nul(d))
            .collect();
        qb.push(" and (r.defect->>'typeId' = any(")
            .push_bind(ids)
            .push(")");
        if none {
            qb.push(" or r.defect is null");
        }
        qb.push(")");
    }
    if f.attachments != 0 {
        qb.push(" and r.attachment_kinds & ")
            .push_bind(f.attachments)
            .push(" <> 0");
    }
    match &f.env {
        Some(EnvFilter::Key(k)) => {
            qb.push(
                " and exists (select 1 from series se where se.id = r.series_id and se.env_key = ",
            )
            .push_bind(no_nul(k))
            .push(")");
        }
        Some(EnvFilter::None) => {
            qb.push(
                " and exists (select 1 from series se where se.id = r.series_id and se.env_key is null)",
            );
        }
        None => {}
    }
}

/// Results inlined in one tree response, at most; the other groups are read page by page.
pub const MAX_TREE_INLINE: usize = 5000;
/// Rows read per query while paging the tests of one group.
const TREE_SCAN_CHUNK: i64 = 2000;
/// Separator of Playwright titles in full names.
const TITLE_SEP: &str = " \u{203a} ";

/// Fixture steps attached to one attempt at read time, at most.
pub const MAX_FIXTURE_STEPS: usize = 10_000;
/// Largest tree the tree endpoint builds.
pub const MAX_TREE_RESULTS: i64 = 200_000;

pub(crate) const SUMMARY_SELECT: &str = "select r.id, r.launch_id, r.launch_created_at, r.test_case_id, r.series_id, \
    s.series_key, r.name, r.full_name, r.suite_path, r.status, r.flaky_in_run, r.attempt_count, r.duration_ms, \
    r.started_at, r.labels, r.muted, r.mute_until, r.fingerprint, r.defect, r.message, r.transition, r.flaky, \
    r.run_item_id, (select c.number from test_cases tc join cases c on c.id = tc.case_id \
      where tc.id = r.test_case_id) as case_number \
    from results r join series s on s.id = r.series_id ";

pub(crate) fn summary_row(r: &PgRow) -> Result<ResultSummary, sqlx::Error> {
    let labels: serde_json::Value = r.try_get("labels")?;
    let defect: Option<serde_json::Value> = r.try_get("defect")?;
    let run_item_id: Option<Uuid> = r.try_get("run_item_id")?;
    Ok(ResultSummary {
        id: r.try_get("id")?,
        launch_id: r.try_get("launch_id")?,
        launch_created_at: r.try_get("launch_created_at")?,
        test_case_id: r.try_get("test_case_id")?,
        series_id: r.try_get("series_id")?,
        series_key: r.try_get("series_key")?,
        name: r.try_get("name")?,
        full_name: r.try_get("full_name")?,
        suite_path: r.try_get("suite_path")?,
        status: r.try_get("status")?,
        flaky_in_run: r.try_get("flaky_in_run")?,
        attempt_count: r.try_get("attempt_count")?,
        duration_ms: r.try_get("duration_ms")?,
        started_at: r.try_get("started_at")?,
        labels: serde_json::from_value::<Vec<LabelView>>(labels).unwrap_or_default(),
        muted: r.try_get("muted")?,
        mute_until: r.try_get("mute_until")?,
        fingerprint: r.try_get("fingerprint")?,
        defect: defect.and_then(|d| serde_json::from_value::<Defect>(d).ok()),
        transition: r.try_get("transition")?,
        flaky: r.try_get("flaky")?,
        message: r
            .try_get::<Option<String>, _>("message")?
            .map(|m| truncate_chars(&m, MESSAGE_CHARS)),
        client_ids: Vec::new(),
        source: if run_item_id.is_some() {
            crate::tm::ResultSource::Manual
        } else {
            crate::tm::ResultSource::Automated
        },
        case_number: r.try_get("case_number")?,
        run_item_id,
    })
}

/// Characters of the error message in result lists.
pub const MESSAGE_CHARS: usize = 500;

fn truncate_chars(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn encode_cursor(full_name: &str, id: Uuid) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{id}\n{full_name}"))
}

fn decode_cursor(c: &str) -> Option<(String, Uuid)> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c)
        .ok()?;
    let s = String::from_utf8(raw).ok()?;
    let (id, name) = s.split_once('\n')?;
    Some((name.to_string(), id.parse().ok()?))
}

/// The kind as the API names it (camelCase).
pub fn kind_name(k: AttachmentKind) -> &'static str {
    match k {
        AttachmentKind::Image => "image",
        AttachmentKind::Video => "video",
        AttachmentKind::Trace => "trace",
        AttachmentKind::ImageDiff => "imageDiff",
        AttachmentKind::HttpExchange => "httpExchange",
        AttachmentKind::Text => "text",
        AttachmentKind::Log => "log",
        AttachmentKind::Json => "json",
        AttachmentKind::Other => "other",
    }
}

fn attachment_view(a: &AttachmentRef) -> AttachmentView {
    AttachmentView {
        id: a.id.as_ref().and_then(|i| i.0.parse().ok()),
        name: a.name.clone(),
        content_type: a.content_type.clone(),
        kind: kind_name(a.kind).to_string(),
        size: a.size,
    }
}

/// Parameters safe to show: masked and hidden ones are removed.
pub fn parameter_views(ps: &[Parameter]) -> Vec<ParameterView> {
    ps.iter()
        .filter(|p| p.mode == ParameterMode::Default)
        .map(|p| ParameterView {
            name: p.name.clone(),
            value: p.value.clone(),
        })
        .collect()
}

fn error_view(
    message: Option<&str>,
    trace: Option<&str>,
    actual: Option<&str>,
    expected: Option<&str>,
) -> Option<ErrorView> {
    if message.is_none() && trace.is_none() && actual.is_none() && expected.is_none() {
        return None;
    }
    Some(ErrorView {
        message: message.map(str::to_string),
        trace: trace.map(str::to_string),
        actual: actual.map(str::to_string),
        expected: expected.map(str::to_string),
    })
}

fn step_view(s: &Step) -> StepView {
    let d = &s.status_details;
    StepView {
        name: s.name.clone(),
        status: s.status.as_str().to_string(),
        kind: match s.kind {
            StepKind::Step => "step",
            StepKind::Fixture => "fixture",
        }
        .to_string(),
        started_at: ts_ms(s.start),
        stopped_at: ts_ms(s.stop),
        duration_ms: s.duration_ms.or(match (s.start, s.stop) {
            (Some(a), Some(b)) if b >= a => Some(b - a),
            _ => None,
        }),
        error: error_view(
            d.message.as_deref(),
            d.trace.as_deref(),
            d.actual.as_deref(),
            d.expected.as_deref(),
        ),
        parameters: parameter_views(&s.parameters),
        attachments: s.attachments.iter().map(attachment_view).collect(),
        steps: s.steps.iter().map(step_view).collect(),
    }
}

fn status_rank(s: &str) -> Status {
    Status::parse_lenient(s)
}

fn add_count(c: &mut Counts, status: &str, flaky: bool, muted: bool) {
    c.total += 1;
    match status_rank(status) {
        Status::Passed => c.passed += 1,
        Status::Failed => c.failed += 1,
        Status::Broken => c.broken += 1,
        Status::Skipped => c.skipped += 1,
        Status::Unknown => c.unknown += 1,
    }
    if flaky {
        c.flaky += 1;
    }
    if muted {
        c.muted += 1;
    }
}

#[derive(Default)]
struct Builder {
    counts: Counts,
    duration: Option<i64>,
    children: BTreeMap<String, Builder>,
    ids: Vec<Uuid>,
    tests: Vec<TreeTest>,
}

impl Builder {
    fn add(&mut self, status: &str, flaky: bool, muted: bool, duration: Option<i64>) {
        add_count(&mut self.counts, status, flaky, muted);
        if let Some(d) = duration {
            self.duration = Some(self.duration.unwrap_or(0).saturating_add(d.max(0)));
        }
    }

    /// Groups with direct tests: `(path, tests, failing, holds the revealed result)`.
    fn groups(
        &self,
        path: &mut Vec<String>,
        reveal: Option<Uuid>,
        out: &mut Vec<(Vec<String>, usize, bool, bool)>,
    ) {
        if !self.tests.is_empty() {
            let failing = self
                .tests
                .iter()
                .any(|t| matches!(t.status.as_str(), "failed" | "broken"));
            let revealed = reveal.is_some_and(|r| self.tests.iter().any(|t| t.id == r));
            out.push((path.clone(), self.tests.len(), failing, revealed));
        }
        for (n, b) in &self.children {
            path.push(n.clone());
            b.groups(path, reveal, out);
            path.pop();
        }
    }

    fn into_node(
        self,
        name: String,
        path: Vec<String>,
        include_tests: bool,
        inline: &HashSet<Vec<String>>,
    ) -> TreeNode {
        let children = self
            .children
            .into_iter()
            .map(|(n, b)| {
                let mut p = path.clone();
                p.push(n.clone());
                b.into_node(n, p, include_tests, inline)
            })
            .collect();
        let test_count = include_tests.then_some(self.tests.len() as i64);
        let tests = (include_tests && !self.tests.is_empty() && inline.contains(&path))
            .then_some(self.tests);
        TreeNode {
            name,
            path,
            counts: self.counts,
            children,
            result_ids: (!include_tests && !self.ids.is_empty()).then_some(self.ids),
            duration_ms: self.duration,
            test_count,
            tests,
            levels: None,
        }
    }
}

/// Groups whose tests fit [`MAX_TREE_INLINE`]: the revealed one, then those with failures,
/// then the smallest.
fn inline_groups(mut groups: Vec<(Vec<String>, usize, bool, bool)>) -> HashSet<Vec<String>> {
    groups.sort_by(|a, b| {
        b.3.cmp(&a.3)
            .then(b.2.cmp(&a.2))
            .then(a.1.cmp(&b.1))
            .then(a.0.cmp(&b.0))
    });
    let mut left = MAX_TREE_INLINE;
    let mut out = HashSet::new();
    for (path, n, _, _) in groups {
        if n <= left {
            left -= n;
            out.insert(path);
        }
    }
    out
}

fn label<'a>(labels: &'a [LabelView], name: &str) -> Option<&'a str> {
    labels
        .iter()
        .find(|l| l.name == name)
        .map(|l| l.value.as_str())
        .filter(|v| !v.is_empty())
}

/// Splits a class name into its package and the class: `com.example.Test` is
/// `["com.example", "Test"]`.
fn split_class(class: &str) -> Vec<String> {
    match class.rfind('.') {
        Some(i) if i > 0 && i + 1 < class.len() => {
            vec![class[..i].to_string(), class[i + 1..].to_string()]
        }
        _ => vec![class.to_string()],
    }
}

/// The suite path in the tree: Allure suite labels as stored; Playwright full names
/// (`file › describe › title`) become `file › describe`; a JUnit suite named like its class
/// (`package.Class#name`) becomes `package › Class`. The flag tells that the leaf name is the
/// last title of the full name.
fn suite_tree_path(
    suite_path: &[String],
    labels: &[LabelView],
    full_name: &str,
) -> (Vec<String>, bool) {
    let allure = label(labels, "parentSuite").is_some() || label(labels, "subSuite").is_some();
    if !allure && suite_path.len() <= 1 {
        let first = suite_path.first().map(String::as_str).unwrap_or("");
        if full_name.contains(TITLE_SEP) && full_name.starts_with(first) {
            let mut segs: Vec<String> = full_name.split(TITLE_SEP).map(str::to_string).collect();
            segs.pop();
            if segs.iter().all(|s| !s.is_empty()) {
                return (segs, true);
            }
        }
        if let Some(i) = full_name.rfind('#')
            && i > 0
            && (suite_path.is_empty() || first == &full_name[..i])
        {
            return (split_class(&full_name[..i]), false);
        }
    }
    (suite_path.to_vec(), false)
}

#[cfg(test)]
fn group_path(
    by: &GroupBy,
    suite_path: &[String],
    labels: &[LabelView],
    full_name: &str,
) -> Vec<String> {
    place(by, suite_path, labels, full_name).0
}

/// The group path of a result and whether its leaf name is the last title of the full name.
fn place(
    by: &GroupBy,
    suite_path: &[String],
    labels: &[LabelView],
    full_name: &str,
) -> (Vec<String>, bool) {
    match by {
        GroupBy::Labels(names) => (label_path(names, labels), false),
        GroupBy::Suite => suite_tree_path(suite_path, labels, full_name),
        GroupBy::Feature => (
            ["epic", "feature", "story"]
                .iter()
                .filter_map(|n| label(labels, n))
                .map(str::to_string)
                .collect(),
            false,
        ),
        GroupBy::Package => {
            let base = label(labels, "package")
                .map(str::to_string)
                .unwrap_or_else(|| {
                    if let Some((file, _)) = full_name.split_once(TITLE_SEP) {
                        return file.to_string();
                    }
                    let cut = full_name.rfind('#').or_else(|| full_name.rfind('.'));
                    cut.map(|i| full_name[..i].to_string()).unwrap_or_default()
                });
            let sep: &[char] =
                if full_name.contains(TITLE_SEP) && label(labels, "package").is_none() {
                    &['/']
                } else {
                    &['.', '/']
                };
            (
                base.split(sep)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
                false,
            )
        }
    }
}

/// The group path of a label grouping: the value of each level, [`NONE_GROUP`] where a
/// label is missing; levels after the last label that is set are left out.
fn label_path(names: &[String], labels: &[LabelView]) -> Vec<String> {
    let values: Vec<Option<&str>> = names.iter().map(|n| label(labels, n)).collect();
    let Some(last) = values.iter().rposition(Option::is_some) else {
        return vec![NONE_GROUP.to_string()];
    };
    values[..=last]
        .iter()
        .map(|v| v.unwrap_or(NONE_GROUP).to_string())
        .collect()
}

fn leaf_name(row: &TreeRow, titled: bool) -> String {
    if titled
        && let Some((_, title)) = row.full_name.rsplit_once(TITLE_SEP)
        && !title.is_empty()
    {
        return title.to_string();
    }
    row.name.clone()
}

/// SQL conditions that every result of the group `path` meets (a superset, checked in Rust).
fn push_group_prefilter(qb: &mut QueryBuilder<Postgres>, by: &GroupBy, path: &[String]) {
    let Some(last) = path.last() else { return };
    match by {
        GroupBy::Labels(names) => {
            for (n, v) in names.iter().zip(path) {
                if v == NONE_GROUP {
                    qb.push(" and ");
                    push_label_missing(qb, n);
                } else {
                    qb.push(" and r.labels @> ")
                        .push_bind(json_no_nul(serde_json::json!([{"name": n, "value": v}])));
                }
            }
        }
        GroupBy::Suite => {
            let titled = format!("{}{}%", like_escape(&path.join(TITLE_SEP)), TITLE_SEP);
            let class = format!("{}#%", like_escape(&path.join(".")));
            qb.push(" and (r.suite_path = ")
                .push_bind(path.iter().map(|p| no_nul(p)).collect::<Vec<_>>())
                .push(" or r.full_name like ")
                .push_bind(titled)
                .push(" or r.full_name like ")
                .push_bind(class)
                .push(")");
        }
        GroupBy::Feature => {
            qb.push(" and (");
            for (i, n) in ["epic", "feature", "story"].iter().enumerate() {
                if i > 0 {
                    qb.push(" or ");
                }
                qb.push("r.labels @> ")
                    .push_bind(json_no_nul(serde_json::json!([{"name": n, "value": last}])));
            }
            qb.push(")");
        }
        GroupBy::Package => {}
    }
}

impl Store {
    pub async fn results(
        &self,
        launch: &Launch,
        f: &ResultFilter,
    ) -> anyhow::Result<Page<ResultSummary>> {
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(SUMMARY_SELECT);
        qb.push("where r.launch_id = ")
            .push_bind(launch.id)
            .push(" and r.launch_created_at = ")
            .push_bind(launch.created_at);
        if !f.statuses.is_empty() {
            qb.push(" and r.status = any(")
                .push_bind(f.statuses.clone())
                .push(")");
        }
        if let Some(q) = &f.q {
            let like = format!("%{}%", like_escape(q));
            qb.push(" and (r.full_name ilike ")
                .push_bind(like.clone())
                .push(" or r.name ilike ")
                .push_bind(like)
                .push(" or to_tsvector('simple', coalesce(r.message, '')) @@ plainto_tsquery('simple', ")
                .push_bind(no_nul(q))
                .push("))");
        }
        if let Some(s) = &f.suite {
            qb.push(" and ")
                .push_bind(no_nul(s))
                .push(" = any(r.suite_path)");
        }
        if let Some((k, v)) = &f.label {
            qb.push(" and r.labels @> ")
                .push_bind(json_no_nul(serde_json::json!([{"name": k, "value": v}])));
        }
        if let Some(fl) = f.flaky {
            qb.push(" and r.flaky_in_run = ").push_bind(fl);
        }
        if let Some(fp) = &f.fingerprint {
            qb.push(" and r.fingerprint = ").push_bind(no_nul(fp));
        }
        match f.source {
            Some(crate::tm::ResultSource::Manual) => {
                qb.push(" and r.run_item_id is not null");
            }
            Some(crate::tm::ResultSource::Automated) => {
                qb.push(" and r.run_item_id is null");
            }
            None => {}
        }
        push_extra_filter(&mut qb, &f.extra);
        if let Some((name, id)) = f.before.as_deref().and_then(decode_cursor) {
            qb.push(" and (r.full_name, r.id) > (")
                .push_bind(name)
                .push(", ")
                .push_bind(id)
                .push(")");
        }
        qb.push(" order by r.full_name, r.id limit ")
            .push_bind(f.limit + 1);
        let rows = qb.build().fetch_all(&self.pool).await?;
        let mut items: Vec<ResultSummary> =
            rows.iter().map(summary_row).collect::<Result<_, _>>()?;
        let next = if items.len() as i64 > f.limit {
            items.truncate(f.limit as usize);
            items.last().map(|r| encode_cursor(&r.full_name, r.id))
        } else {
            None
        };
        if !items.is_empty() {
            let ids: Vec<Uuid> = items.iter().map(|r| r.id).collect();
            let keys: Vec<(Uuid, String)> = sqlx::query_as(
                "select result_id, substr(client_key, 3) from attempts \
                 where launch_id = $1 and launch_created_at = $2 and result_id = any($3) \
                   and client_key like 'r:%' order by result_id, seq limit 20000",
            )
            .bind(launch.id)
            .bind(launch.created_at)
            .bind(&ids)
            .fetch_all(&self.pool)
            .await?;
            let mut by: HashMap<Uuid, Vec<String>> = HashMap::new();
            for (r, k) in keys {
                by.entry(r).or_default().push(k);
            }
            for r in &mut items {
                r.client_ids = by.remove(&r.id).unwrap_or_default();
            }
        }
        Ok(Page { items, next })
    }

    /// The tree of a launch: groups with counts, leaf groups carry result ids.
    pub async fn tree(&self, launch: &Launch, by: GroupBy) -> anyhow::Result<TreeNode> {
        self.tree_with(launch, &TreeOptions::groups(by)).await
    }

    /// The tree of a launch: groups with counts and durations; with `include_tests`, the
    /// results of the groups that fit [`MAX_TREE_INLINE`] as leaves, else result ids.
    pub async fn tree_with(&self, launch: &Launch, o: &TreeOptions) -> anyhow::Result<TreeNode> {
        let mut qb = tree_select(&o.by);
        qb.push("where r.launch_id = ")
            .push_bind(launch.id)
            .push(" and r.launch_created_at = ")
            .push_bind(launch.created_at);
        push_tree_filter(&mut qb, &o.filter);
        qb.push(" order by r.full_name, r.id limit ")
            .push_bind(MAX_TREE_RESULTS);
        let rows = qb.build().fetch_all(&self.pool).await?;
        let rows: Vec<TreeRow> = rows.iter().map(tree_row).collect::<Result<_, _>>()?;
        let by = match &o.by {
            GroupBy::Labels(names) => GroupBy::Labels(
                names
                    .iter()
                    .filter(|n| rows.iter().any(|r| label(&r.labels, n).is_some()))
                    .cloned()
                    .collect(),
            ),
            other => other.clone(),
        };
        let mut root = Builder::default();
        for row in rows {
            let (path, titled) = place(&by, &row.suite_path, &row.labels, &row.full_name);
            root.add(&row.status, row.flaky_in_run, row.muted, row.duration_ms);
            let mut node = &mut root;
            for seg in path {
                node = node.children.entry(seg).or_default();
                node.add(&row.status, row.flaky_in_run, row.muted, row.duration_ms);
            }
            if o.include_tests {
                let name = leaf_name(&row, titled);
                node.tests.push(row.leaf(name));
            } else {
                node.ids.push(row.id);
            }
        }
        let inline = if o.include_tests {
            let mut groups = Vec::new();
            root.groups(&mut Vec::new(), o.reveal, &mut groups);
            inline_groups(groups)
        } else {
            HashSet::new()
        };
        let mut node = root.into_node(String::new(), Vec::new(), o.include_tests, &inline);
        if let GroupBy::Labels(levels) = by {
            node.levels = Some(levels);
        }
        Ok(node)
    }

    /// A page of the results directly in the group `path` of the tree, in full name order.
    pub async fn tree_tests(
        &self,
        launch: &Launch,
        by: &GroupBy,
        path: &[String],
        f: &TreeFilter,
        before: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Page<TreeTest>> {
        let limit = limit.max(1) as usize;
        let mut after = before.and_then(decode_cursor);
        let mut items: Vec<(TreeTest, String)> = Vec::new();
        let mut scanned: i64 = 0;
        let mut more = false;
        while scanned < MAX_TREE_RESULTS {
            let mut qb = tree_select(by);
            qb.push("where r.launch_id = ")
                .push_bind(launch.id)
                .push(" and r.launch_created_at = ")
                .push_bind(launch.created_at);
            push_tree_filter(&mut qb, f);
            push_group_prefilter(&mut qb, by, path);
            if let Some((name, id)) = &after {
                qb.push(" and (r.full_name, r.id) > (")
                    .push_bind(name.clone())
                    .push(", ")
                    .push_bind(*id)
                    .push(")");
            }
            let chunk = TREE_SCAN_CHUNK.min(MAX_TREE_RESULTS - scanned);
            qb.push(" order by r.full_name, r.id limit ")
                .push_bind(chunk);
            let rows = qb.build().fetch_all(&self.pool).await?;
            let n = rows.len() as i64;
            scanned += n;
            for r in &rows {
                let row = tree_row(r)?;
                after = Some((row.full_name.clone(), row.id));
                let (p, titled) = place(by, &row.suite_path, &row.labels, &row.full_name);
                if p.as_slice() != path {
                    continue;
                }
                if items.len() == limit {
                    more = true;
                    break;
                }
                let name = leaf_name(&row, titled);
                let full_name = row.full_name.clone();
                items.push((row.leaf(name), full_name));
            }
            if more || n < chunk {
                break;
            }
        }
        let next = more
            .then(|| items.last().map(|(t, full)| encode_cursor(full, t.id)))
            .flatten();
        Ok(Page {
            items: items.into_iter().map(|(t, _)| t).collect(),
            next,
        })
    }

    /// A result of the project with its attempts (steps and fixtures included) and the last 20
    /// results of its series.
    pub async fn result_detail(
        &self,
        project_id: Uuid,
        id: Uuid,
        history: &dyn HistoryStore,
    ) -> anyhow::Result<Option<ResultDetail>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{SUMMARY_SELECT} where r.project_id = $1 and r.id = $2"
        )))
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = r else { return Ok(None) };
        let summary = summary_row(&r)?;
        type AttemptRow = (
            Uuid,
            String,
            Option<DateTime<Utc>>,
            Option<DateTime<Utc>>,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            serde_json::Value,
            Option<Vec<u8>>,
            Option<Vec<u8>>,
            i64,
            String,
        );
        let mut rows: Vec<AttemptRow> = sqlx::query_as(
            "select id, status, started_at, stopped_at, duration_ms, message, trace, source_uuid, trace_id, detail, \
             steps_z, logs_z, seq, client_key from attempts where result_id = $1 and launch_created_at = $2 \
             order by seq limit 1000",
        )
        .bind(summary.id)
        .bind(summary.launch_created_at)
        .fetch_all(&self.pool)
        .await?;
        rows.sort_by_key(|a| (a.2.is_some(), a.2, a.12));
        let uuids: Vec<String> = rows.iter().filter_map(|a| a.7.clone()).collect();
        let mut fixtures_of: HashMap<String, Vec<(Option<i64>, ContainerFixtures)>> =
            HashMap::new();
        if !uuids.is_empty() {
            let containers: Vec<(Vec<String>, Vec<u8>, Option<i64>)> = sqlx::query_as(
                "select children, fixtures_z, start_ms from launch_containers \
                 where launch_id = $1 and children && $2::text[] limit 200",
            )
            .bind(summary.launch_id)
            .bind(&uuids)
            .fetch_all(&self.pool)
            .await?;
            for (children, z, start) in containers {
                let Ok(fx) = codec::decode::<ContainerFixtures>(&z) else {
                    continue;
                };
                for u in uuids.iter().filter(|u| children.contains(u)) {
                    fixtures_of
                        .entry(u.clone())
                        .or_default()
                        .push((start, fx.clone()));
                }
            }
        }
        let mut attempts = Vec::with_capacity(rows.len());
        let mut latest_detail = AttemptDetail::default();
        for (no, a) in rows.iter().enumerate() {
            let detail: AttemptDetail = serde_json::from_value(a.9.clone()).unwrap_or_default();
            let steps: Vec<Step> = match &a.10 {
                Some(z) => codec::decode(z).unwrap_or_default(),
                None => Vec::new(),
            };
            let logs: AttemptLogs = match &a.11 {
                Some(z) => codec::decode(z).unwrap_or_default(),
                None => AttemptLogs::default(),
            };
            let mut befores: Vec<Step> = Vec::new();
            let mut afters: Vec<Step> = Vec::new();
            if let Some(list) = a.7.as_ref().and_then(|u| fixtures_of.get(u)) {
                let mut budget = MAX_FIXTURE_STEPS;
                for (_, fx) in list {
                    for (src, dst) in [(&fx.befores, &mut befores), (&fx.afters, &mut afters)] {
                        for s in src {
                            let n = s.count();
                            if n > budget {
                                continue;
                            }
                            budget -= n;
                            let mut s = s.clone();
                            s.kind = StepKind::Fixture;
                            dst.push(s);
                        }
                    }
                }
                befores.sort_by_key(|s| s.start.unwrap_or(i64::MAX));
                afters.sort_by_key(|s| s.start.unwrap_or(i64::MAX));
            }
            let mut views: Vec<StepView> = befores.iter().map(step_view).collect();
            views.extend(steps.iter().map(step_view));
            views.extend(afters.iter().map(step_view));
            attempts.push(AttemptView {
                no: no as i32 + 1,
                status: a.1.clone(),
                started_at: a.2,
                stopped_at: a.3,
                duration_ms: a.4,
                error: error_view(
                    a.5.as_deref(),
                    a.6.as_deref(),
                    detail.actual.as_deref(),
                    detail.expected.as_deref(),
                ),
                steps: views,
                attachments: detail.attachments.iter().map(attachment_view).collect(),
                stdout: logs.stdout.map(|t| t.text),
                stderr: logs.stderr.map(|t| t.text),
                trace_id: a.8.clone(),
                client_id: a.13.strip_prefix("r:").map(str::to_string),
            });
            latest_detail = detail;
        }
        let history = history
            .series_history(project_id, summary.series_id, 20)
            .await?
            .into_iter()
            .map(|h| SeriesPoint {
                launch_id: h.launch_id,
                launch_number: h.launch_number,
                result_id: h.result_id,
                status: h.status,
                started_at: h.started_at,
            })
            .collect();
        let launch_number: i64 = sqlx::query_scalar("select number from launches where id = $1")
            .bind(summary.launch_id)
            .fetch_one(&self.pool)
            .await?;
        let mute_reason: Option<String> = if summary.muted {
            sqlx::query_scalar("select reason from series_mutes where series_id = $1")
                .bind(summary.series_id)
                .fetch_optional(&self.pool)
                .await?
                .filter(|r: &String| !r.is_empty())
        } else {
            None
        };
        Ok(Some(ResultDetail {
            launch_number,
            mute_reason,
            description: latest_detail.description.html.clone(),
            description_text: latest_detail.description.text.clone(),
            parameters: parameter_views(&latest_detail.parameters),
            links: latest_detail
                .links
                .iter()
                .map(|l| LinkView {
                    name: l.name.clone(),
                    url: l.url.clone(),
                    link_type: l.link_type.clone(),
                })
                .collect(),
            attempts,
            history,
            summary,
        }))
    }

    /// The launch a result belongs to (for authorization of derived routes).
    pub async fn result_launch(&self, project_id: Uuid, id: Uuid) -> anyhow::Result<Option<Uuid>> {
        Ok(
            sqlx::query_scalar("select launch_id from results where project_id = $1 and id = $2")
                .bind(project_id)
                .bind(id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// A test case of the project.
    pub async fn test_case(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<crate::TestCaseView>> {
        let r: Option<(Uuid, String, String)> = sqlx::query_as(
            "select id, display_name, full_name from test_cases where project_id = $1 and id = $2",
        )
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(|(id, name, full_name)| crate::TestCaseView {
            id,
            name,
            full_name,
        }))
    }
}

/// A label name of a launch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelName {
    pub name: String,
    /// Results with the label.
    pub count: i64,
    /// Distinct values.
    pub values: i64,
}

/// A value and how many results have it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ValueCount {
    pub value: String,
    pub count: i64,
}

/// The values of one label in a launch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelFacet {
    pub name: String,
    /// Most frequent first, at most [`MAX_FACET_VALUES`].
    pub values: Vec<ValueCount>,
    /// Results without the label.
    pub none: i64,
    /// More values exist than listed.
    pub more: bool,
}

/// Counts per value of the explorer filters, over the launch (in the environment asked for).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LaunchFacets {
    pub counts: Counts,
    pub retried: i64,
    /// Failed or broken, not muted results whose series did not fail in the baseline launch.
    pub new_failures: i64,
    /// The launch compared with: the previous finished launch on the same branch and
    /// environment (absent without one; then every failure is new).
    pub baseline_launch_id: Option<Uuid>,
    pub manual: i64,
    pub automated: i64,
    /// `severity`, `owner` and `tag`.
    pub labels: Vec<LabelFacet>,
    /// Defect type ids (`none`: results without a defect).
    pub defects: Vec<ValueCount>,
    /// `trace`, `video` and `screenshot`.
    pub attachments: Vec<ValueCount>,
    pub max_duration_ms: Option<i64>,
}

/// The results of one environment of a launch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCounts {
    /// The environment key of the series; `null` for results without one.
    pub key: Option<String>,
    pub counts: Counts,
}

/// Label names listed for a launch, at most.
pub const MAX_LABEL_NAMES: i64 = 200;
/// Values listed per label facet, at most.
pub const MAX_FACET_VALUES: usize = 50;
/// Environments listed for a launch, at most.
pub const MAX_ENVIRONMENTS: usize = 100;
/// Labels with value counts in the facets.
pub const FACET_LABELS: [&str; 3] = ["severity", "owner", "tag"];

fn launch_scope(qb: &mut QueryBuilder<Postgres>, launch: &Launch) {
    qb.push(" where r.launch_id = ")
        .push_bind(launch.id)
        .push(" and r.launch_created_at = ")
        .push_bind(launch.created_at);
}

impl Store {
    /// The label names of the results of a launch, most used first.
    pub async fn launch_labels(
        &self,
        launch: &Launch,
        f: &TreeFilter,
    ) -> anyhow::Result<Vec<LabelName>> {
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "select l->>'name' as name, count(distinct r.id) as n, count(distinct l->>'value') as v \
             from results r cross join lateral jsonb_array_elements(r.labels) l",
        );
        launch_scope(&mut qb, launch);
        push_tree_filter(&mut qb, f);
        qb.push(" and coalesce(l->>'name', '') <> '' group by 1 order by 2 desc, 1 limit ")
            .push_bind(MAX_LABEL_NAMES);
        let rows: Vec<(String, i64, i64)> = qb.build_query_as().fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|(name, count, values)| LabelName {
                name,
                count,
                values,
            })
            .collect())
    }

    /// Counts of the explorer filter values over a launch, within `f` (its environment).
    pub async fn launch_facets(
        &self,
        launch: &Launch,
        f: &TreeFilter,
        baseline: &Baseline,
    ) -> anyhow::Result<LaunchFacets> {
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "select count(*), count(*) filter (where r.status = 'passed'), \
             count(*) filter (where r.status = 'failed'), count(*) filter (where r.status = 'broken'), \
             count(*) filter (where r.status = 'skipped'), \
             count(*) filter (where r.status not in ('passed', 'failed', 'broken', 'skipped')), \
             count(*) filter (where r.flaky_in_run), count(*) filter (where r.muted), \
             count(*) filter (where r.attempt_count > 1), count(*) filter (where r.run_item_id is not null), \
             count(*) filter (where r.attachment_kinds & ",
        );
        qb.push_bind(KIND_TRACE)
            .push(" <> 0), count(*) filter (where r.attachment_kinds & ")
            .push_bind(KIND_VIDEO)
            .push(" <> 0), count(*) filter (where r.attachment_kinds & ")
            .push_bind(KIND_SCREENSHOT)
            .push(" <> 0), count(*) filter (where r.defect is null), max(r.duration_ms), ");
        for n in FACET_LABELS {
            qb.push("count(*) filter (where ");
            push_label_missing(&mut qb, n);
            qb.push("), ");
        }
        qb.push("count(*) filter (where r.status in ('failed', 'broken') and not r.muted");
        if let Some((id, created)) = baseline.launch {
            qb.push(" and not exists (select 1 from results b where b.launch_id = ")
                .push_bind(id)
                .push(" and b.launch_created_at = ")
                .push_bind(created)
                .push(" and b.series_id = r.series_id and b.status in ('failed', 'broken'))");
        }
        qb.push(") from results r");
        launch_scope(&mut qb, launch);
        push_tree_filter(&mut qb, f);
        let row = qb.build().fetch_one(&self.pool).await?;
        let n = |i: usize| -> Result<i64, sqlx::Error> { row.try_get::<i64, _>(i) };
        let counts = Counts {
            total: n(0)?,
            passed: n(1)?,
            failed: n(2)?,
            broken: n(3)?,
            skipped: n(4)?,
            unknown: n(5)?,
            flaky: n(6)?,
            muted: n(7)?,
        };
        let manual = n(9)?;
        let attachments = ATTACHMENT_FILTERS
            .iter()
            .enumerate()
            .map(|(i, (name, _))| {
                Ok(ValueCount {
                    value: (*name).to_string(),
                    count: n(10 + i)?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        let no_defect = n(13)?;
        let max_duration_ms: Option<i64> = row.try_get(14)?;
        let label_none: Vec<i64> = (0..FACET_LABELS.len())
            .map(|i| n(15 + i))
            .collect::<Result<_, _>>()?;
        let new_failures = n(15 + FACET_LABELS.len())?;
        let retried = n(8)?;

        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "select l->>'name', l->>'value', count(distinct r.id) from results r \
             cross join lateral jsonb_array_elements(r.labels) l",
        );
        launch_scope(&mut qb, launch);
        push_tree_filter(&mut qb, f);
        qb.push(" and l->>'name' = any(")
            .push_bind(FACET_LABELS.map(str::to_string).to_vec())
            .push(") and coalesce(l->>'value', '') <> '' group by 1, 2 order by 3 desc, 2 limit ")
            .push_bind((MAX_FACET_VALUES * FACET_LABELS.len() * 4) as i64);
        let rows: Vec<(String, String, i64)> = qb.build_query_as().fetch_all(&self.pool).await?;
        let mut labels: Vec<LabelFacet> = FACET_LABELS
            .iter()
            .zip(label_none)
            .map(|(name, none)| LabelFacet {
                name: (*name).to_string(),
                values: Vec::new(),
                none,
                more: false,
            })
            .collect();
        for (name, value, count) in rows {
            if let Some(l) = labels.iter_mut().find(|l| l.name == name) {
                if l.values.len() < MAX_FACET_VALUES {
                    l.values.push(ValueCount { value, count });
                } else {
                    l.more = true;
                }
            }
        }

        let mut qb: QueryBuilder<Postgres> =
            QueryBuilder::new("select r.defect->>'typeId', count(*) from results r");
        launch_scope(&mut qb, launch);
        push_tree_filter(&mut qb, f);
        qb.push(" and r.defect is not null group by 1 order by 2 desc limit 200");
        let rows: Vec<(Option<String>, i64)> = qb.build_query_as().fetch_all(&self.pool).await?;
        let mut defects: Vec<ValueCount> = rows
            .into_iter()
            .filter_map(|(v, count)| v.map(|value| ValueCount { value, count }))
            .collect();
        if no_defect > 0 {
            defects.push(ValueCount {
                value: "none".into(),
                count: no_defect,
            });
        }
        Ok(LaunchFacets {
            automated: counts.total - manual,
            counts,
            retried,
            new_failures,
            baseline_launch_id: baseline.launch.map(|b| b.0),
            manual,
            labels,
            defects,
            attachments,
            max_duration_ms,
        })
    }

    /// The environments of the results of a launch (by the environment key of their series),
    /// with counts, largest first.
    pub async fn launch_environments(
        &self,
        launch: &Launch,
    ) -> anyhow::Result<Vec<EnvironmentCounts>> {
        let rows: Vec<(Option<String>, String, bool, bool, i64)> = sqlx::query_as(
            "select se.env_key, r.status, r.flaky_in_run, r.muted, count(*) from results r \
             join series se on se.id = r.series_id \
             where r.launch_id = $1 and r.launch_created_at = $2 group by 1, 2, 3, 4",
        )
        .bind(launch.id)
        .bind(launch.created_at)
        .fetch_all(&self.pool)
        .await?;
        let mut by: BTreeMap<Option<String>, Counts> = BTreeMap::new();
        for (key, status, flaky, muted, n) in rows {
            let c = by.entry(key).or_default();
            let mut one = Counts::default();
            add_count(&mut one, &status, flaky, muted);
            c.total += n;
            c.passed += one.passed * n;
            c.failed += one.failed * n;
            c.broken += one.broken * n;
            c.skipped += one.skipped * n;
            c.unknown += one.unknown * n;
            c.flaky += one.flaky * n;
            c.muted += one.muted * n;
        }
        let mut out: Vec<EnvironmentCounts> = by
            .into_iter()
            .map(|(key, counts)| EnvironmentCounts { key, counts })
            .collect();
        out.sort_by(|a, b| b.counts.total.cmp(&a.counts.total).then(a.key.cmp(&b.key)));
        out.truncate(MAX_ENVIRONMENTS);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trip() {
        let id = Uuid::now_v7();
        let c = encode_cursor("a.b#c\nd", id);
        assert_eq!(decode_cursor(&c), Some(("a.b#c\nd".to_string(), id)));
        assert_eq!(decode_cursor("!!"), None);
    }

    fn labels(pairs: &[(&str, &str)]) -> Vec<LabelView> {
        pairs
            .iter()
            .map(|(n, v)| LabelView {
                name: (*n).into(),
                value: (*v).into(),
            })
            .collect()
    }

    #[test]
    fn suite_paths_by_format() {
        let pw = "e2e/checkout.spec.ts \u{203a} Checkout \u{203a} Card \u{203a} pays";
        assert_eq!(
            place(&GroupBy::Suite, &["e2e/checkout.spec".into()], &[], pw),
            (
                vec![
                    "e2e/checkout.spec.ts".to_string(),
                    "Checkout".into(),
                    "Card".into()
                ],
                true
            )
        );
        assert_eq!(
            place(
                &GroupBy::Suite,
                &["com.example.CheckoutTest".into()],
                &labels(&[("suite", "com.example.CheckoutTest")]),
                "com.example.CheckoutTest#pays"
            )
            .0,
            vec!["com.example", "CheckoutTest"]
        );
        assert_eq!(
            place(
                &GroupBy::Suite,
                &["api".into()],
                &labels(&[("suite", "api")]),
                "api.Suite#first"
            )
            .0,
            vec!["api"],
            "an explicit suite that is not the class stays"
        );
        let allure = labels(&[
            ("parentSuite", "staging"),
            ("suite", "cart.spec.ts"),
            ("subSuite", "Cart"),
        ]);
        assert_eq!(
            place(
                &GroupBy::Suite,
                &["staging".into(), "cart.spec.ts".into(), "Cart".into()],
                &allure,
                "cart.spec.ts \u{203a} Cart \u{203a} adds"
            ),
            (
                vec!["staging".to_string(), "cart.spec.ts".into(), "Cart".into()],
                false
            )
        );
        assert_eq!(split_class("Plain"), vec!["Plain"]);
        assert_eq!(split_class(".Odd"), vec![".Odd"]);
        assert_eq!(
            place(
                &GroupBy::Package,
                &[],
                &[],
                "e2e/shop/cart.spec.ts \u{203a} Cart \u{203a} adds"
            )
            .0,
            vec!["e2e", "shop", "cart.spec.ts"]
        );
    }

    #[test]
    fn inline_prefers_reveal_then_failures_then_small() {
        let big = MAX_TREE_INLINE - 10;
        let groups = vec![
            (vec!["big".to_string()], big, false, false),
            (vec!["failing".to_string()], 20, true, false),
            (vec!["revealed".to_string()], 30, false, true),
            (vec!["small".to_string()], 5, false, false),
        ];
        let set = inline_groups(groups);
        assert!(set.contains(&vec!["revealed".to_string()]));
        assert!(set.contains(&vec!["failing".to_string()]));
        assert!(set.contains(&vec!["small".to_string()]));
        assert!(!set.contains(&vec!["big".to_string()]));
    }

    #[test]
    fn label_grouping_places_missing_levels_in_none() {
        let by = GroupBy::Labels(vec!["epic".into(), "feature".into(), "story".into()]);
        let all = labels(&[("epic", "Shop"), ("feature", "Cart"), ("story", "Add")]);
        assert_eq!(group_path(&by, &[], &all, "x"), vec!["Shop", "Cart", "Add"]);
        let gap = labels(&[("story", "Add"), ("epic", "Shop")]);
        assert_eq!(
            group_path(&by, &[], &gap, "x"),
            vec!["Shop", NONE_GROUP, "Add"]
        );
        let top = labels(&[("epic", "Shop"), ("owner", "bob")]);
        assert_eq!(group_path(&by, &[], &top, "x"), vec!["Shop"]);
        assert_eq!(group_path(&by, &[], &[], "x"), vec![NONE_GROUP]);
        let empty = labels(&[("epic", ""), ("epic", "Later"), ("feature", "Cart")]);
        assert_eq!(
            group_path(&by, &[], &empty, "x"),
            vec![NONE_GROUP, "Cart"],
            "the first value counts, an empty one is missing"
        );
        assert!(valid_label_name("x.custom-1:a/b@c"));
        assert!(!valid_label_name("a b"));
        assert!(!valid_label_name(&"n".repeat(MAX_LABEL_NAME_BYTES + 1)));
        assert!(!valid_label_name(""));
    }

    #[test]
    fn package_grouping() {
        let p = group_path(&GroupBy::Package, &[], &[], "com.example.CheckoutTest#pays");
        assert_eq!(p, vec!["com", "example", "CheckoutTest"]);
        let labels = vec![LabelView {
            name: "feature".into(),
            value: "Payments".into(),
        }];
        assert_eq!(
            group_path(&GroupBy::Feature, &[], &labels, "x"),
            vec!["Payments"]
        );
    }
}
