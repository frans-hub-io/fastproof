//! Manual runs: run items of a launch, my queue, the executed case version, step results,
//! status changes (projected into results), reruns, attachments, defects, new-issue links,
//! closing and reopening the manual run, and the manual report.

use axum::Json;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use fastproof_store::analysis::triage::{TriageIssue, TriageRequest};
use fastproof_store::tm::runs::{
    ManualReportItem, MyRunItem, RunItemBrief, RunItemFilter, step_at,
};
use fastproof_store::tm::{
    CaseBody, ManualClose, ManualState, RunDefect, RunItem, RunItemExecution, RunItemPatch,
    RunItemSummary, RunItemsAdd, RunItemsAdded, RunStatus, RunStepAck, RunStepWrite, StepStatus,
};
use fastproof_store::{Launch, Page, Project};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::attachments::AttachmentCreated;
use super::plans::{assignee_filter, etag, if_match};
use super::{ErrorBody, P, Q, page_limit, tm_error};
use crate::integrations::settings::IntegrationSettings;
use crate::state::AppState;

/// Bytes of a manual report, at most.
pub const MAX_REPORT_BYTES: usize = 64 * 1024 * 1024;
/// Items read per page while building a manual report.
const REPORT_PAGE: i64 = 500;
/// Bytes of a new-issue URL, at most (browsers and trackers cut longer ones).
pub const MAX_ISSUE_URL_BYTES: usize = 8000;

/// The routes of this module, mounted once by `crate::app::api_router`.
pub fn router() -> OpenApiRouter<AppState> {
    let uploads = OpenApiRouter::<AppState>::new()
        .routes(routes!(upload_run_attachment))
        .layer(DefaultBodyLimit::disable());
    OpenApiRouter::new()
        .routes(routes!(add_run_items, list_run_items))
        .routes(routes!(my_run_items))
        .routes(routes!(get_run_item, patch_run_item))
        .routes(routes!(put_run_step))
        .routes(routes!(run_item_defect))
        .routes(routes!(run_item_new_issue_url))
        .routes(routes!(rerun_run_item))
        .routes(routes!(run_item_executions))
        .routes(routes!(project_people))
        .routes(routes!(close_manual_run))
        .routes(routes!(reopen_manual_run))
        .routes(routes!(manual_report))
        .merge(uploads)
}

/// One status or a comma-separated list.
fn run_statuses(v: Option<&str>) -> ApiResult<Vec<RunStatus>> {
    let Some(v) = v else {
        return Ok(Vec::new());
    };
    v.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(|x| {
            RunStatus::parse(x).ok_or_else(|| {
                ApiError::bad_request(
                    "status: untested, in_progress, passed, failed, blocked or skipped",
                )
            })
        })
        .collect()
}

async fn item_of(s: &AppState, p: &Project, id: Uuid) -> ApiResult<RunItemBrief> {
    s.store
        .run_item_brief(p.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("run item"))
}

/// The item with its Markdown rendered.
async fn rendered(s: &AppState, p: &Project, id: Uuid) -> ApiResult<RunItem> {
    let mut item = s.store.run_item(p.id, id).await.map_err(tm_error)?;
    let html = crate::markdown::render_body(
        &CaseBody {
            description: String::new(),
            preconditions: item.preconditions.clone(),
            steps: item.steps.clone(),
            links: Vec::new(),
        },
        &p.key,
    );
    item.preconditions_html = html.preconditions;
    item.steps_html = html.steps;
    Ok(item)
}

fn item_response(item: RunItem) -> Response {
    let rev = item.revision;
    let mut r = Json(item).into_response();
    r.headers_mut().insert(header::ETAG, etag(rev));
    r
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/{id}/run-items", tag = "runs", operation_id = "add_run_items",
    description = "Adds run items for the manual cases among `caseNumbers` (at most `maxBulk`) or of a managed plan, and \
        opens the manual run of the launch. Automated cases (CI runs them) and cases already in the launch are skipped; \
        `skips` names them with the codes `automated` and `in_launch`.",
    request_body = RunItemsAdd,
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = RunItemsAdded), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`manual_run_closed`, `limit_reached`")))]
pub async fn add_run_items(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<RunItemsAdd>,
) -> ApiResult<Json<RunItemsAdded>> {
    let p = s.project_for(&auth, &key, &s.perms.runs_execute).await?;
    crate::tm_rate::admit(&s, &auth)?;
    let _permit = s.guards.heavy(&auth).await?;
    let out = s
        .store
        .add_run_items(&p, id, &body, &s.cfg.tm)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "run.start",
        Some(&p.key),
        serde_json::json!({"launchId": id, "planId": body.plan_id, "added": out.added, "skipped": out.skipped}),
    )
    .await;
    Ok(Json(out))
}

/// Query of `GET /launches/{id}/run-items`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RunItemsQuery {
    /// One status or a comma-separated list.
    pub status: Option<String>,
    /// `me`, `none` (unassigned) or a name.
    pub assignee: Option<String>,
    /// Cases in this folder or its subfolders.
    pub folder: Option<Uuid>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/run-items", tag = "runs", operation_id = "list_run_items",
    description = "Run items of a launch by case number.",
    params(("key" = String, Path), ("id" = Uuid, Path), RunItemsQuery),
    responses((status = 200, body = Page<RunItemSummary>), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_run_items(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<RunItemsQuery>,
) -> ApiResult<Json<Page<RunItemSummary>>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    s.launch_of(&p, id).await?;
    let f = RunItemFilter {
        statuses: run_statuses(q.status.as_deref())?,
        assignee: assignee_filter(q.assignee.as_deref(), &auth.principal.name)?,
        folder: q.folder,
        before: q.before,
        limit: page_limit(q.limit)?,
    };
    Ok(Json(
        s.store.run_items(p.id, id, &f).await.map_err(tm_error)?,
    ))
}

/// Query of `GET /run-items`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MyRunItemsQuery {
    /// `me` (the default and the only value).
    pub assignee: Option<String>,
    /// One status or a comma-separated list (default `untested,in_progress`).
    pub status: Option<String>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/run-items", tag = "runs", operation_id = "my_run_items",
    description = "My queue: run items assigned to the caller in open manual runs of the project.",
    params(("key" = String, Path), MyRunItemsQuery),
    responses((status = 200, body = Page<MyRunItem>), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn my_run_items(
    State(s): State<AppState>,
    auth: Auth,
    headers: axum::http::HeaderMap,
    P(key): P<String>,
    Q(q): Q<MyRunItemsQuery>,
) -> ApiResult<Json<Page<MyRunItem>>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    crate::tm_people::note(&s, &auth, &headers);
    if q.assignee.as_deref().is_some_and(|a| a != "me") {
        return Err(ApiError::bad_request("assignee must be me"));
    }
    let mut statuses = run_statuses(q.status.as_deref())?;
    if statuses.is_empty() {
        statuses = vec![RunStatus::Untested, RunStatus::InProgress];
    }
    Ok(Json(
        s.store
            .my_run_items(
                p.id,
                &auth.principal.name,
                &statuses,
                q.before.as_deref(),
                page_limit(q.limit)?,
            )
            .await
            .map_err(tm_error)?,
    ))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/run-items/{itemId}", tag = "runs", operation_id = "get_run_item",
    description = "A run item with the executed version of its case (Markdown and sanitized HTML), its step results and \
        `ETag: \"r<revision>\"`.",
    params(("key" = String, Path), ("itemId" = Uuid, Path)),
    responses((status = 200, body = RunItem), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_run_item(
    State(s): State<AppState>,
    auth: Auth,
    headers: axum::http::HeaderMap,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    crate::tm_people::note(&s, &auth, &headers);
    Ok(item_response(rendered(&s, &p, id).await?))
}

#[utoipa::path(put, path = "/api/v1/projects/{key}/run-items/{itemId}/steps/{path}", tag = "runs", operation_id = "put_run_step",
    description = "Writes the result of one step (`path` = `1`, `1.2`, `1.2.3` over the executed version). No `If-Match`: \
        the last writer wins, and writing the same value again is harmless. Absent `actual`, `comment` and \
        `attachmentIds` keep their values. An untested item becomes `in_progress`. `suggestedStatus` is the item status \
        the step results suggest.",
    request_body = RunStepWrite,
    params(("key" = String, Path), ("itemId" = Uuid, Path), ("path" = String, Path)),
    responses((status = 200, body = RunStepAck), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`manual_run_closed`, `limit_reached`")))]
pub async fn put_run_step(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id, path)): P<(String, Uuid, String)>,
    ApiJson(body): ApiJson<RunStepWrite>,
) -> ApiResult<Json<RunStepAck>> {
    let p = s.project_for(&auth, &key, &s.perms.runs_execute).await?;
    Ok(Json(
        s.store
            .put_run_step(&p, id, &path, &body, &auth.principal.name, &s.cfg.tm)
            .await
            .map_err(tm_error)?,
    ))
}

#[utoipa::path(patch, path = "/api/v1/projects/{key}/run-items/{itemId}", tag = "runs", operation_id = "patch_run_item",
    description = "Changes the status, comment or assignee of a run item (needs `If-Match`). A final status (`passed`, \
        `failed`, `blocked`, `skipped`) is projected into the launch results (`blocked` as `broken`); back to `untested` \
        or `in_progress` removes the projected attempt. `untestedStepsTo: skipped` marks the untested steps skipped.",
    request_body = RunItemPatch,
    params(("key" = String, Path), ("itemId" = Uuid, Path), ("If-Match" = String, Header, description = "ETag of the item")),
    responses((status = 200, body = RunItem), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`manual_run_closed`"),
        (status = 412, body = ErrorBody), (status = 428, body = ErrorBody)))]
pub async fn patch_run_item(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<RunItemPatch>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.runs_execute).await?;
    let revision = if_match(&headers)?;
    let ch = s
        .store
        .patch_run_item(&p, id, revision, &body, &auth.principal.name, &s.cfg.tm)
        .await
        .map_err(tm_error)?;
    if (ch.new_status != ch.old_status && ch.new_status.is_final()) || ch.assignee_changed {
        s.audit(
            &auth,
            "runItem.update",
            Some(&p.key),
            serde_json::json!({"launchId": ch.launch_id, "id": id, "case": format!("TC-{}", ch.case_number),
                "from": ch.old_status, "status": ch.new_status,
                "assignee": body.assignee.as_ref().filter(|_| ch.assignee_changed)}),
        )
        .await;
    }
    Ok(item_response(rendered(&s, &p, id).await?))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/run-items/{itemId}/attachments", tag = "runs", operation_id = "upload_run_attachment",
    description = "Stores a file for a step result (raw body, at most `maxAttachmentBytes`; name in \
        `X-FastProof-Attachment-Name`), content-addressed like launch attachments. Reference it in `attachmentIds` of \
        a step write.",
    params(("key" = String, Path), ("itemId" = Uuid, Path),
        ("X-FastProof-Attachment-Name" = Option<String>, Header, description = "file name")),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses((status = 201, body = AttachmentCreated), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`manual_run_closed`"), (status = 413, body = ErrorBody)))]
pub async fn upload_run_attachment(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    req: Request,
) -> ApiResult<(StatusCode, Json<AttachmentCreated>)> {
    let p = s.project_for(&auth, &key, &s.perms.runs_execute).await?;
    let item = item_of(&s, &p, id).await?;
    if item.manual_state == Some(ManualState::Closed) {
        return Err(tm_error(fastproof_store::tm::TmError::conflict(
            "manual_run_closed",
            "the manual run is closed; reopen it first",
        )));
    }
    let created = super::attachments::store_attachment(
        &s,
        &p,
        item.launch_id,
        req,
        "maxAttachmentBytes",
        s.cfg.tm.max_attachment_bytes,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(created)))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/run-items/{itemId}/defect", tag = "runs", operation_id = "run_item_defect",
    description = "Triages the projected result of a run item (the same triage as `/triage`, `source: user`) and keeps the \
        defect on the item. Holders of `results:triage` may set it on any item; holders of `runs:execute` only on items \
        they executed (the executor of the current execution; the assignee does not count). Otherwise 403 with \
        `details {required: \"results:triage\", scope}`.",
    request_body = RunDefect,
    params(("key" = String, Path), ("itemId" = Uuid, Path)),
    responses((status = 200, body = RunItem), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`run_item_not_final`")))]
pub async fn run_item_defect(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<RunDefect>,
) -> ApiResult<Response> {
    let triager = auth.principal.allowed(&s.perms.results_triage, Some(&key));
    let perm = if triager {
        &s.perms.results_triage
    } else {
        &s.perms.runs_execute
    };
    let p = s.project_for(&auth, &key, perm).await?;
    let item = item_of(&s, &p, id).await?;
    let me = auth.principal.name.as_str();
    if !triager && item.executed_by.as_deref() != Some(me) {
        return Err(ApiError::forbidden(
            "a defect can be set only on run items you executed (or with results:triage)",
        )
        .with_details(
            serde_json::json!({"required": s.perms.results_triage.as_str(), "scope": p.key}),
        ));
    }
    let Some(result_id) = item.result_id else {
        return Err(tm_error(fastproof_store::tm::TmError::conflict(
            "run_item_not_final",
            "the run item has no final status yet",
        )));
    };
    let req = TriageRequest {
        result_ids: Some(vec![result_id]),
        defect_type_id: body.defect_type_id,
        comment: body.comment.clone(),
        issue: body.issue.as_ref().map(|i| TriageIssue {
            url: Some(i.url.clone()),
            key: i.key.clone(),
        }),
        source: Some(
            if crate::mcp_tm::is_agent(&s, &auth, &p.key) {
                "ai"
            } else {
                "user"
            }
            .into(),
        ),
        ..Default::default()
    };
    let (out, req) = super::analysis::apply_triage(&s, &p, me, req).await?;
    if out.updated > 0 {
        s.store
            .sync_run_item_defect(&p, id)
            .await
            .map_err(tm_error)?;
    }
    s.audit(
        &auth,
        "runItem.update",
        Some(&p.key),
        serde_json::json!({"launchId": item.launch_id, "id": id, "case": format!("TC-{}", item.case_number),
            "defect": {"defectTypeId": req.defect_type_id, "comment": req.comment.is_some(),
                "issue": req.issue.as_ref().and_then(|i| i.url.clone())}}),
    )
    .await;
    Ok(item_response(rendered(&s, &p, id).await?))
}

/// Query of `GET /run-items/{itemId}/new-issue-url`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct NewIssueQuery {
    /// `jira`, `gitlab` or `github` (default: the first configured issue tracker).
    pub tracker: Option<String>,
}

/// A prefilled "new issue" link.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewIssueUrl {
    pub url: String,
    /// Whether `url` itself carries the title and description. `false` for a Jira tracker
    /// without `projectId` and `issueTypeId`: `url` opens Jira's create page and the text is in
    /// `prefill` to copy.
    pub prefilled: bool,
    /// The issue text, for trackers whose link may not carry it (Jira).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefill: Option<NewIssuePrefill>,
}

/// Title and description of a new issue.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct NewIssuePrefill {
    pub title: String,
    pub description: String,
}

/// Jira ids that make `CreateIssueDetails!init.jspa` open a filled-in form.
#[derive(Clone, Copy, Debug, Default)]
pub struct JiraIds<'a> {
    pub project_id: Option<&'a str>,
    pub issue_type_id: Option<&'a str>,
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn cut(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// The new-issue URL of a tracker from its `{key}` URL template: GitHub and GitLab
/// `…/issues/new?title=&body=` (`issue[title]`, `issue[description]` on GitLab); Jira with a
/// project and issue type id `…/secure/CreateIssueDetails!init.jspa?pid=&issuetype=&summary=&description=`,
/// without them only Jira's create page `…/secure/CreateIssue!default.jspa` (the second value
/// is `false`: the text is not in the URL). The description is cut so the URL stays within
/// [`MAX_ISSUE_URL_BYTES`].
pub fn new_issue_url(
    tracker: &str,
    url_template: &str,
    title: &str,
    description: &str,
    jira: JiraIds<'_>,
) -> Option<(String, bool)> {
    if tracker == "jira" {
        let i = url_template.find("/browse/{key}")?;
        let root = &url_template[..i];
        let (Some(pid), Some(it)) = (jira.project_id, jira.issue_type_id) else {
            let url = format!("{root}/secure/CreateIssue!default.jspa");
            let parsed = url::Url::parse(&url).ok()?;
            return matches!(parsed.scheme(), "https" | "http").then_some((url, false));
        };
        let base = format!(
            "{root}/secure/CreateIssueDetails!init.jspa?pid={}&issuetype={}&",
            enc(pid),
            enc(it)
        );
        return fill(&base, "summary", "description", title, description).map(|u| (u, true));
    }
    let (base, t, d) = match tracker {
        "github" | "gitlab" => {
            let i = url_template.find("/issues/{key}")?;
            let base = format!("{}/issues/new?", &url_template[..i]);
            if tracker == "github" {
                (base, "title", "body")
            } else {
                (base, "issue%5Btitle%5D", "issue%5Bdescription%5D")
            }
        }
        _ => return None,
    };
    fill(&base, t, d, title, description).map(|u| (u, true))
}

fn fill(base: &str, t: &str, d: &str, title: &str, description: &str) -> Option<String> {
    let parsed = url::Url::parse(base).ok()?;
    if !matches!(parsed.scheme(), "https" | "http") {
        return None;
    }
    let head = format!("{base}{t}={}&{d}=", enc(&cut(title, 200)));
    let mut n = description.chars().count();
    loop {
        let url = format!("{head}{}", enc(&cut(description, n)));
        if url.len() <= MAX_ISSUE_URL_BYTES || n == 0 {
            return (url.len() <= MAX_ISSUE_URL_BYTES).then_some(url);
        }
        n = n * 3 / 4;
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/run-items/{itemId}/new-issue-url", tag = "runs", operation_id = "run_item_new_issue_url",
    description = "A link that opens a prefilled new issue in a configured tracker (`issueTrackers` settings) with the \
        case, the failed step, expected and actual results and a link back to the run item. Nothing is sent to the tracker. \
        Jira: with `projectId` and `issueTypeId` in the tracker settings the link opens a filled-in create form \
        (`CreateIssueDetails!init.jspa`, which Jira 9+ may disable); without them it opens Jira's create page and \
        `prefilled` is false. Jira answers always carry the text in `prefill` for copying.",
    params(("key" = String, Path), ("itemId" = Uuid, Path), NewIssueQuery),
    responses((status = 200, body = NewIssueUrl), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`no_issue_tracker`")))]
pub async fn run_item_new_issue_url(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<NewIssueQuery>,
) -> ApiResult<Json<NewIssueUrl>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    if q.tracker
        .as_deref()
        .is_some_and(|t| !matches!(t, "jira" | "gitlab" | "github"))
    {
        return Err(ApiError::bad_request(
            "tracker must be jira, gitlab or github",
        ));
    }
    let item = s.store.run_item(p.id, id).await.map_err(tm_error)?;
    let doc = s.store.project_settings_json(p.id).await?;
    let settings = IntegrationSettings::parse(&doc).unwrap_or_default();
    let no_tracker = || {
        tm_error(fastproof_store::tm::TmError::conflict(
            "no_issue_tracker",
            "no matching issue tracker with an `/issues/{key}` or `/browse/{key}` URL template is configured",
        ))
    };
    let tracker = settings
        .issue_trackers
        .iter()
        .find(|t| q.tracker.as_deref().is_none_or(|w| w == t.tracker))
        .ok_or_else(no_tracker)?;
    let launch = s.launch_of(&p, item.launch_id).await?;
    let failed = item
        .step_results
        .iter()
        .find(|r| matches!(r.status, StepStatus::Failed | StepStatus::Blocked));
    let case = format!("TC-{} {}", item.summary.case_number, item.summary.name);
    let title = match failed {
        Some(f) => format!("{case}: step {} {}", f.path, f.status),
        None => format!("{case}: {}", item.summary.status),
    };
    let mut d = format!(
        "Case: {case} (version {})\nLaunch: #{} {}\nStatus: {}\n",
        item.case_version, launch.number, launch.name, item.summary.status
    );
    if let Some(f) = failed {
        let step = step_at(&item.steps, &f.path);
        d.push_str(&format!(
            "\nStep {}: {}\nExpected: {}\nActual: {}\n",
            f.path,
            step.map(|s| s.action.as_str()).unwrap_or_default(),
            step.map(|s| s.expected.as_str()).unwrap_or_default(),
            f.actual
        ));
        if !f.comment.is_empty() {
            d.push_str(&format!("Comment: {}\n", f.comment));
        }
    }
    if !item.comment.is_empty() {
        d.push_str(&format!("\n{}\n", item.comment));
    }
    let back = crate::integrations::dispatch::page_url(
        &s,
        &format!("p/{}/launches/{}/run/{}", p.key, item.launch_id, id),
    );
    if let Some(b) = back {
        d.push_str(&format!("\nRun item: {b}\n"));
    }
    let jira = JiraIds {
        project_id: tracker.project_id.as_deref(),
        issue_type_id: tracker.issue_type_id.as_deref(),
    };
    let (url, prefilled) = new_issue_url(&tracker.tracker, &tracker.url_template, &title, &d, jira)
        .ok_or_else(no_tracker)?;
    let prefill = (tracker.tracker == "jira").then(|| NewIssuePrefill {
        title: cut(&title, 200),
        description: d.clone(),
    });
    Ok(Json(NewIssueUrl {
        url,
        prefilled,
        prefill,
    }))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/run-items/{itemId}/rerun", tag = "runs", operation_id = "rerun_run_item",
    description = "Starts a new execution of the item (`executionNo + 1`, `untested`, no step results; needs the item's \
        `If-Match`). The previous execution (status, comment, executor, times, defect, step results with their \
        attachments) is kept outside the result partitions and listed by `GET /run-items/{itemId}/executions`; its \
        attempt stays in the result's attempts. Audited as `runItem.rerun`.",
    params(("key" = String, Path), ("itemId" = Uuid, Path), ("If-Match" = String, Header, description = "ETag of the item")),
    responses((status = 200, body = RunItem), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`manual_run_closed`"), (status = 412, body = ErrorBody),
        (status = 428, body = ErrorBody)))]
pub async fn rerun_run_item(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.runs_execute).await?;
    let revision = if_match(&headers)?;
    let ch = s
        .store
        .rerun_run_item(&p, id, revision, &auth.principal.name)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "runItem.rerun",
        Some(&p.key),
        serde_json::json!({"launchId": ch.launch_id, "id": id, "case": format!("TC-{}", ch.case_number),
            "from": ch.old_status, "executionNo": ch.execution_no}),
    )
    .await;
    Ok(item_response(rendered(&s, &p, id).await?))
}

/// Query of `GET /run-items/{itemId}/executions`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExecutionsQuery {
    pub before: Option<String>,
    /// At most 100 (default 50).
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/run-items/{itemId}/executions", tag = "runs", operation_id = "list_run_item_executions",
    description = "Earlier executions of a run item (kept on rerun), newest first, with their step results.",
    params(("key" = String, Path), ("itemId" = Uuid, Path), ExecutionsQuery),
    responses((status = 200, body = Page<RunItemExecution>), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn run_item_executions(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<ExecutionsQuery>,
) -> ApiResult<Json<Page<RunItemExecution>>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let limit = match q.limit {
        None => 50,
        Some(n) if (1..=fastproof_store::tm::runs::MAX_EXECUTIONS_PAGE).contains(&n) => n,
        Some(_) => return Err(ApiError::bad_request("limit must be 1..=100")),
    };
    let before = match q.before.as_deref() {
        None => None,
        Some(c) => Some(
            fastproof_store::tm::runs::decode_execution_cursor(c)
                .ok_or_else(|| ApiError::bad_request("invalid cursor"))?,
        ),
    };
    Ok(Json(
        s.store
            .run_item_executions(p.id, id, before, limit)
            .await
            .map_err(tm_error)?,
    ))
}

/// Query of `GET /people`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PeopleQuery {
    /// Only principals whose name contains this text (case-insensitive, at most 200 bytes).
    pub q: Option<String>,
}

/// A principal someone can assign run items to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    /// The principal name (`assignee` values compare with it).
    pub principal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

/// People of a project.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct People {
    pub items: Vec<Person>,
}

/// People listed at most.
pub const MAX_PEOPLE: usize = 100;

#[utoipa::path(get, path = "/api/v1/projects/{key}/people", tag = "runs", operation_id = "list_project_people",
    description = "Principals to assign run items to: users and managed tokens with `runs:execute` in the project (from \
        the role bindings), members of bound groups who opened test management in the last 90 days (with the groups \
        of their last sign-in; members who never signed in are not known), and the names already used as assignees or \
        executors in the project. `display` is the `name` claim of the identity provider of those members, shown only \
        to callers with `cases:read` or `runs:execute` (never for token names); `q` then matches it too. Sorted by name, at most 100. Assignees are free text; names that are not in this \
        list are allowed.",
    params(("key" = String, Path), PeopleQuery),
    responses((status = 200, body = People), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn project_people(
    State(s): State<AppState>,
    auth: Auth,
    headers: axum::http::HeaderMap,
    P(key): P<String>,
    Q(q): Q<PeopleQuery>,
) -> ApiResult<Json<People>> {
    let p = s.project_for(&auth, &key, &s.perms.projects_read).await?;
    let q = q.q.filter(|q| !q.is_empty());
    if q.as_ref().is_some_and(|q| q.len() > 200) {
        return Err(ApiError::bad_request("q has at most 200 bytes"));
    }
    let _permit = s.guards.heavy(&auth).await?;
    let wanted = |n: &str| {
        q.as_ref()
            .is_none_or(|q| n.to_lowercase().contains(&q.to_lowercase()))
    };
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let policy = s.platform.policy();
    for b in policy.bindings() {
        if let franshub_access::Subject::User(name) = &b.subject
            && wanted(name)
            && policy
                .user_principal(name, &[])
                .is_some_and(|pr| pr.allowed(&s.perms.runs_execute, Some(&p.key)))
        {
            names.insert(name.clone());
        }
    }
    let mut token_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(tokens) = s.platform.tokens() {
        let now = chrono::Utc::now();
        for t in tokens.store().list().await? {
            if t.status(now) == franshub_auth::TokenStatus::Active
                && wanted(&t.name)
                && s.platform
                    .token_principal(&t)
                    .is_some_and(|pr| pr.allowed(&s.perms.runs_execute, Some(&p.key)))
            {
                names.insert(t.name.clone());
            }
            token_names.insert(t.name.clone());
        }
    }
    let show = auth.allowed(&s.perms.cases_read, Some(&p.key))
        || auth.allowed(&s.perms.runs_execute, Some(&p.key));
    let mut displays: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for k in s.store.known_people(MAX_KNOWN_PEOPLE).await? {
        let eligible = policy
            .user_principal(&k.principal, &k.groups)
            .is_some_and(|pr| pr.allowed(&s.perms.runs_execute, Some(&p.key)));
        if !eligible {
            continue;
        }
        let display = k.display.filter(|_| show);
        if wanted(&k.principal) || display.as_deref().is_some_and(wanted) {
            names.insert(k.principal.clone());
        }
        if let Some(d) = display {
            displays.insert(k.principal, d);
        }
    }
    for t in &token_names {
        displays.remove(t);
    }
    for n in s
        .store
        .seen_principals(p.id, q.as_deref(), MAX_PEOPLE as i64)
        .await?
    {
        names.insert(n);
    }
    crate::tm_people::note(&s, &auth, &headers);
    Ok(Json(People {
        items: names
            .into_iter()
            .take(MAX_PEOPLE)
            .map(|principal| Person {
                display: displays.get(&principal).cloned(),
                principal,
            })
            .collect(),
    }))
}

/// People recorded at sign-in that `GET /people` looks at, most recent first.
pub const MAX_KNOWN_PEOPLE: i64 = 5000;

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/{id}/manual/close", tag = "runs", operation_id = "close_manual_run",
    description = "Closes the manual run: untested items become `skipped` unless `untestedTo: leave`; a purely manual launch \
        becomes `finished`, and the gate of a finished launch is evaluated again. Writes to the items then answer 409 \
        `manual_run_closed` until the run is reopened.",
    request_body = ManualClose,
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = Launch), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`manual_run_closed`, `no_manual_run`")))]
pub async fn close_manual_run(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<ManualClose>,
) -> ApiResult<Json<Launch>> {
    let p = s.project_for(&auth, &key, &s.perms.runs_execute).await?;
    let out = s
        .store
        .close_manual_run(&p, id, body.untested_to)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "manualRun.close",
        Some(&p.key),
        serde_json::json!({"launchId": id, "untestedTo": body.untested_to, "skipped": out.skipped,
            "launchFinished": out.launch_finished}),
    )
    .await;
    Ok(Json(s.launch_of(&p, id).await?))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/{id}/manual/reopen", tag = "runs", operation_id = "reopen_manual_run",
    description = "Reopens a closed manual run.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = Launch), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`manual_run_open`, `no_manual_run`")))]
pub async fn reopen_manual_run(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<Launch>> {
    let p = s.project_for(&auth, &key, &s.perms.plans_write).await?;
    s.store
        .reopen_manual_run(&p, id, &s.cfg.tm)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "manualRun.reopen",
        Some(&p.key),
        serde_json::json!({"launchId": id}),
    )
    .await;
    Ok(Json(s.launch_of(&p, id).await?))
}

/// Query of `GET /launches/{id}/manual-report`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ReportQuery {
    /// `csv` (default) or `json`.
    pub format: Option<String>,
}

/// The JSON manual report.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManualReport {
    pub launch_id: Uuid,
    pub launch_number: i64,
    pub launch_name: String,
    pub items: Vec<ManualReportItem>,
}

/// A CSV cell: quoted, with `'` before a leading `=`, `+`, `-`, `@`, tab or carriage return so
/// spreadsheets do not run it as a formula.
pub fn csv_cell(v: &str) -> String {
    let v = if v.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{v}")
    } else {
        v.to_string()
    };
    format!("\"{}\"", v.replace('"', "\"\""))
}

fn csv_row(cells: &[String]) -> String {
    let mut s = cells
        .iter()
        .map(|c| csv_cell(c))
        .collect::<Vec<_>>()
        .join(",");
    s.push_str("\r\n");
    s
}

fn report_csv_row(i: &ManualReportItem) -> String {
    let defect = i.defect.as_ref();
    let steps = i
        .steps
        .iter()
        .filter(|s| s.status != StepStatus::Untested || !s.actual.is_empty())
        .map(|s| {
            let mut t = format!("{} {}", s.path, s.status);
            if !s.actual.is_empty() {
                t.push_str(&format!(": {}", s.actual));
            }
            t
        })
        .collect::<Vec<_>>()
        .join("\n");
    let ts =
        |t: Option<chrono::DateTime<chrono::Utc>>| t.map(|t| t.to_rfc3339()).unwrap_or_default();
    let str_of = |k: &str| {
        defect
            .and_then(|d| d.get(k))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    csv_row(&[
        format!("TC-{}", i.case_number),
        i.name.clone(),
        i.case_version.to_string(),
        i.status.as_str().to_string(),
        i.execution_no.to_string(),
        i.assignee.clone().unwrap_or_default(),
        i.executed_by.clone().unwrap_or_default(),
        ts(i.started_at),
        ts(i.finished_at),
        i.comment.clone(),
        str_of("name"),
        str_of("issueUrl"),
        steps,
    ])
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/manual-report", tag = "runs", operation_id = "manual_report",
    description = "Report of the manual run: case, version, status, steps, executor, times and defects, as CSV (cells that \
        could start a formula are prefixed with `'`) or JSON.",
    params(("key" = String, Path), ("id" = Uuid, Path), ReportQuery),
    responses((status = 200, body = ManualReport, description = "JSON, or text/csv"), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 413, body = ErrorBody),
        (status = 429, body = ErrorBody)))]
pub async fn manual_report(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<ReportQuery>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let csv = match q.format.as_deref() {
        None | Some("csv") => true,
        Some("json") => false,
        Some(_) => return Err(ApiError::bad_request("format must be csv or json")),
    };
    let launch = s.launch_of(&p, id).await?;
    let _permit = s.guards.heavy(&auth).await?;
    let mut out = String::new();
    let mut items = Vec::new();
    let mut bytes = 0usize;
    if csv {
        out.push_str(&csv_row(
            &[
                "case",
                "name",
                "version",
                "status",
                "execution",
                "assignee",
                "executedBy",
                "startedAt",
                "finishedAt",
                "comment",
                "defect",
                "issue",
                "steps",
            ]
            .map(String::from),
        ));
    }
    let mut after = 0i64;
    loop {
        let page = s
            .store
            .manual_report_page(p.id, id, after, REPORT_PAGE)
            .await?;
        let Some(last) = page.last() else {
            break;
        };
        after = last.case_number;
        let n = page.len() as i64;
        for i in page {
            if csv {
                out.push_str(&report_csv_row(&i));
                bytes = out.len();
            } else {
                bytes += serde_json::to_vec(&i).map(|v| v.len()).unwrap_or(0);
                items.push(i);
            }
            if bytes > MAX_REPORT_BYTES {
                return Err(super::uploads::too_large(
                    "max_report_bytes",
                    MAX_REPORT_BYTES as u64,
                ));
            }
        }
        if n < REPORT_PAGE {
            break;
        }
    }
    let (body, ct, ext) = if csv {
        (out.into_bytes(), "text/csv; charset=utf-8", "csv")
    } else {
        (
            serde_json::to_vec(&ManualReport {
                launch_id: id,
                launch_number: launch.number,
                launch_name: launch.name.clone(),
                items,
            })
            .map_err(anyhow::Error::from)?,
            "application/json",
            "json",
        )
    };
    let mut r = (StatusCode::OK, body).into_response();
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    if let Ok(v) = HeaderValue::from_str(&format!(
        "attachment; filename=\"manual-report-{}.{ext}\"",
        launch.number
    )) {
        r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formula_cells_are_prefixed() {
        assert_eq!(csv_cell("=SUM(A1)"), "\"'=SUM(A1)\"");
        assert_eq!(csv_cell("+1"), "\"'+1\"");
        assert_eq!(csv_cell("-2"), "\"'-2\"");
        assert_eq!(csv_cell("@cmd"), "\"'@cmd\"");
        assert_eq!(csv_cell("\tx"), "\"'\tx\"");
        assert_eq!(csv_cell("a \"b\""), "\"a \"\"b\"\"\"");
        assert_eq!(csv_cell("plain"), "\"plain\"");
    }

    #[test]
    fn issue_urls() {
        let gh = new_issue_url(
            "github",
            "https://github.com/org/app/issues/{key}",
            "TC-1 Login: step 2 failed",
            "Actual: 500 & more",
            JiraIds::default(),
        )
        .unwrap()
        .0;
        assert_eq!(
            gh,
            "https://github.com/org/app/issues/new?title=TC-1+Login%3A+step+2+failed&body=Actual%3A+500+%26+more"
        );
        let gl = new_issue_url(
            "gitlab",
            "https://gitlab.example.com/g/app/-/issues/{key}",
            "t",
            "d",
            JiraIds::default(),
        )
        .unwrap()
        .0;
        assert!(gl.starts_with("https://gitlab.example.com/g/app/-/issues/new?issue%5Btitle%5D=t&issue%5Bdescription%5D=d"));
        let j = new_issue_url(
            "jira",
            "https://jira.example.com/browse/{key}",
            "t",
            "d",
            JiraIds::default(),
        )
        .unwrap();
        assert_eq!(
            j,
            (
                "https://jira.example.com/secure/CreateIssue!default.jspa".to_string(),
                false
            ),
            "without pid and issuetype only the create page, the text goes to prefill"
        );
        let j = new_issue_url(
            "jira",
            "https://jira.example.com/browse/{key}",
            "t",
            "d",
            JiraIds {
                project_id: Some("10000"),
                issue_type_id: Some("10004"),
            },
        )
        .unwrap();
        assert_eq!(
            j,
            (
                "https://jira.example.com/secure/CreateIssueDetails!init.jspa?pid=10000&issuetype=10004&summary=t&description=d"
                    .to_string(),
                true
            )
        );
        let none = JiraIds::default();
        assert!(
            new_issue_url("jira", "https://jira.example.com/x/{key}", "t", "d", none).is_none()
        );
        assert!(new_issue_url("github", "javascript:/issues/{key}", "t", "d", none).is_none());
        let long = "x".repeat(50_000);
        let u = new_issue_url(
            "github",
            "https://github.com/org/app/issues/{key}",
            "t",
            &long,
            JiraIds::default(),
        )
        .unwrap()
        .0;
        assert!(u.len() <= MAX_ISSUE_URL_BYTES);
    }
}
