//! Import of requirements from an issue tracker (the `requirements` job): a dry run reads the
//! issues a query finds in the tracker's project through the tracker client (its secret, the
//! egress policy, no redirects), keeps them cleaned in the job and counts what would change;
//! the apply creates or updates requirements matched by tracker and issue key. Nothing is
//! written to the tracker.

use fastproof_store::tm::ImportReport;
use fastproof_store::tm::TmJobProgress;
use fastproof_store::tm::TmJobStatus;
use fastproof_store::tm::cases::CaseCtx;
use fastproof_store::tm::import::ClaimedJob;
use fastproof_store::tm::requirements::{
    ImportedRequirement, MAX_IMPORT_ISSUES, MAX_IMPORT_QUERY_BYTES, RequirementImportParams,
    clean_import,
};
use franshub_http::{ApiError, ApiResult};
use uuid::Uuid;

use super::settings::{IntegrationSettings, IssueTracker};
use super::trackers::Client;
use crate::state::AppState;

/// Checks the text of a tracker query: 1 to [`MAX_IMPORT_QUERY_BYTES`] bytes without control
/// characters other than line breaks. Returns it trimmed.
pub fn check_query(q: &str) -> ApiResult<String> {
    let t = q.trim();
    if t.is_empty() || t.len() > MAX_IMPORT_QUERY_BYTES {
        return Err(ApiError::bad_request(format!(
            "query must have 1 to {MAX_IMPORT_QUERY_BYTES} bytes"
        )));
    }
    if t.chars()
        .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
    {
        return Err(ApiError::bad_request(
            "query must not contain control characters",
        ));
    }
    Ok(t.to_string())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dialect {
    Jql,
    Wiql,
}

/// The words of a query outside quotes, brackets (WIQL) and parentheses, as byte ranges;
/// `None` when quotes, brackets or parentheses do not balance.
fn top_words(s: &str, d: Dialect) -> Option<Vec<(usize, usize)>> {
    let cs: Vec<(usize, char)> = s.char_indices().collect();
    let mut words = Vec::new();
    let mut depth = 0i32;
    let mut word: Option<usize> = None;
    let mut i = 0;
    while i < cs.len() {
        let (at, c) = cs[i];
        let is_word = c.is_alphanumeric() || c == '_';
        if !is_word && let Some(start) = word.take() {
            words.push((start, at));
        }
        match c {
            '"' | '\'' => {
                i += 1;
                loop {
                    let (_, x) = *cs.get(i)?;
                    if d == Dialect::Jql && x == '\\' {
                        i += 2;
                        continue;
                    }
                    if x == c {
                        if d == Dialect::Wiql && cs.get(i + 1).map(|p| p.1) == Some(c) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            '[' if d == Dialect::Wiql => loop {
                i += 1;
                if cs.get(i)?.1 == ']' {
                    break;
                }
            },
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            _ if is_word && depth == 0 && word.is_none() => word = Some(at),
            _ => {}
        }
        i += 1;
    }
    if let Some(start) = word {
        words.push((start, s.len()));
    }
    (depth == 0).then_some(words)
}

fn word_is(q: &str, w: (usize, usize), kw: &str) -> bool {
    q[w.0..w.1].eq_ignore_ascii_case(kw)
}

/// A JQL limited to the tracker's project: `project = "<key>" AND (<query>)`, with the
/// query's `ORDER BY` kept at the end.
fn jql_scoped(project: &str, q: &str) -> Result<String, String> {
    let words = top_words(q, Dialect::Jql).ok_or("the JQL has unbalanced quotes or parentheses")?;
    let order = (0..words.len().saturating_sub(1))
        .rev()
        .find(|&i| word_is(q, words[i], "order") && word_is(q, words[i + 1], "by"))
        .map(|i| words[i].0);
    let (cond, tail) = match order {
        Some(at) => (q[..at].trim(), q[at..].trim()),
        None => (q.trim(), ""),
    };
    let key = project.replace('\\', "\\\\").replace('"', "\\\"");
    let mut out = format!("project = \"{key}\"");
    if !cond.is_empty() {
        out.push_str(&format!(" AND ({cond})"));
    }
    if !tail.is_empty() {
        out.push(' ');
        out.push_str(tail);
    }
    Ok(out)
}

/// A WIQL limited to the project of the request URL: `SELECT [System.Id] FROM WorkItems WHERE
/// [System.TeamProject] = @project AND (<conditions>)`, with the query's `ORDER BY` and `ASOF`
/// kept. Only flat work item queries are accepted.
fn wiql_scoped(q: &str) -> Result<String, String> {
    const SHAPE: &str = "the WIQL must be SELECT … FROM WorkItems [WHERE …] [ORDER BY …] [ASOF …]";
    let words = top_words(q, Dialect::Wiql)
        .ok_or("the WIQL has unbalanced quotes, brackets or parentheses")?;
    if words.first().is_none_or(|w| !word_is(q, *w, "select")) {
        return Err(SHAPE.into());
    }
    let from = (1..words.len())
        .find(|&i| word_is(q, words[i], "from"))
        .ok_or(SHAPE)?;
    if words
        .get(from + 1)
        .is_none_or(|w| !word_is(q, *w, "workitems"))
    {
        return Err(SHAPE.into());
    }
    let after = words[from + 1].1;
    let rest = from + 2..words.len();
    let where_i = rest.clone().find(|&i| word_is(q, words[i], "where"));
    let order_i = rest.clone().find(|&i| {
        word_is(q, words[i], "order") && words.get(i + 1).is_some_and(|w| word_is(q, *w, "by"))
    });
    let asof_i = rest.clone().find(|&i| word_is(q, words[i], "asof"));
    let tail_at = [order_i, asof_i]
        .into_iter()
        .flatten()
        .map(|i| words[i].0)
        .min()
        .unwrap_or(q.len());
    let head_end = where_i.map(|i| words[i].0).unwrap_or(tail_at);
    if head_end > tail_at || !q[after..head_end].trim().is_empty() {
        return Err(SHAPE.into());
    }
    let cond = where_i.map(|i| q[words[i].1..tail_at].trim()).unwrap_or("");
    let mut out =
        "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project".to_string();
    if !cond.is_empty() {
        out.push_str(&format!(" AND ({cond})"));
    }
    let tail = q[tail_at..].trim();
    if !tail.is_empty() {
        out.push(' ');
        out.push_str(tail);
    }
    Ok(out)
}

/// The query sent to the tracker, limited to the tracker's configured project so a query can
/// neither read nor count issues of other projects (the service account may see more): JQL
/// and WIQL are wrapped, a GitLab or GitHub label is read through the project's own issue
/// list. A query that cannot be limited is an error (400 at the request).
pub fn scoped_query(tracker: &IssueTracker, q: &str) -> Result<String, String> {
    let project = tracker
        .api
        .as_ref()
        .map(|a| a.project.as_str())
        .unwrap_or_default();
    match tracker.tracker.as_str() {
        "jira" => jql_scoped(project, q),
        "azure" => wiql_scoped(q),
        _ => Ok(q.to_string()),
    }
}

async fn tracker_of(
    st: &AppState,
    project_id: Uuid,
    id: &str,
) -> anyhow::Result<Option<IssueTracker>> {
    let doc = st.store.project_settings_json(project_id).await?;
    let settings = IntegrationSettings::parse(&doc).unwrap_or_default();
    Ok(IssueTracker::connected(&settings.issue_trackers, id).cloned())
}

/// The tracker must be in the project settings with `api`, and the query must be one that
/// can be limited to its project (else 400).
pub async fn check_tracker(
    st: &AppState,
    project_id: Uuid,
    id: &str,
    query: &str,
) -> ApiResult<()> {
    match tracker_of(st, project_id, id).await? {
        Some(t) => scoped_query(&t, query)
            .map(|_| ())
            .map_err(ApiError::bad_request),
        None => Err(ApiError::bad_request(
            "trackerId names no issue tracker with api settings in this project",
        )),
    }
}

fn failed(code: &str, message: String) -> ImportReport {
    ImportReport {
        errors: vec![fastproof_store::tm::ImportRowError {
            row: 0,
            code: code.into(),
            message,
        }],
        ..Default::default()
    }
}

/// Runs one step of a `requirements` job: the dry run (`phase: dryRun`) or the apply.
pub async fn run(st: &AppState, job: &ClaimedJob, owner: &str) -> anyhow::Result<()> {
    let params: RequirementImportParams = serde_json::from_value(job.params.clone())?;
    let by = job.created_by.clone().unwrap_or_else(|| "import".into());
    let ctx = CaseCtx {
        project_id: job.project_id,
        project_key: &job.project_key,
        by: &by,
        limits: &st.cfg.tm,
        can_review: false,
    };
    if params.phase == "apply" {
        let report = st
            .store
            .apply_requirement_items(
                &ctx,
                &params.tracker_id,
                params.parent_number,
                &params.items,
            )
            .await?;
        let n = params.items.len() as i64;
        st.store
            .finish_tm_job(
                job.id,
                owner,
                &job.project_key,
                TmJobStatus::Done,
                TmJobProgress {
                    done: n,
                    total: Some(n),
                },
                Some(&report),
                Some(serde_json::json!({"items": []})),
            )
            .await?;
        return Ok(());
    }
    let Some(tracker) = tracker_of(st, job.project_id, &params.tracker_id).await? else {
        let report = failed(
            "tracker_not_configured",
            "the tracker is no longer in the project settings".into(),
        );
        st.store
            .finish_tm_job(
                job.id,
                owner,
                &job.project_key,
                TmJobStatus::Failed,
                job.progress,
                Some(&report),
                None,
            )
            .await?;
        return Ok(());
    };
    let read = match scoped_query(&tracker, &params.query) {
        Err(e) => Err(super::trackers::TrackerError::Config(e)),
        Ok(q) => match Client::new(st, job.project_id, &tracker).await {
            Ok(c) => c.search(&q, MAX_IMPORT_ISSUES + 1).await,
            Err(e) => Err(e),
        },
    };
    let (found, other) = match read {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(project = %job.project_key, tracker = %params.tracker_id, error = %e,
                "reading requirements from a tracker failed");
            let report = failed("tracker_error", e.public());
            st.store
                .finish_tm_job(
                    job.id,
                    owner,
                    &job.project_key,
                    TmJobStatus::Failed,
                    job.progress,
                    Some(&report),
                    None,
                )
                .await?;
            return Ok(());
        }
    };
    let mut report = ImportReport::default();
    report.skip += other as i64;
    if other > 0 {
        report.warnings.push(fastproof_store::tm::ImportRowError {
            row: 0,
            code: "other_project".into(),
            message: format!("{other} issues of other projects were left out"),
        });
    }
    let items: Vec<ImportedRequirement> = found
        .into_iter()
        .map(|(s, description)| ImportedRequirement {
            issue_key: s.key,
            title: s.title,
            description,
            url: Some(s.url).filter(|u| !u.is_empty()),
            external_state: Some(s.state).filter(|x| !x.is_empty()),
        })
        .collect();
    let items = clean_import(&st.cfg.tm, items, &mut report);
    st.store
        .plan_requirement_import(
            job.project_id,
            &params.tracker_id,
            &items,
            st.cfg.tm.max_requirements,
            &mut report,
        )
        .await?;
    let n = items.len() as i64;
    st.store
        .finish_tm_job(
            job.id,
            owner,
            &job.project_key,
            TmJobStatus::Ready,
            TmJobProgress {
                done: n,
                total: Some(n),
            },
            Some(&report),
            Some(serde_json::json!({"items": items})),
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jql_is_limited_to_the_project() {
        assert_eq!(
            jql_scoped("SHOP", "labels = req").unwrap(),
            "project = \"SHOP\" AND (labels = req)"
        );
        assert_eq!(
            jql_scoped("SHOP", "type = Story or labels = req ORDER BY created DESC").unwrap(),
            "project = \"SHOP\" AND (type = Story or labels = req) ORDER BY created DESC"
        );
        assert_eq!(
            jql_scoped("SHOP", "order by rank").unwrap(),
            "project = \"SHOP\" order by rank"
        );
        assert_eq!(
            jql_scoped("SHOP", "summary ~ \"a ) OR (b \\\" order by\"").unwrap(),
            "project = \"SHOP\" AND (summary ~ \"a ) OR (b \\\" order by\")"
        );
        for bad in [
            "x) OR (project = HR",
            "labels = req) OR project = HR OR (labels = x",
            "summary ~ \"open",
            "(labels = req",
        ] {
            assert!(jql_scoped("SHOP", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn wiql_is_limited_to_the_project() {
        assert_eq!(
            wiql_scoped("SELECT [System.Id], [System.Title] FROM WorkItems WHERE [System.WorkItemType] = 'Epic' ORDER BY [System.Id]")
                .unwrap(),
            "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project AND \
             ([System.WorkItemType] = 'Epic') ORDER BY [System.Id]"
        );
        assert_eq!(
            wiql_scoped("select [System.Id] from workitems").unwrap(),
            "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project"
        );
        assert_eq!(
            wiql_scoped("SELECT [Id] FROM WorkItems WHERE [System.Title] CONTAINS 'it''s ) OR (x' ASOF '2026-01-01'")
                .unwrap(),
            "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project AND \
             ([System.Title] CONTAINS 'it''s ) OR (x') ASOF '2026-01-01'"
        );
        for bad in [
            "[System.TeamProject] = 'HR'",
            "SELECT [System.Id] FROM WorkItemLinks WHERE [Source].[System.Id] > 0",
            "SELECT [System.Id] FROM WorkItems WHERE [System.Id] > 0) OR ([System.TeamProject] = 'HR'",
            "SELECT [System.Id] FROM WorkItems, Other WHERE [System.Id] > 0",
            "SELECT [System.Id] FROM WorkItems WHERE [System.Title] = 'open",
            "SELECT [System.Id] FROM WorkItems ORDER BY [System.Id] WHERE [System.Id] > 0",
        ] {
            assert!(wiql_scoped(bad).is_err(), "{bad}");
        }
    }
}
