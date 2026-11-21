//! Reading results: lists, the launch tree, and the detail with attempts, steps and fixtures.

use std::collections::{BTreeMap, HashMap, HashSet};

use base64::Engine as _;
use chrono::{DateTime, Utc};
use fastproof_model::{
    AttachmentKind, AttachmentRef, Parameter, ParameterMode, Status, Step, StepKind,
};
use sqlx::postgres::PgRow;
use sqlx::{Postgres, QueryBuilder, Row};
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
    pub before: Option<String>,
    pub limit: i64,
}

/// Grouping of the launch tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupBy {
    Suite,
    Feature,
    Package,
}

/// Filters of the launch tree.
#[derive(Clone, Debug, Default)]
pub struct TreeFilter {
    pub statuses: Vec<String>,
    /// Substring of the test name, full name or suite path.
    pub q: Option<String>,
    /// Only results flaky in this run.
    pub flaky: Option<bool>,
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

const TREE_SELECT: &str = "select id, name, full_name, suite_path, labels, status, flaky_in_run, muted, \
    duration_ms, attempt_count, flaky, transition, defect from results ";

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
        qb.push(" and status = any(")
            .push_bind(f.statuses.clone())
            .push(")");
    }
    if let Some(q) = &f.q {
        let like = format!("%{}%", like_escape(q));
        qb.push(" and (name ilike ")
            .push_bind(like.clone())
            .push(" or full_name ilike ")
            .push_bind(like.clone())
            .push(" or array_to_string(suite_path, ' ') ilike ")
            .push_bind(like)
            .push(")");
    }
    if let Some(fl) = f.flaky {
        qb.push(" and flaky_in_run = ").push_bind(fl);
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
    r.started_at, r.labels, r.muted, r.mute_until, r.fingerprint, r.defect, r.message, r.transition, r.flaky \
    from results r join series s on s.id = r.series_id ";

pub(crate) fn summary_row(r: &PgRow) -> Result<ResultSummary, sqlx::Error> {
    let labels: serde_json::Value = r.try_get("labels")?;
    let defect: Option<serde_json::Value> = r.try_get("defect")?;
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
    by: GroupBy,
    suite_path: &[String],
    labels: &[LabelView],
    full_name: &str,
) -> Vec<String> {
    place(by, suite_path, labels, full_name).0
}

/// The group path of a result and whether its leaf name is the last title of the full name.
fn place(
    by: GroupBy,
    suite_path: &[String],
    labels: &[LabelView],
    full_name: &str,
) -> (Vec<String>, bool) {
    match by {
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
fn push_group_prefilter(qb: &mut QueryBuilder<Postgres>, by: GroupBy, path: &[String]) {
    let Some(last) = path.last() else { return };
    match by {
        GroupBy::Suite => {
            let titled = format!("{}{}%", like_escape(&path.join(TITLE_SEP)), TITLE_SEP);
            let class = format!("{}#%", like_escape(&path.join(".")));
            qb.push(" and (suite_path = ")
                .push_bind(path.iter().map(|p| no_nul(p)).collect::<Vec<_>>())
                .push(" or full_name like ")
                .push_bind(titled)
                .push(" or full_name like ")
                .push_bind(class)
                .push(")");
        }
        GroupBy::Feature => {
            qb.push(" and (");
            for (i, n) in ["epic", "feature", "story"].iter().enumerate() {
                if i > 0 {
                    qb.push(" or ");
                }
                qb.push("labels @> ")
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
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(TREE_SELECT);
        qb.push("where launch_id = ")
            .push_bind(launch.id)
            .push(" and launch_created_at = ")
            .push_bind(launch.created_at);
        push_tree_filter(&mut qb, &o.filter);
        qb.push(" order by full_name, id limit ")
            .push_bind(MAX_TREE_RESULTS);
        let rows = qb.build().fetch_all(&self.pool).await?;
        let mut root = Builder::default();
        for r in &rows {
            let row = tree_row(r)?;
            let (path, titled) = place(o.by, &row.suite_path, &row.labels, &row.full_name);
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
        drop(rows);
        let inline = if o.include_tests {
            let mut groups = Vec::new();
            root.groups(&mut Vec::new(), o.reveal, &mut groups);
            inline_groups(groups)
        } else {
            HashSet::new()
        };
        Ok(root.into_node(String::new(), Vec::new(), o.include_tests, &inline))
    }

    /// A page of the results directly in the group `path` of the tree, in full name order.
    pub async fn tree_tests(
        &self,
        launch: &Launch,
        by: GroupBy,
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
            let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(TREE_SELECT);
            qb.push("where launch_id = ")
                .push_bind(launch.id)
                .push(" and launch_created_at = ")
                .push_bind(launch.created_at);
            push_tree_filter(&mut qb, f);
            push_group_prefilter(&mut qb, by, path);
            if let Some((name, id)) = &after {
                qb.push(" and (full_name, id) > (")
                    .push_bind(name.clone())
                    .push(", ")
                    .push_bind(*id)
                    .push(")");
            }
            let chunk = TREE_SCAN_CHUNK.min(MAX_TREE_RESULTS - scanned);
            qb.push(" order by full_name, id limit ").push_bind(chunk);
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
            place(GroupBy::Suite, &["e2e/checkout.spec".into()], &[], pw),
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
                GroupBy::Suite,
                &["com.example.CheckoutTest".into()],
                &labels(&[("suite", "com.example.CheckoutTest")]),
                "com.example.CheckoutTest#pays"
            )
            .0,
            vec!["com.example", "CheckoutTest"]
        );
        assert_eq!(
            place(
                GroupBy::Suite,
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
                GroupBy::Suite,
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
                GroupBy::Package,
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
    fn package_grouping() {
        let p = group_path(GroupBy::Package, &[], &[], "com.example.CheckoutTest#pays");
        assert_eq!(p, vec!["com", "example", "CheckoutTest"]);
        let labels = vec![LabelView {
            name: "feature".into(),
            value: "Payments".into(),
        }];
        assert_eq!(
            group_path(GroupBy::Feature, &[], &labels, "x"),
            vec!["Payments"]
        );
    }
}
