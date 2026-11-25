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
    /// Only results marked with this log pattern (`result_patterns`).
    pub pattern: Option<String>,
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

impl EnvFilter {
    /// The bind values of an optional environment filter in static SQL: a mode (0 any, 1 the
    /// key, 2 no key) and the key; see [`ENV_MATCH`].
    pub fn args(env: Option<&EnvFilter>) -> (i32, Option<String>) {
        match env {
            None => (0, None),
            Some(EnvFilter::Key(k)) => (1, Some(no_nul(k))),
            Some(EnvFilter::None) => (2, None),
        }
    }
}

/// `se.env_key` matches the arguments of [`EnvFilter::args`] bound as `$m` and `$k` (replace
/// them with the parameter numbers).
pub const ENV_MATCH: &str = "(($m = 1 and se.env_key = $k) or ($m = 2 and se.env_key is null))";

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

/// The labels the grouping reads, projected from `r.labels` (they are most of a row's bytes).
fn push_label_projection(qb: &mut QueryBuilder<Postgres>, by: &GroupBy) {
    let names = by.label_names();
    if names.is_empty() {
        qb.push("'[]'::jsonb");
        return;
    }
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
        .push(")");
}

/// The select of the tree rows, with the labels the grouping reads.
fn tree_select(by: &GroupBy) -> QueryBuilder<Postgres> {
    let mut qb: QueryBuilder<Postgres> =
        QueryBuilder::new("select r.id, r.name, r.full_name, r.suite_path, ");
    push_label_projection(&mut qb, by);
    qb.push(
        " as labels, r.status, r.flaky_in_run, r.muted, r.duration_ms, r.attempt_count, r.flaky, \
         r.transition, r.defect, r.suite_tree, r.suite_titled from results r ",
    );
    qb
}

/// Label levels of a grouping, at most (the columns `l0` to `l4` of [`push_place_key`]).
const KEY_LABELS: usize = MAX_GROUP_LEVELS;
/// The columns of [`push_place_key`].
const PLACE_KEY: &str = "st, stt, sp, l0, l1, l2, l3, l4, tp, sw, hp, dp, ft";

/// The values of the labels the grouping reads, one JSON array per name (`l0` to `l4`), from
/// `results.label_values` (computed from `labels` for results the backfill has not reached).
fn push_label_columns(qb: &mut QueryBuilder<Postgres>, by: &GroupBy, only_if: &str) {
    let names = by.label_names();
    for i in 0..KEY_LABELS {
        match names.get(i) {
            Some(n) => {
                qb.push(format!(
                    "case when {only_if} then coalesce(r.label_values, fastproof_label_values(r.labels)) ->> "
                ))
                .push_bind(no_nul(n))
                .push(format!("::text end as l{i}, "));
            }
            None => {
                qb.push(format!("null::text as l{i}, "));
            }
        }
    }
}

/// The SQL form of [`PlaceKey::of`]: the columns [`PLACE_KEY`] of a result `r`, for the
/// grouping `by`. Keep the two in step (`tests/explorer_perf.rs` compares the trees).
fn push_place_key(qb: &mut QueryBuilder<Postgres>, by: &GroupBy) {
    // `f` before the last `c`.
    let before_last = |c: char| {
        format!("left(r.full_name, length(r.full_name) - strpos(reverse(r.full_name), '{c}'))")
    };
    let sep = TITLE_SEP;
    let short = "cardinality(r.suite_path) <= 1";
    match by {
        GroupBy::Suite => {
            // The path stored at ingest (`suite_tree`), else what `place` needs to compute it.
            let short = format!("r.suite_tree is null and {short}");
            qb.push(
                "r.suite_tree as st, r.suite_titled as stt, \
                 case when r.suite_tree is null then r.suite_path else '{}'::text[] end as sp, ",
            );
            push_label_columns(qb, by, "r.suite_tree is null");
            qb.push(format!(
                "case when {short} and strpos(r.full_name, '{sep}') > 0 \
                   then trim_array(string_to_array(r.full_name, '{sep}'), 1) end as tp, \
                 coalesce({short} and strpos(r.full_name, '{sep}') > 0 \
                   and starts_with(r.full_name, coalesce(r.suite_path[1], '')), false) as sw, \
                 case when {short} and strpos(r.full_name, '#') > 0 then {} end as hp, \
                 null::text as dp, null::text as ft",
                before_last('#'),
            ));
        }
        GroupBy::Package => {
            qb.push("null::text[] as st, null::bool as stt, '{}'::text[] as sp, ");
            push_label_columns(qb, by, "true");
            qb.push(format!(
                "null::text[] as tp, false as sw, \
                 case when strpos(r.full_name, '{sep}') = 0 and strpos(r.full_name, '#') > 0 \
                   then {} end as hp, \
                 case when strpos(r.full_name, '{sep}') = 0 and strpos(r.full_name, '#') = 0 \
                   and strpos(r.full_name, '.') > 0 then {} end as dp, \
                 case when strpos(r.full_name, '{sep}') > 0 then split_part(r.full_name, '{sep}', 1) end as ft",
                before_last('#'),
                before_last('.'),
            ));
        }
        GroupBy::Feature | GroupBy::Labels(_) => {
            qb.push("null::text[] as st, null::bool as stt, '{}'::text[] as sp, ");
            push_label_columns(qb, by, "true");
            qb.push(
                "null::text[] as tp, false as sw, null::text as hp, null::text as dp, \
                 null::text as ft",
            );
        }
    }
}

/// What places a result in the tree: the parts of its suite path, labels and full name that
/// [`place`] reads. Results of one group mostly share it, so the tree counts them per key in
/// SQL ([`push_place_key`]) and reads only the results of the groups it inlines.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct PlaceKey {
    /// Suite: the path and leaf-name flag stored at ingest (`results.suite_tree`), when set.
    #[serde(default)]
    stored: Option<(Vec<String>, bool)>,
    suite_path: Vec<String>,
    /// The labels the grouping reads, in their order.
    labels: Vec<LabelView>,
    /// Suite: the titles of the full name (split at the title separator) but the last, when
    /// the full name has one and the suite path has at most one name.
    titles: Option<Vec<String>>,
    /// Suite: with `titles`, whether the full name starts with the first suite name.
    starts_with_suite: bool,
    /// Suite (short suite path) and Package (no title separator): the full name before its
    /// last `#`.
    hash_prefix: Option<String>,
    /// Package: the full name before its last `.`, without a title separator or `#`.
    dot_prefix: Option<String>,
    /// Package: the full name before its first title separator.
    first_title: Option<String>,
}

impl PlaceKey {
    /// The key of a result read in full (`labels` projected for `by` or not).
    fn of(by: &GroupBy, suite_path: &[String], labels: &[LabelView], full_name: &str) -> Self {
        let labels: Vec<LabelView> = by
            .label_names()
            .iter()
            .flat_map(|n| labels.iter().filter(move |l| &l.name == n))
            .cloned()
            .collect();
        let before_last = |c: char| full_name.rsplit_once(c).map(|(a, _)| a.to_string());
        match by {
            GroupBy::Suite => {
                let short = suite_path.len() <= 1;
                let titles = (short && full_name.contains(TITLE_SEP)).then(|| {
                    let mut t: Vec<String> =
                        full_name.split(TITLE_SEP).map(str::to_string).collect();
                    t.pop();
                    t
                });
                PlaceKey {
                    stored: None,
                    suite_path: suite_path.to_vec(),
                    labels,
                    starts_with_suite: titles.is_some()
                        && full_name.starts_with(suite_path.first().map_or("", String::as_str)),
                    titles,
                    hash_prefix: short.then(|| before_last('#')).flatten(),
                    dot_prefix: None,
                    first_title: None,
                }
            }
            GroupBy::Package => {
                let first_title = full_name.split_once(TITLE_SEP).map(|(a, _)| a.to_string());
                let hash_prefix = first_title.is_none().then(|| before_last('#')).flatten();
                let dot_prefix = (first_title.is_none() && !full_name.contains('#'))
                    .then(|| before_last('.'))
                    .flatten();
                PlaceKey {
                    labels,
                    hash_prefix,
                    dot_prefix,
                    first_title,
                    ..PlaceKey::default()
                }
            }
            GroupBy::Feature | GroupBy::Labels(_) => PlaceKey {
                labels,
                ..PlaceKey::default()
            },
        }
    }

    /// The key of a row of [`push_place_key`] for the grouping `by`.
    fn from_row(by: &GroupBy, r: &PgRow) -> Result<Self, sqlx::Error> {
        let mut labels = Vec::new();
        for (i, name) in by.label_names().iter().take(KEY_LABELS).enumerate() {
            let values: Option<String> = r.try_get(format!("l{i}").as_str())?;
            let values: Vec<serde_json::Value> = values
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_default();
            labels.extend(values.into_iter().map(|v| LabelView {
                name: name.clone(),
                value: match v {
                    serde_json::Value::String(s) => s,
                    serde_json::Value::Null => String::new(),
                    other => other.to_string(),
                },
            }));
        }
        let stored: Option<Vec<String>> = r.try_get("st")?;
        let titled: Option<bool> = r.try_get("stt")?;
        Ok(PlaceKey {
            stored: stored.map(|p| (p, titled.unwrap_or(false))),
            suite_path: r.try_get("sp")?,
            labels,
            titles: r.try_get("tp")?,
            starts_with_suite: r.try_get("sw")?,
            hash_prefix: r.try_get("hp")?,
            dot_prefix: r.try_get("dp")?,
            first_title: r.try_get("ft")?,
        })
    }
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
    /// `suite_tree` and `suite_titled`, when the select reads them and they are set.
    stored: Option<(Vec<String>, bool)>,
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
        stored: r
            .try_get::<Option<Vec<String>>, _>("suite_tree")
            .ok()
            .flatten()
            .map(|p| {
                let titled: Option<bool> = r.try_get("suite_titled").ok().flatten();
                (p, titled.unwrap_or(false))
            }),
    })
}

impl TreeRow {
    /// The key with the stored Suites path, when the row has one: the tree, its pages and the
    /// result-id tree then place the row alike. (Changing [`suite_tree_path`] needs a migration
    /// that clears `suite_tree` and resets the `explorer.results.backfill` state.)
    fn with_stored(&self, by: &GroupBy, mut key: PlaceKey) -> PlaceKey {
        if matches!(by, GroupBy::Suite) {
            key.stored = self.stored.clone();
        }
        key
    }

    fn leaf(&self, name: String) -> TreeTest {
        TreeTest {
            id: self.id,
            name,
            status: self.status.clone(),
            duration_ms: self.duration_ms,
            attempt_count: (self.attempt_count > 1).then_some(self.attempt_count),
            flaky_in_run: self.flaky_in_run,
            flaky: self.flaky,
            muted: self.muted,
            transition: self.transition.clone(),
            defect: self.defect.clone(),
        }
    }
}

/// A label grouping without the levels that no result has a value of.
fn present_levels<'a>(by: &GroupBy, mut labels: impl Iterator<Item = &'a [LabelView]>) -> GroupBy {
    match by {
        GroupBy::Labels(names) => {
            let mut present = vec![false; names.len()];
            for l in labels.by_ref() {
                for (i, n) in names.iter().enumerate() {
                    present[i] |=
                        !present[i] && l.iter().any(|x| &x.name == n && !x.value.is_empty());
                }
                if present.iter().all(|p| *p) {
                    break;
                }
            }
            GroupBy::Labels(
                names
                    .iter()
                    .zip(present)
                    .filter(|(_, p)| *p)
                    .map(|(n, _)| n.clone())
                    .collect(),
            )
        }
        other => other.clone(),
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

/// Results without a non-empty value of the label `name`.
fn push_label_missing(qb: &mut QueryBuilder<Postgres>, name: &str) {
    qb.push("not jsonb_path_exists(r.labels, '$[*] ? (@.name == $n && @.value != \"\")', ")
        .push_bind(json_no_nul(serde_json::json!({ "n": name })))
        .push(")");
}

/// The filters the tree and the result list share (all but status, text, flaky and source).
fn push_extra_filter(qb: &mut QueryBuilder<Postgres>, f: &TreeFilter) {
    if let Some(m) = &f.message {
        qb.push(
            " and to_tsvector('simple', left(coalesce(r.message, ''), 65536)) @@ plainto_tsquery('simple', ",
        )
        .push_bind(no_nul(m))
        .push(")");
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
/// Largest tree the tree endpoint builds by reading results (with result ids).
pub const MAX_TREE_RESULTS: i64 = 200_000;
/// Counted keys (distinct placements with status and flags) one tree reads, at most.
pub const MAX_TREE_KEYS: usize = 200_000;
/// Groups one tree holds, at most; beyond it the tree is `partial`.
pub const MAX_TREE_NODES: usize = 200_000;

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

fn add_counts(c: &mut Counts, d: &Counts) {
    c.total += d.total;
    c.passed += d.passed;
    c.failed += d.failed;
    c.broken += d.broken;
    c.skipped += d.skipped;
    c.unknown += d.unknown;
    c.flaky += d.flaky;
    c.muted += d.muted;
}

fn counts_of(status: &str, flaky: bool, muted: bool, n: i64) -> Counts {
    let mut one = Counts::default();
    add_count(&mut one, status, flaky, muted);
    Counts {
        total: n,
        passed: one.passed * n,
        failed: one.failed * n,
        broken: one.broken * n,
        skipped: one.skipped * n,
        unknown: one.unknown * n,
        flaky: one.flaky * n,
        muted: one.muted * n,
    }
}

#[derive(Default)]
struct Builder {
    counts: Counts,
    duration: Option<i64>,
    children: BTreeMap<String, Builder>,
    /// Results directly in this node.
    direct: i64,
    /// A failed or broken result is directly in this node.
    failing: bool,
    /// The revealed result is directly in this node.
    revealed: bool,
    ids: Vec<Uuid>,
    tests: Vec<TreeTest>,
    /// The counted keys directly in this node, with the leaf-name flag of their path.
    keys: Vec<(usize, bool)>,
}

impl Builder {
    fn add(&mut self, c: &Counts, duration: Option<i64>) {
        add_counts(&mut self.counts, c);
        if let Some(d) = duration {
            self.duration = Some(self.duration.unwrap_or(0).saturating_add(d.max(0)));
        }
    }

    /// Counts results (that share their paths) once in the root and in every node on their
    /// paths, and returns the nodes at the ends of the paths.
    fn place(
        &mut self,
        paths: &[(Vec<String>, bool)],
        c: &Counts,
        duration: Option<i64>,
        mut leaf: impl FnMut(&mut Builder, bool),
    ) -> usize {
        self.add(c, duration);
        let mut seen: HashSet<&[String]> = HashSet::new();
        let mut created = 0;
        for (path, titled) in paths {
            let mut node: &mut Builder = self;
            for i in 0..path.len() {
                node = match node.children.entry(path[i].clone()) {
                    std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
                    std::collections::btree_map::Entry::Vacant(v) => {
                        created += 1;
                        v.insert(Builder::default())
                    }
                };
                if seen.insert(&path[..=i]) {
                    node.add(c, duration);
                }
            }
            leaf(node, *titled);
        }
        created
    }

    fn node(&self, path: &[String]) -> Option<&Builder> {
        let mut node = self;
        for seg in path {
            node = node.children.get(seg)?;
        }
        Some(node)
    }

    fn node_mut(&mut self, path: &[String]) -> Option<&mut Builder> {
        let mut node = self;
        for seg in path {
            node = node.children.get_mut(seg)?;
        }
        Some(node)
    }

    /// Groups with direct results: `(path, results, failing, holds the revealed result)`.
    fn groups(&self, path: &mut Vec<String>, out: &mut Vec<(Vec<String>, i64, bool, bool)>) {
        if self.direct > 0 {
            out.push((path.clone(), self.direct, self.failing, self.revealed));
        }
        for (n, b) in &self.children {
            path.push(n.clone());
            b.groups(path, out);
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
        let test_count = include_tests.then_some(self.direct);
        let tests =
            (include_tests && self.direct > 0 && inline.contains(&path)).then_some(self.tests);
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
            partial: false,
        }
    }
}

/// Groups whose tests fit [`MAX_TREE_INLINE`]: the revealed one, then those with failures,
/// then the smallest.
fn inline_groups(mut groups: Vec<(Vec<String>, i64, bool, bool)>) -> HashSet<Vec<String>> {
    groups.sort_by(|a, b| {
        b.3.cmp(&a.3)
            .then(b.2.cmp(&a.2))
            .then(a.1.cmp(&b.1))
            .then(a.0.cmp(&b.0))
    });
    let mut left = MAX_TREE_INLINE as i64;
    let mut out = HashSet::new();
    for (path, n, _, _) in groups {
        if n <= left {
            left -= n;
            out.insert(path);
        }
    }
    out
}

/// The first value of the label `name`, unless empty (the suite and package groupings).
fn label<'a>(labels: &'a [LabelView], name: &str) -> Option<&'a str> {
    labels
        .iter()
        .find(|l| l.name == name)
        .map(|l| l.value.as_str())
        .filter(|v| !v.is_empty())
}

/// Distinct values of one label a label grouping places a result by, at most.
pub const MAX_LEVEL_VALUES: usize = 20;
/// Groups one result is placed in, at most (a result with several values of the grouping
/// labels is in each of their groups).
pub const MAX_RESULT_PATHS: usize = 64;

/// The distinct non-empty values of the label `name`, in their order, at most
/// [`MAX_LEVEL_VALUES`].
fn label_values<'a>(labels: &'a [LabelView], name: &str) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    for l in labels
        .iter()
        .filter(|l| l.name == name && !l.value.is_empty())
    {
        if !out.contains(&l.value.as_str()) {
            out.push(&l.value);
            if out.len() == MAX_LEVEL_VALUES {
                break;
            }
        }
    }
    out
}

/// Every combination of one value per level, in order, at most [`MAX_RESULT_PATHS`].
fn combinations(levels: &[Vec<&str>]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = vec![Vec::new()];
    for values in levels {
        let mut next = Vec::new();
        'outer: for p in &out {
            for v in values {
                if next.len() == MAX_RESULT_PATHS {
                    break 'outer;
                }
                let mut q = p.clone();
                q.push((*v).to_string());
                next.push(q);
            }
        }
        out = next;
    }
    out
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
fn suite_tree_path(k: &PlaceKey) -> (Vec<String>, bool) {
    let allure =
        label(&k.labels, "parentSuite").is_some() || label(&k.labels, "subSuite").is_some();
    if !allure && k.suite_path.len() <= 1 {
        let first = k.suite_path.first().map(String::as_str).unwrap_or("");
        if let Some(titles) = &k.titles
            && k.starts_with_suite
            && titles.iter().all(|s| !s.is_empty())
        {
            return (titles.clone(), true);
        }
        if let Some(class) = k.hash_prefix.as_deref()
            && !class.is_empty()
            && (k.suite_path.is_empty() || first == class)
        {
            return (split_class(class), false);
        }
    }
    (k.suite_path.clone(), false)
}

/// The suite path of a result in the tree and whether its leaf name is the last title of its
/// full name (stored at ingest as `results.suite_tree` and `suite_titled`).
pub fn suite_place(
    suite_path: &[String],
    labels: &[LabelView],
    full_name: &str,
) -> (Vec<String>, bool) {
    suite_tree_path(&PlaceKey::of(
        &GroupBy::Suite,
        suite_path,
        labels,
        full_name,
    ))
}

/// [`suite_place`] of labels stored as a JSON array of `{name, value}`.
pub fn suite_place_json(
    suite_path: &[String],
    labels: &serde_json::Value,
    full_name: &str,
) -> (Vec<String>, bool) {
    let labels: Vec<LabelView> = serde_json::from_value(labels.clone()).unwrap_or_default();
    suite_place(suite_path, &labels, full_name)
}

#[cfg(test)]
fn group_path(
    by: &GroupBy,
    suite_path: &[String],
    labels: &[LabelView],
    full_name: &str,
) -> Vec<String> {
    let k = PlaceKey::of(by, suite_path, labels, full_name);
    place(by, &k).swap_remove(0).0
}

/// The group paths of a result (several when a label of a label grouping has several
/// values) and whether its leaf name is the last title of the full name.
fn place(by: &GroupBy, k: &PlaceKey) -> Vec<(Vec<String>, bool)> {
    match by {
        GroupBy::Labels(names) => label_paths(names, &k.labels)
            .into_iter()
            .map(|p| (p, false))
            .collect(),
        GroupBy::Suite => vec![k.stored.clone().unwrap_or_else(|| suite_tree_path(k))],
        GroupBy::Feature => {
            let levels: Vec<Vec<&str>> = ["epic", "feature", "story"]
                .iter()
                .map(|n| label_values(&k.labels, n))
                .filter(|v| !v.is_empty())
                .collect();
            combinations(&levels)
                .into_iter()
                .map(|p| (p, false))
                .collect()
        }
        GroupBy::Package => {
            let package = label(&k.labels, "package");
            let base = match package {
                Some(p) => p.to_string(),
                None => k
                    .first_title
                    .clone()
                    .or_else(|| k.hash_prefix.clone())
                    .or_else(|| k.dot_prefix.clone())
                    .unwrap_or_default(),
            };
            let sep: &[char] = if k.first_title.is_some() && package.is_none() {
                &['/']
            } else {
                &['.', '/']
            };
            vec![(
                base.split(sep)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
                false,
            )]
        }
    }
}

/// The group paths of a label grouping: a value of each level, [`NONE_GROUP`] where a label
/// is missing; levels after the last label that is set are left out. A label with several
/// values places the result in the group of each.
fn label_paths(names: &[String], labels: &[LabelView]) -> Vec<Vec<String>> {
    let mut levels: Vec<Vec<&str>> = names.iter().map(|n| label_values(labels, n)).collect();
    let Some(last) = levels.iter().rposition(|v| !v.is_empty()) else {
        return vec![vec![NONE_GROUP.to_string()]];
    };
    levels.truncate(last + 1);
    for v in &mut levels {
        if v.is_empty() {
            v.push(NONE_GROUP);
        }
    }
    combinations(&levels)
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

/// Results of a launch with one [`PlaceKey`], status, flaky and muted flag, as counted.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Counted {
    key: PlaceKey,
    counts: Counts,
    failing: bool,
    duration: Option<i64>,
    #[serde(skip)]
    revealed: bool,
    /// Their ids, when the key has at most [`MAX_TREE_INLINE`] results (in the cache, only
    /// for the keys of the groups inlined).
    ids: Option<Vec<Uuid>>,
}

/// Counted keys of one tree cached, at most.
const MAX_CACHED_KEYS: usize = 20_000;

/// The cache key of an unfiltered tree (all results or those of one environment).
/// The cache key of the tree a launch page asks for first (Suites, no filter).
pub(crate) fn default_tree_cache_key() -> String {
    tree_cache_key(&GroupBy::Suite, &TreeFilter::default()).unwrap_or_default()
}

fn tree_cache_key(by: &GroupBy, f: &TreeFilter) -> Option<String> {
    let env = f.env.clone();
    let rest = TreeFilter {
        env: None,
        ..f.clone()
    };
    if !rest.is_empty() {
        return None;
    }
    let by = match by {
        GroupBy::Suite => serde_json::json!(["suite"]),
        GroupBy::Feature => serde_json::json!(["feature"]),
        GroupBy::Package => serde_json::json!(["package"]),
        GroupBy::Labels(names) => serde_json::json!(["labels", names]),
    };
    Some(format!(
        "tree2:{}",
        serde_json::json!([by, env_key_part(env.as_ref())])
    ))
}

/// The cache key part of an environment filter, as JSON (no two filters share one).
fn env_key_part(env: Option<&EnvFilter>) -> serde_json::Value {
    match env {
        None => serde_json::Value::Null,
        Some(EnvFilter::None) => serde_json::json!({ "none": true }),
        Some(EnvFilter::Key(k)) => serde_json::json!({ "key": k }),
    }
}

/// The order of results by name within a launch. It leads with the expression of the name
/// index of `results` (`results_launch_name_key`): a B-tree entry holds at most 2704 bytes,
/// and a name may have up to `max_name_bytes` (4 KiB). Names that share their first 600
/// characters are ordered by the whole name.
pub const NAME_ORDER: &str = "left(r.full_name, 600), r.full_name, r.id";

/// Keeps the results after `(name, id)` in [`NAME_ORDER`] (a page cursor).
fn push_after_name(qb: &mut QueryBuilder<Postgres>, name: &str, id: Uuid) {
    qb.push(" and (left(r.full_name, 600), r.full_name, r.id) > (left(")
        .push_bind(name.to_string())
        .push(", 600), ")
        .push_bind(name.to_string())
        .push(", ")
        .push_bind(id)
        .push(")");
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
            let p: Vec<String> = path.iter().map(|p| no_nul(p)).collect();
            qb.push(" and (r.suite_tree = ")
                .push_bind(p.clone())
                .push(" or r.suite_path = ")
                .push_bind(p)
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
                .push(
                    " or to_tsvector('simple', left(coalesce(r.message, ''), 65536)) @@ plainto_tsquery('simple', ",
                )
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
            let fp = no_nul(fp);
            qb.push(" and r.fingerprint = any(array(select ")
                .push_bind(fp.clone())
                .push("::text union all select a.fingerprint from failure_fingerprints a where a.project_id = ")
                .push_bind(launch.project_id)
                .push(" and a.alias_of = ")
                .push_bind(fp)
                .push("))");
        }
        if let Some(p) = &f.pattern {
            crate::analysis::log_rules::result_pattern_filter(&mut qb, p);
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
            push_after_name(&mut qb, &name, id);
        }
        qb.push(format!(" order by {NAME_ORDER} limit "))
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
        if o.include_tests {
            return self.tree_counted(launch, o).await;
        }
        let mut qb = tree_select(&o.by);
        qb.push("where r.launch_id = ")
            .push_bind(launch.id)
            .push(" and r.launch_created_at = ")
            .push_bind(launch.created_at);
        push_tree_filter(&mut qb, &o.filter);
        qb.push(format!(" order by {NAME_ORDER} limit "))
            .push_bind(MAX_TREE_RESULTS);
        let rows = qb.build().fetch_all(&self.pool).await?;
        let rows: Vec<TreeRow> = rows.iter().map(tree_row).collect::<Result<_, _>>()?;
        let by = present_levels(&o.by, rows.iter().map(|r| r.labels.as_slice()));
        let mut root = Builder::default();
        let mut nodes = 0;
        let mut partial = false;
        for row in rows {
            if nodes >= self.tree_limits.1 {
                partial = true;
                break;
            }
            let key = PlaceKey::of(&by, &row.suite_path, &row.labels, &row.full_name);
            let key = row.with_stored(&by, key);
            let c = counts_of(&row.status, row.flaky_in_run, row.muted, 1);
            nodes += root.place(&place(&by, &key), &c, row.duration_ms, |node, _| {
                node.direct += 1;
                node.ids.push(row.id);
            });
        }
        let mut node = root.into_node(String::new(), Vec::new(), false, &HashSet::new());
        node.partial = partial;
        if let GroupBy::Labels(levels) = by {
            node.levels = Some(levels);
        }
        Ok(node)
    }

    /// The tree with tests: SQL counts the results per [`PlaceKey`] (with their status and
    /// flags), the groups are built from the keys, and only the results of the groups inlined
    /// are read, by id. Both reads see one snapshot. For a launch that does not run, the
    /// counted keys of an unfiltered tree (in one environment or all) are cached with the ids
    /// of the groups inlined.
    async fn tree_counted(&self, launch: &Launch, o: &TreeOptions) -> anyhow::Result<TreeNode> {
        let cache_key = (launch.status != "running")
            .then(|| tree_cache_key(&o.by, &o.filter))
            .flatten();
        let mut tx = self.pool.begin().await?;
        sqlx::query("set transaction isolation level repeatable read, read only")
            .execute(&mut *tx)
            .await?;
        let mut generation = 0;
        let mut cached: Option<Vec<Counted>> = None;
        // An entry that misses only because of the reveal is not written again (a deep link
        // into a group the plain tree does not inline would rewrite it on every view).
        let mut had_entry = false;
        // Suites stored at ingest: the tests of the inlined groups are read by their path, so
        // no ids are aggregated (the largest cost of the count otherwise).
        let suites = matches!(o.by, GroupBy::Suite);
        let by_path = |c: &[Counted]| suites && c.iter().all(|k| k.key.stored.is_some());
        if let Some(key) = &cache_key {
            generation = crate::explorer::gen_of(&mut tx, launch.id).await?;
            if let Some(v) = crate::explorer::cache_get(&mut tx, launch.id, key, generation).await?
            {
                let parsed = serde_json::from_value::<Vec<Counted>>(v).ok();
                had_entry = parsed.is_some();
                cached = parsed.filter(|c| {
                    by_path(c)
                        || o.reveal.is_none_or(|r| {
                            c.iter()
                                .any(|k| k.ids.as_ref().is_some_and(|ids| ids.contains(&r)))
                        })
                });
            }
        }
        let from_cache = cached.is_some();
        let mut partial = false;
        let mut counted = match cached {
            Some(c) => c,
            None => {
                let (c, p) = self.count_keys(&mut tx, launch, o, !suites).await?;
                partial = p;
                c
            }
        };
        if suites && !by_path(&counted) && counted.iter().all(|c| c.ids.is_none()) {
            // Results not backfilled yet: count again with their ids.
            let (c, p) = self.count_keys(&mut tx, launch, o, true).await?;
            counted = c;
            partial = p;
        }
        let by_path = by_path(&counted);
        let by = present_levels(&o.by, counted.iter().map(|c| c.key.labels.as_slice()));
        let mut root = Builder::default();
        let mut nodes = 0;
        for (i, c) in counted.iter().enumerate() {
            if nodes >= self.tree_limits.1 {
                partial = true;
                break;
            }
            nodes += root.place(
                &place(&by, &c.key),
                &c.counts,
                c.duration,
                |node, titled| {
                    node.direct += c.counts.total;
                    node.failing |= c.failing;
                    node.revealed |= c.revealed;
                    node.keys.push((i, titled));
                },
            );
        }
        if by_path
            && from_cache
            && let Some(r) = o.reveal
        {
            let revealed: Option<(Vec<String>, bool)> = sqlx::query_as(
                "select suite_tree, suite_titled from results where launch_id = $1 \
                 and launch_created_at = $2 and id = $3 and suite_tree is not null",
            )
            .bind(launch.id)
            .bind(launch.created_at)
            .bind(r)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(node) = revealed.and_then(|(p, _)| root.node_mut(&p)) {
                node.revealed = true;
            }
        }
        let mut groups = Vec::new();
        root.groups(&mut Vec::new(), &mut groups);
        let plain = (o.reveal.is_some() && !from_cache).then(|| {
            inline_groups(
                groups
                    .iter()
                    .map(|(p, n, f, _)| (p.clone(), *n, *f, false))
                    .collect(),
            )
        });
        let inline = inline_groups(groups);
        // Stored suites whose ids are not known (the first build, or a revealed group the
        // cached build did not inline) are read by their path.
        let by_path_groups: HashSet<Vec<String>> = if by_path {
            inline
                .iter()
                .filter(|p| {
                    root.node(p)
                        .is_some_and(|n| n.keys.iter().any(|(i, _)| counted[*i].ids.is_none()))
                })
                .cloned()
                .collect()
        } else {
            HashSet::new()
        };
        let mut wanted: HashMap<Uuid, Vec<(&[String], bool)>> = HashMap::new();
        for path in inline.iter().filter(|p| !by_path_groups.contains(*p)) {
            for &(i, titled) in root
                .node(path)
                .map(|n| n.keys.as_slice())
                .unwrap_or_default()
            {
                for id in counted[i].ids.iter().flatten() {
                    wanted
                        .entry(*id)
                        .or_default()
                        .push((path.as_slice(), titled));
                }
            }
        }
        let keep: HashSet<usize> = plain
            .as_ref()
            .unwrap_or(&inline)
            .iter()
            .filter_map(|p| root.node(p))
            .flat_map(|n| n.keys.iter().map(|(i, _)| *i))
            .collect();
        // Read by path on the first build; the ids found are cached with the keys, so later
        // builds read the rows by id.
        if !by_path_groups.is_empty() {
            let paths: Vec<serde_json::Value> = by_path_groups
                .iter()
                .map(|p| json_no_nul(serde_json::json!(p)))
                .collect();
            // The hashes of the paths, so the rows are found through the suite index.
            let hashes: Vec<i64> = sqlx::query_scalar(
                "select hash_array_extended(array(select e from jsonb_array_elements_text(p) \
                   with ordinality as x(e, o) order by o), 0::bigint) \
                 from unnest($1::jsonb[]) as w(p)",
            )
            .bind(&paths)
            .fetch_all(&mut *tx)
            .await?;
            let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
                "select r.id, r.name, r.full_name, '{}'::text[] as suite_path, '[]'::jsonb as labels, \
                 r.status, r.flaky_in_run, r.muted, r.duration_ms, r.attempt_count, r.flaky, r.transition, \
                 r.defect, r.suite_tree, coalesce(r.suite_titled, false) as suite_titled from results r",
            );
            launch_scope(&mut qb, launch);
            qb.push(format!(
                " and {} = any(",
                crate::analysis::indexes::SUITE_HASH
            ))
            .push_bind(hashes)
            .push("::bigint[])");
            push_tree_filter(&mut qb, &o.filter);
            if by_path_groups.len() <= 64 {
                qb.push(" and (");
                for (i, p) in by_path_groups.iter().enumerate() {
                    if i > 0 {
                        qb.push(" or ");
                    }
                    qb.push("r.suite_tree = ")
                        .push_bind(p.iter().map(|x| no_nul(x)).collect::<Vec<_>>())
                        .push("::text[]");
                }
                qb.push(")");
            } else {
                qb.push(" and to_jsonb(r.suite_tree) in (select p from unnest(")
                    .push_bind(paths)
                    .push("::jsonb[]) as w(p))");
            }
            qb.push(format!(" order by {NAME_ORDER}"));
            let rows = qb.build().fetch_all(&mut *tx).await?;
            let mut found: HashMap<(Vec<String>, bool), Vec<Uuid>> = HashMap::new();
            for r in &rows {
                let row = tree_row(r)?;
                let path: Vec<String> = r.try_get("suite_tree")?;
                let titled: bool = r.try_get("suite_titled")?;
                if by_path_groups.contains(&path)
                    && let Some(node) = root.node_mut(&path)
                {
                    node.tests.push(row.leaf(leaf_name(&row, titled)));
                    found.entry((path, titled)).or_default().push(row.id);
                }
            }
            for path in &by_path_groups {
                for &(i, titled) in root
                    .node(path)
                    .map(|n| n.keys.as_slice())
                    .unwrap_or_default()
                {
                    counted[i].ids =
                        Some(found.remove(&(path.clone(), titled)).unwrap_or_default());
                }
            }
        }
        if !wanted.is_empty() {
            let ids: Vec<Uuid> = wanted.keys().copied().collect();
            let rows = sqlx::query(
                "select r.id, r.name, r.full_name, '{}'::text[] as suite_path, '[]'::jsonb as labels, \
                 r.status, r.flaky_in_run, r.muted, r.duration_ms, r.attempt_count, r.flaky, r.transition, \
                 r.defect from results r where r.launch_id = $1 and r.launch_created_at = $2 \
                 and r.id = any($3) order by left(r.full_name, 600), r.full_name, r.id",
            )
            .bind(launch.id)
            .bind(launch.created_at)
            .bind(&ids)
            .fetch_all(&mut *tx)
            .await?;
            for r in &rows {
                let row = tree_row(r)?;
                for (path, titled) in wanted.get(&row.id).into_iter().flatten() {
                    if let Some(node) = root.node_mut(path) {
                        node.tests.push(row.leaf(leaf_name(&row, *titled)));
                    }
                }
            }
        }
        tx.commit().await?;
        if from_cache && let Some(key) = &cache_key {
            self.cache_read(launch.id, key).await;
        }
        if let Some(key) = cache_key
            .filter(|_| !from_cache && !partial && !had_entry && counted.len() <= MAX_CACHED_KEYS)
        {
            let stored: Vec<Counted> = counted
                .iter()
                .enumerate()
                .map(|(i, c)| Counted {
                    ids: if keep.contains(&i) {
                        c.ids.clone()
                    } else {
                        None
                    },
                    revealed: false,
                    ..c.clone()
                })
                .collect();
            self.cache_put(launch.id, &key, generation, &serde_json::to_value(&stored)?)
                .await;
        }
        let mut node = root.into_node(String::new(), Vec::new(), true, &inline);
        node.partial = partial;
        if let GroupBy::Labels(levels) = by {
            node.levels = Some(levels);
        }
        Ok(node)
    }

    /// The results of a launch counted per stored suite path (`suite_tree`, null for results
    /// the backfill has not reached), status and flags, without ids. The planner expects as
    /// many groups as rows and would sort them on disk, so the statement asks for hashing.
    async fn count_stored_suites(
        &self,
        conn: &mut sqlx::PgConnection,
        launch: &Launch,
        o: &TreeOptions,
    ) -> anyhow::Result<(Vec<Counted>, bool)> {
        let limit = self.tree_limits.0;
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "select r.suite_tree as st, coalesce(r.suite_titled, false) as stt, r.status, r.flaky_in_run, \
             r.muted, count(*) as n, least(sum(greatest(r.duration_ms, 0)), 9223372036854775807)::bigint as d, ",
        );
        match o.reveal {
            Some(id) => {
                qb.push("coalesce(bool_or(r.id = ")
                    .push_bind(id)
                    .push("), false)");
            }
            None => {
                qb.push("false");
            }
        }
        qb.push(" as rv from results r");
        launch_scope(&mut qb, launch);
        push_tree_filter(&mut qb, &o.filter);
        qb.push(" group by 1, 2, 3, 4, 5 limit ")
            .push_bind(limit as i64 + 1);
        sqlx::query("set local enable_sort = off")
            .execute(&mut *conn)
            .await?;
        let rows = qb.build().fetch_all(&mut *conn).await;
        sqlx::query("set local enable_sort = on")
            .execute(&mut *conn)
            .await?;
        let rows = rows?;
        let partial = rows.len() > limit;
        let counted = rows
            .iter()
            .take(limit)
            .map(|r| {
                let status: String = r.try_get("status")?;
                let path: Option<Vec<String>> = r.try_get("st")?;
                Ok(Counted {
                    key: PlaceKey {
                        stored: path
                            .map(|p| Ok::<_, sqlx::Error>((p, r.try_get("stt")?)))
                            .transpose()?,
                        ..PlaceKey::default()
                    },
                    counts: counts_of(
                        &status,
                        r.try_get("flaky_in_run")?,
                        r.try_get("muted")?,
                        r.try_get("n")?,
                    ),
                    failing: matches!(status.as_str(), "failed" | "broken"),
                    duration: r.try_get("d")?,
                    revealed: r.try_get("rv")?,
                    ids: None,
                })
            })
            .collect::<Result<_, sqlx::Error>>()?;
        Ok((counted, partial))
    }

    /// The results of a launch counted per [`PlaceKey`], status and flags, with their ids
    /// where a key has at most [`MAX_TREE_INLINE`] results.
    async fn count_keys(
        &self,
        conn: &mut sqlx::PgConnection,
        launch: &Launch,
        o: &TreeOptions,
        with_ids: bool,
    ) -> anyhow::Result<(Vec<Counted>, bool)> {
        if matches!(o.by, GroupBy::Suite) && !with_ids {
            return self.count_stored_suites(conn, launch, o).await;
        }
        let limit = self.tree_limits.0;
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(format!(
            "select {PLACE_KEY}, status, flaky_in_run, muted, count(*) as n, \
             least(sum(greatest(duration_ms, 0)), 9223372036854775807)::bigint as d, "
        ));
        match o.reveal {
            Some(id) => {
                qb.push("coalesce(bool_or(id = ")
                    .push_bind(id)
                    .push("), false)");
            }
            None => {
                qb.push("false");
            }
        }
        if with_ids {
            qb.push(format!(
                " as rv, case when sum(count(*)) over (partition by {PLACE_KEY}) <= "
            ))
            .push_bind(MAX_TREE_INLINE as i64)
            .push(" then array_agg(id) end as ids from (select ");
        } else {
            qb.push(" as rv, null::uuid[] as ids from (select ");
        }
        push_place_key(&mut qb, &o.by);
        qb.push(", r.id, r.status, r.flaky_in_run, r.muted, r.duration_ms from results r where r.launch_id = ")
            .push_bind(launch.id)
            .push(" and r.launch_created_at = ")
            .push_bind(launch.created_at);
        push_tree_filter(&mut qb, &o.filter);
        qb.push(format!(
            ") k group by {PLACE_KEY}, status, flaky_in_run, muted limit "
        ))
        .push_bind(limit as i64 + 1);
        let rows = qb.build().fetch_all(&mut *conn).await?;
        let partial = rows.len() > limit;
        let counted: Vec<Counted> = rows
            .iter()
            .take(limit)
            .map(|r| {
                let status: String = r.try_get("status")?;
                Ok(Counted {
                    key: PlaceKey::from_row(&o.by, r)?,
                    counts: counts_of(
                        &status,
                        r.try_get("flaky_in_run")?,
                        r.try_get("muted")?,
                        r.try_get("n")?,
                    ),
                    failing: matches!(status.as_str(), "failed" | "broken"),
                    duration: r.try_get("d")?,
                    revealed: r.try_get("rv")?,
                    ids: r.try_get("ids")?,
                })
            })
            .collect::<Result<_, sqlx::Error>>()?;
        Ok((counted, partial))
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
                push_after_name(&mut qb, name, *id);
            }
            let chunk = TREE_SCAN_CHUNK.min(MAX_TREE_RESULTS - scanned);
            qb.push(format!(" order by {NAME_ORDER} limit "))
                .push_bind(chunk);
            let rows = qb.build().fetch_all(&self.pool).await?;
            let n = rows.len() as i64;
            scanned += n;
            for r in &rows {
                let row = tree_row(r)?;
                after = Some((row.full_name.clone(), row.id));
                let key = PlaceKey::of(by, &row.suite_path, &row.labels, &row.full_name);
                let key = row.with_stored(by, key);
                let Some(titled) = place(by, &key)
                    .into_iter()
                    .find(|(p, _)| p.as_slice() == path)
                    .map(|(_, t)| t)
                else {
                    continue;
                };
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
        let patterns = self.result_patterns(summary.id).await?;
        Ok(Some(ResultDetail {
            launch_number,
            mute_reason,
            patterns,
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
    /// Only the first results of a very large launch were counted.
    #[serde(default)]
    pub partial: bool,
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

/// Results an aggregate of the label names or the filter counts reads, at most (the counts
/// are then `partial`).
pub const MAX_FACET_RESULTS: i64 = MAX_TREE_RESULTS;
/// Values per label name the filter counts read of one result, at most.
pub const MAX_FACET_LABELS: i64 = 64;

/// The label names of a launch, with whether only part of its results was read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchLabels {
    pub items: Vec<LabelName>,
    pub partial: bool,
}

/// Whether the label names and facets of a launch read only part of its results.
fn partial(launch: &Launch) -> bool {
    launch.counts.total > MAX_FACET_RESULTS
}

/// The results of a launch within `f` as `r`: all of them, or for a launch with more than
/// [`MAX_FACET_RESULTS`], that many (a bounded subquery, which PostgreSQL does not run in
/// parallel, so smaller launches read the table directly).
fn push_bounded(qb: &mut QueryBuilder<Postgres>, launch: &Launch, f: &TreeFilter, cols: &str) {
    if !partial(launch) {
        qb.push(format!("(select {cols} from results r"));
        launch_scope(qb, launch);
        push_tree_filter(qb, f);
        qb.push(") r");
        return;
    }
    qb.push(format!("(select {cols} from results r"));
    launch_scope(qb, launch);
    push_tree_filter(qb, f);
    qb.push(" limit ").push_bind(MAX_FACET_RESULTS).push(") r");
}

/// The cache key part of the filters of the label names and facets (`None` when more than
/// the environment is set: such counts are not cached).
fn env_only(f: &TreeFilter) -> Option<serde_json::Value> {
    let rest = TreeFilter {
        env: None,
        ..f.clone()
    };
    rest.is_empty().then(|| env_key_part(f.env.as_ref()))
}

/// A read of explorer data in one snapshot, served from the cache when the launch does not
/// run and `key` is cacheable.
struct CachedRead {
    tx: sqlx::Transaction<'static, Postgres>,
    key: Option<String>,
    generation: i64,
}

impl Store {
    async fn cached_read(
        &self,
        launch: &Launch,
        key: Option<String>,
    ) -> anyhow::Result<(CachedRead, Option<serde_json::Value>)> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("set transaction isolation level repeatable read, read only")
            .execute(&mut *tx)
            .await?;
        let key = key.filter(|_| launch.status != "running");
        let mut generation = 0;
        let mut hit = None;
        if let Some(k) = &key {
            generation = crate::explorer::gen_of(&mut tx, launch.id).await?;
            hit = crate::explorer::cache_get(&mut tx, launch.id, k, generation).await?;
        }
        Ok((
            CachedRead {
                tx,
                key,
                generation,
            },
            hit,
        ))
    }

    async fn cache_done<T: Serialize>(
        &self,
        launch: &Launch,
        read: CachedRead,
        value: &T,
    ) -> anyhow::Result<()> {
        read.tx.commit().await?;
        if let Some(k) = read.key {
            self.cache_put(
                launch.id,
                &k,
                read.generation,
                &serde_json::to_value(value)?,
            )
            .await;
        }
        Ok(())
    }

    /// The label names of the results of a launch, most used first.
    pub async fn launch_labels(
        &self,
        launch: &Launch,
        f: &TreeFilter,
    ) -> anyhow::Result<LaunchLabels> {
        let key = env_only(f).map(|e| format!("labels2:{e}"));
        let (mut read, hit) = self.cached_read(launch, key).await?;
        if let Some(v) = hit.and_then(|v| serde_json::from_value::<LaunchLabels>(v).ok()) {
            read.tx.commit().await?;
            return Ok(v);
        }
        let lv = "coalesce(r.label_values, fastproof_label_values(r.labels)) as lv";
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("select k, count(*) from ");
        push_bounded(&mut qb, launch, f, lv);
        qb.push(" cross join jsonb_object_keys(r.lv) k where k <> '' group by 1");
        let names: Vec<(String, i64)> = qb.build_query_as().fetch_all(&mut *read.tx).await?;
        let mut qb: QueryBuilder<Postgres> =
            QueryBuilder::new("select n, count(*) from (select k.key as n, v from ");
        push_bounded(&mut qb, launch, f, lv);
        qb.push(
            " cross join jsonb_each(r.lv) k cross join jsonb_array_elements_text(k.value) v \
             where k.key <> '' and v <> '' group by 1, 2) d group by 1",
        );
        let values: HashMap<String, i64> = qb
            .build_query_as::<(String, i64)>()
            .fetch_all(&mut *read.tx)
            .await?
            .into_iter()
            .collect();
        let mut items: Vec<LabelName> = names
            .into_iter()
            .map(|(name, count)| LabelName {
                values: values.get(&name).copied().unwrap_or(0),
                name: truncate_chars(&name, MAX_FACET_NAME_CHARS),
                count,
            })
            .collect();
        items.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
        items.truncate(MAX_LABEL_NAMES as usize);
        let out = LaunchLabels {
            items,
            partial: partial(launch),
        };
        self.cache_done(launch, read, &out).await?;
        Ok(out)
    }

    /// Counts of the explorer filter values over a launch, within `f` (its environment).
    pub async fn launch_facets(
        &self,
        launch: &Launch,
        f: &TreeFilter,
        baseline: &Baseline,
    ) -> anyhow::Result<LaunchFacets> {
        let (mut read, mut hit) = self.cached_read(launch, None).await?;
        if let Some(e) = env_only(f).filter(|_| launch.status != "running") {
            let base = match baseline.launch {
                Some((id, _)) => {
                    serde_json::json!([id, crate::explorer::gen_of(&mut read.tx, id).await?])
                }
                None => serde_json::Value::Null,
            };
            let key = format!("facets2:{}", serde_json::json!([e, base]));
            read.generation = crate::explorer::gen_of(&mut read.tx, launch.id).await?;
            hit =
                crate::explorer::cache_get(&mut read.tx, launch.id, &key, read.generation).await?;
            read.key = Some(key);
        }
        if let Some(v) = hit.and_then(|v| serde_json::from_value::<LaunchFacets>(v).ok()) {
            read.tx.commit().await?;
            return Ok(v);
        }
        let out = facets_in(&mut read.tx, launch, f, baseline).await?;
        self.cache_done(launch, read, &out).await?;
        Ok(out)
    }

    /// The environments of the results of a launch (by the environment key of their series),
    /// with counts, largest first.
    pub async fn launch_environments(
        &self,
        launch: &Launch,
    ) -> anyhow::Result<Vec<EnvironmentCounts>> {
        let (mut read, hit) = self.cached_read(launch, Some("envs2".into())).await?;
        if let Some(v) = hit.and_then(|v| serde_json::from_value::<Vec<EnvironmentCounts>>(v).ok())
        {
            read.tx.commit().await?;
            return Ok(v);
        }
        let rows: Vec<(Option<String>, String, bool, bool, i64)> = sqlx::query_as(
            "select se.env_key, r.status, r.flaky_in_run, r.muted, count(*) from results r \
             join series se on se.id = r.series_id \
             where r.launch_id = $1 and r.launch_created_at = $2 group by 1, 2, 3, 4",
        )
        .bind(launch.id)
        .bind(launch.created_at)
        .fetch_all(&mut *read.tx)
        .await?;
        let mut by: BTreeMap<Option<String>, Counts> = BTreeMap::new();
        for (key, status, flaky, muted, n) in rows {
            add_counts(
                by.entry(key).or_default(),
                &counts_of(&status, flaky, muted, n),
            );
        }
        let mut out: Vec<EnvironmentCounts> = by
            .into_iter()
            .map(|(key, counts)| EnvironmentCounts { key, counts })
            .collect();
        out.sort_by(|a, b| b.counts.total.cmp(&a.counts.total).then(a.key.cmp(&b.key)));
        out.truncate(MAX_ENVIRONMENTS);
        self.cache_done(launch, read, &out).await?;
        Ok(out)
    }
}

/// Characters of a label name or value in the label names and facets, at most.
pub const MAX_FACET_NAME_CHARS: usize = 1024;

async fn facets_in(
    conn: &mut sqlx::PgConnection,
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
    qb.push(") from ");
    push_bounded(&mut qb, launch, f, "r.*");
    let row = qb.build().fetch_one(&mut *conn).await?;
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

    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("select n.name, v.x, count(*) from ");
    push_bounded(
        &mut qb,
        launch,
        f,
        "coalesce(r.label_values, fastproof_label_values(r.labels)) as lv",
    );
    qb.push(" cross join unnest(")
        .push_bind(FACET_LABELS.map(str::to_string).to_vec())
        .push(
            "::text[]) as n(name) cross join lateral (select distinct x from \
             (select x from jsonb_array_elements_text(r.lv -> n.name) x limit ",
        )
        .push_bind(MAX_FACET_LABELS)
        .push(") y where x <> '') v group by 1, 2 order by 3 desc, 2 limit ")
        .push_bind((MAX_FACET_VALUES * FACET_LABELS.len() * 4) as i64);
    let rows: Vec<(String, String, i64)> = qb.build_query_as().fetch_all(&mut *conn).await?;
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
                l.values.push(ValueCount {
                    value: truncate_chars(&value, MAX_FACET_NAME_CHARS),
                    count,
                });
            } else {
                l.more = true;
            }
        }
    }

    let mut qb: QueryBuilder<Postgres> =
        QueryBuilder::new("select r.defect->>'typeId', count(*) from ");
    push_bounded(&mut qb, launch, f, "r.defect");
    qb.push(" where r.defect is not null group by 1 order by 2 desc limit 200");
    let rows: Vec<(Option<String>, i64)> = qb.build_query_as().fetch_all(&mut *conn).await?;
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
        partial: partial(launch),
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

    fn place1(
        by: &GroupBy,
        suite_path: &[String],
        labels: &[LabelView],
        full_name: &str,
    ) -> (Vec<String>, bool) {
        place(by, &PlaceKey::of(by, suite_path, labels, full_name)).swap_remove(0)
    }

    #[test]
    fn suite_paths_by_format() {
        let pw = "e2e/checkout.spec.ts \u{203a} Checkout \u{203a} Card \u{203a} pays";
        assert_eq!(
            place1(&GroupBy::Suite, &["e2e/checkout.spec".into()], &[], pw),
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
            place1(
                &GroupBy::Suite,
                &["com.example.CheckoutTest".into()],
                &labels(&[("suite", "com.example.CheckoutTest")]),
                "com.example.CheckoutTest#pays"
            )
            .0,
            vec!["com.example", "CheckoutTest"]
        );
        assert_eq!(
            place1(
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
            place1(
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
            place1(
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
    fn cache_keys_tell_groupings_and_environments_apart() {
        let none = TreeFilter::default();
        let env = |k: &str| TreeFilter {
            env: Some(EnvFilter::Key(k.into())),
            ..TreeFilter::default()
        };
        let a = tree_cache_key(&GroupBy::Labels(vec!["a:k".into()]), &none).unwrap();
        let b = tree_cache_key(&GroupBy::Labels(vec!["a".into()]), &env("all")).unwrap();
        let c = tree_cache_key(&GroupBy::Labels(vec!["a".into()]), &env("k:all")).unwrap();
        let d = tree_cache_key(
            &GroupBy::Labels(vec!["a".into()]),
            &TreeFilter {
                env: Some(EnvFilter::None),
                ..TreeFilter::default()
            },
        )
        .unwrap();
        let e = tree_cache_key(&GroupBy::Labels(vec!["a".into()]), &env("none")).unwrap();
        let f = tree_cache_key(&GroupBy::Labels(vec!["a,b".into()]), &none).unwrap();
        let g = tree_cache_key(&GroupBy::Labels(vec!["a".into(), "b".into()]), &none).unwrap();
        let all = [&a, &b, &c, &d, &e, &f, &g];
        let distinct: HashSet<&&String> = all.iter().collect();
        assert_eq!(distinct.len(), all.len(), "{all:?}");
        assert!(
            tree_cache_key(
                &GroupBy::Suite,
                &TreeFilter {
                    retried: true,
                    ..TreeFilter::default()
                }
            )
            .is_none()
        );
    }

    #[test]
    fn inline_prefers_reveal_then_failures_then_small() {
        let big = MAX_TREE_INLINE - 10;
        let groups = vec![
            (vec!["big".to_string()], big as i64, false, false),
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
            vec!["Later", "Cart"],
            "an empty value is missing"
        );
        let blank = labels(&[("epic", ""), ("feature", "Cart")]);
        assert_eq!(group_path(&by, &[], &blank, "x"), vec![NONE_GROUP, "Cart"]);
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
