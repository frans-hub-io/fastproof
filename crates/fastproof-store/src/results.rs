//! Reading results: lists, the launch tree, and the detail with attempts, steps and fixtures.

use std::collections::{BTreeMap, HashMap};

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
    TreeNode, codec, json_no_nul, no_nul, ts_ms,
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

type TreeRow = (
    Uuid,
    String,
    Vec<String>,
    serde_json::Value,
    String,
    bool,
    bool,
);

/// Fixture steps attached to one attempt at read time, at most.
pub const MAX_FIXTURE_STEPS: usize = 10_000;
/// Largest tree the tree endpoint builds.
pub const MAX_TREE_RESULTS: i64 = 200_000;

const SUMMARY_SELECT: &str = "select r.id, r.launch_id, r.launch_created_at, r.test_case_id, r.series_id, \
    s.series_key, r.name, r.full_name, r.suite_path, r.status, r.flaky_in_run, r.attempt_count, r.duration_ms, \
    r.started_at, r.labels, r.muted, r.fingerprint, r.defect, r.message \
    from results r join series s on s.id = r.series_id ";

fn summary_row(r: &PgRow) -> Result<ResultSummary, sqlx::Error> {
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
        fingerprint: r.try_get("fingerprint")?,
        defect: defect.and_then(|d| serde_json::from_value::<Defect>(d).ok()),
        message: r
            .try_get::<Option<String>, _>("message")?
            .map(|m| truncate_chars(&m, MESSAGE_CHARS)),
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
    children: BTreeMap<String, Builder>,
    ids: Vec<Uuid>,
}

impl Builder {
    fn into_node(self, name: String, path: Vec<String>) -> TreeNode {
        let children = self
            .children
            .into_iter()
            .map(|(n, b)| {
                let mut p = path.clone();
                p.push(n.clone());
                b.into_node(n, p)
            })
            .collect();
        TreeNode {
            name,
            path,
            counts: self.counts,
            children,
            result_ids: (!self.ids.is_empty()).then_some(self.ids),
        }
    }
}

fn label<'a>(labels: &'a [LabelView], name: &str) -> Option<&'a str> {
    labels
        .iter()
        .find(|l| l.name == name)
        .map(|l| l.value.as_str())
        .filter(|v| !v.is_empty())
}

fn group_path(
    by: GroupBy,
    suite_path: &[String],
    labels: &[LabelView],
    full_name: &str,
) -> Vec<String> {
    match by {
        GroupBy::Suite => suite_path.to_vec(),
        GroupBy::Feature => ["epic", "feature", "story"]
            .iter()
            .filter_map(|n| label(labels, n))
            .map(str::to_string)
            .collect(),
        GroupBy::Package => {
            let base = label(labels, "package")
                .map(str::to_string)
                .unwrap_or_else(|| {
                    let cut = full_name.rfind('#').or_else(|| full_name.rfind('.'));
                    cut.map(|i| full_name[..i].to_string()).unwrap_or_default()
                });
            base.split(['.', '/'])
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        }
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
        Ok(Page { items, next })
    }

    /// The tree of a launch: groups with counts, leaf groups carry result ids.
    pub async fn tree(&self, launch: &Launch, by: GroupBy) -> anyhow::Result<TreeNode> {
        let rows: Vec<TreeRow> = sqlx::query_as(
            "select id, full_name, suite_path, labels, status, flaky_in_run, muted from results \
             where launch_id = $1 and launch_created_at = $2 order by full_name, id limit $3",
        )
        .bind(launch.id)
        .bind(launch.created_at)
        .bind(MAX_TREE_RESULTS)
        .fetch_all(&self.pool)
        .await?;
        let mut root = Builder::default();
        for (id, full_name, suite, labels, status, flaky, muted) in rows {
            let labels: Vec<LabelView> = serde_json::from_value(labels).unwrap_or_default();
            let path = group_path(by, &suite, &labels, &full_name);
            add_count(&mut root.counts, &status, flaky, muted);
            let mut node = &mut root;
            for seg in path {
                node = node.children.entry(seg).or_default();
                add_count(&mut node.counts, &status, flaky, muted);
            }
            node.ids.push(id);
        }
        Ok(root.into_node(String::new(), Vec::new()))
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
        );
        let mut rows: Vec<AttemptRow> = sqlx::query_as(
            "select id, status, started_at, stopped_at, duration_ms, message, trace, source_uuid, trace_id, detail, \
             steps_z, logs_z, seq from attempts where result_id = $1 and launch_created_at = $2 \
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
        Ok(Some(ResultDetail {
            launch_number,
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
