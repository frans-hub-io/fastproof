//! Issues in trackers: create from a result or a failure cluster, link an existing one, list
//! and read them with the state synchronised from the tracker, read now, stop tracking,
//! un-mute linked tests once an issue is closed, test a tracker connection, and the trackers'
//! webhooks.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use chrono::Utc;
use fastproof_store::Project;
use fastproof_store::analysis::triage::{TriageIssue, TriageRequest};
use fastproof_store::trackers::{
    Begin, IssueFilter, IssueTarget, MAX_LOOKUP, MAX_TRACKED_ISSUES, NewLink, TrackerIssue,
};
use franshub_auth::{Auth, require};
use franshub_http::{ApiError, ApiJson, ApiResult};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{ErrorBody, P, Q};
use crate::integrations::issue_template::{self as tpl, IssueContext};
use crate::integrations::secrets::SecretPurpose;
use crate::integrations::settings::{IntegrationSettings, IssueTracker};
use crate::integrations::tracker_sync::{self, MIN_GAP};
use crate::integrations::trackers::{
    Client, Image, MAX_IMAGE_BYTES, MAX_IMAGES, TrackerError, image_matches, parse_key,
};
use crate::state::AppState;

/// Results listed in one create or link request, at most.
pub const MAX_TARGET_RESULTS: usize = 100;
/// Issues created per project per hour, at most.
pub const MAX_CREATES_PER_HOUR: i64 = 60;
/// Largest webhook body.
pub const MAX_WEBHOOK_BYTES: usize = 256 * 1024;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(link))
        .routes(routes!(lookup))
        .routes(routes!(get, delete))
        .routes(routes!(sync_now))
        .routes(routes!(unmute))
        .routes(routes!(dismiss))
        .routes(routes!(test_tracker))
        .routes(routes!(webhook))
}

fn tracker_error(e: TrackerError) -> ApiError {
    let msg = e.to_string();
    match e {
        TrackerError::Config(_) => ApiError::new(StatusCode::CONFLICT, "tracker_settings", msg),
        TrackerError::Refused(..) => ApiError::new(StatusCode::BAD_GATEWAY, "tracker_refused", msg),
        TrackerError::NotFound => ApiError::new(StatusCode::NOT_FOUND, "tracker_not_found", msg),
        TrackerError::Throttled(d) => ApiError::too_many_requests(msg)
            .with_header(header::RETRY_AFTER, HeaderValue::from(d.as_secs().max(1))),
        TrackerError::Unavailable(_) => {
            ApiError::new(StatusCode::BAD_GATEWAY, "tracker_unavailable", msg)
        }
    }
}

async fn settings_of(s: &AppState, p: &Project) -> ApiResult<IntegrationSettings> {
    let doc = s.store.project_settings_json(p.id).await?;
    IntegrationSettings::parse(&doc)
        .map_err(|e| ApiError::conflict(format!("invalid project settings: {e}")))
}

fn connected<'a>(settings: &'a IntegrationSettings, id: &str) -> ApiResult<&'a IssueTracker> {
    IssueTracker::connected(&settings.issue_trackers, id).ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            "no_issue_tracker",
            format!("no issue tracker with id {id:?} and an api connection is configured"),
        )
    })
}

/// `user`, or `ai` for an AI agent token (it holds `ai:propose` but not `cases:write`).
fn source_of(s: &AppState, auth: &Auth, key: &str) -> &'static str {
    if auth.allowed(&s.perms.ai_propose, Some(key))
        && !auth.allowed(&s.perms.cases_write, Some(key))
    {
        "ai"
    } else {
        "user"
    }
}

/// Un-muting needs `results:mute` where the server has it, `results:triage` otherwise.
fn require_mute(s: &AppState, auth: &Auth, key: &str) -> ApiResult<()> {
    if let Some(p) = s.platform.policy().catalog().permission("results:mute") {
        require(auth, &p, Some(key))?;
    }
    Ok(())
}

fn may_mute(s: &AppState, auth: &Auth, key: &str) -> bool {
    match s.platform.policy().catalog().permission("results:mute") {
        Some(p) => auth.allowed(&p, Some(key)),
        None => auth.allowed(&s.perms.results_triage, Some(key)),
    }
}

/// `{items, next}` of tracked issues.
#[derive(Serialize, ToSchema)]
pub struct TrackerIssueList {
    pub items: Vec<TrackerIssue>,
    /// Cursor of the next page (`before=`), `null` on the last page.
    pub next: Option<Uuid>,
}

/// Query of `GET /tracker-issues`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct IssueQuery {
    /// Tracker id.
    pub tracker: Option<String>,
    /// `open`, `closed` or `creating`.
    pub state: Option<String>,
    /// Only issues that suggest un-muting (or only those that do not).
    pub unmute_suggested: Option<bool>,
    /// Issues linked to this result.
    pub result_id: Option<Uuid>,
    /// Issues linked to results or clusters of this launch.
    pub launch_id: Option<Uuid>,
    pub before: Option<Uuid>,
    /// 1 to 200, default 50.
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/tracker-issues", tag = "integrations", operation_id = "tracker_issues_list",
    description = "Issues created or linked from FastProof with the state last read from their tracker, newest first.",
    params(("key" = String, Path), IssueQuery),
    responses((status = 200, body = TrackerIssueList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<IssueQuery>,
) -> ApiResult<Json<TrackerIssueList>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    if q.state
        .as_deref()
        .is_some_and(|x| !matches!(x, "open" | "closed" | "creating"))
    {
        return Err(ApiError::bad_request(
            "state must be open, closed or creating",
        ));
    }
    if q.tracker.as_deref().is_some_and(|t| t.len() > 32) {
        return Err(ApiError::bad_request("tracker is a tracker id"));
    }
    let (items, next) = s
        .store
        .tracker_issues(
            p.id,
            &IssueFilter {
                tracker_id: q.tracker,
                category: q.state,
                unmute_suggested: q.unmute_suggested,
                result_id: q.result_id,
                launch_id: q.launch_id,
                before: q.before,
                limit: q.limit.unwrap_or(50),
                ..Default::default()
            },
        )
        .await?;
    Ok(Json(TrackerIssueList { items, next }))
}

/// Body of `POST /tracker-issues/lookup`.
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IssueLookup {
    /// Issue URLs (as on defects), at most 100.
    #[serde(default)]
    pub urls: Vec<String>,
    /// Issue keys, at most 100.
    #[serde(default)]
    pub keys: Vec<String>,
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/tracker-issues/lookup", tag = "integrations", operation_id = "tracker_issues_lookup",
    description = "The tracked issues with these URLs or keys (the issue links of defects on a page of results), to show their state.",
    request_body = IssueLookup, params(("key" = String, Path)),
    responses((status = 200, body = TrackerIssueList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn lookup(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<IssueLookup>,
) -> ApiResult<Json<TrackerIssueList>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    if body.urls.len() + body.keys.len() > MAX_LOOKUP
        || body.urls.iter().chain(&body.keys).any(|x| x.len() > 2048)
    {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_LOOKUP} urls and keys of at most 2048 bytes"
        )));
    }
    if body.urls.is_empty() && body.keys.is_empty() {
        return Ok(Json(TrackerIssueList {
            items: Vec::new(),
            next: None,
        }));
    }
    let (items, _) = s
        .store
        .tracker_issues(
            p.id,
            &IssueFilter {
                urls: body.urls,
                keys: body.keys,
                limit: 200,
                ..Default::default()
            },
        )
        .await?;
    Ok(Json(TrackerIssueList { items, next: None }))
}

/// A tracked issue and what the caller may do with it.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrackerIssueDetail {
    #[serde(flatten)]
    pub issue: TrackerIssue,
    /// The caller may un-mute the linked tests (`results:mute`).
    pub can_unmute: bool,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/tracker-issues/{id}", tag = "integrations", operation_id = "tracker_issues_get",
    description = "One tracked issue with its links and the number of linked tests that are muted now.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = TrackerIssueDetail), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<TrackerIssueDetail>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let issue = s
        .store
        .tracker_issue(p.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("tracker issue"))?;
    Ok(Json(TrackerIssueDetail {
        issue,
        can_unmute: may_mute(&s, &auth, &p.key)
            && auth.allowed(&s.perms.results_triage, Some(&p.key)),
    }))
}

/// What an issue is created or linked for: results, or a failure cluster (`launchId` and
/// `fingerprint`).
#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IssueFor {
    /// 1 to 100 results.
    pub result_ids: Option<Vec<Uuid>>,
    pub launch_id: Option<Uuid>,
    /// With `launchId`: every result of the launch with this failure fingerprint.
    pub fingerprint: Option<String>,
}

/// Body of `POST /tracker-issues`.
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateTrackerIssue {
    /// Tracker id (`issueTrackers[].id` with `api`).
    pub tracker: String,
    /// Makes the request idempotent: a retried request with the same id answers the issue it
    /// created instead of creating another.
    pub request_id: Uuid,
    #[serde(flatten)]
    pub target: IssueFor,
    /// Title instead of the template's, at most 255 characters.
    pub title: Option<String>,
    /// A note put above the description, at most 4000 bytes (shown as plain text).
    pub note: Option<String>,
    /// Attach screenshots; default from the tracker settings.
    pub attach_screenshots: Option<bool>,
    /// Also set this defect type on the results.
    pub defect_type_id: Option<Uuid>,
    /// Only render the title and description; nothing is sent.
    #[serde(default)]
    pub dry_run: bool,
}

/// Title and description of a new issue as they are sent (the description in the tracker's
/// markup: Markdown, Jira wiki markup, ADF JSON or HTML) and as plain text.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IssuePreview {
    pub title: String,
    pub body: String,
    /// `markdown`, `jiraWiki`, `adf` or `html`.
    pub markup: String,
    pub text: String,
    /// Screenshots that would be attached.
    pub screenshots: Vec<String>,
}

/// Answer of `POST /tracker-issues`.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrackerIssueCreated {
    /// The issue (absent for a dry run).
    pub issue: Option<TrackerIssue>,
    pub preview: IssuePreview,
    /// Screenshots attached.
    pub attached: usize,
    /// Screenshots that could not be attached, and why.
    pub notes: Vec<String>,
    /// Results whose defect now links the issue.
    pub updated_results: u64,
    /// The request id was used before: this is the issue it created.
    pub repeated: bool,
}

fn check_target(t: &IssueFor) -> ApiResult<()> {
    match (&t.result_ids, t.launch_id, &t.fingerprint) {
        (Some(ids), None, None) if !ids.is_empty() && ids.len() <= MAX_TARGET_RESULTS => Ok(()),
        (None, Some(_), Some(f)) if f.len() == 16 && f.bytes().all(|b| b.is_ascii_hexdigit()) => {
            Ok(())
        }
        _ => Err(ApiError::bad_request(format!(
            "give resultIds (1 to {MAX_TARGET_RESULTS}), or launchId and fingerprint (16 hex digits)"
        ))),
    }
}

async fn targets_of(s: &AppState, p: &Project, t: &IssueFor) -> ApiResult<Vec<IssueTarget>> {
    let rows = match (&t.result_ids, t.launch_id, &t.fingerprint) {
        (Some(ids), _, _) => s.store.issue_targets(p.id, ids).await?,
        (None, Some(l), Some(f)) => s.store.cluster_targets(p.id, l, f).await?,
        _ => Vec::new(),
    };
    if rows.is_empty() {
        return Err(ApiError::not_found("result"));
    }
    Ok(rows)
}

fn links_of(t: &IssueFor, rows: &[IssueTarget]) -> Vec<NewLink> {
    match (&t.launch_id, &t.fingerprint) {
        (Some(l), Some(f)) => vec![NewLink {
            launch_id: Some(*l),
            result_id: None,
            fingerprint: Some(f.clone()),
            series_ids: rows.iter().map(|r| r.series_id).collect(),
        }],
        _ => rows
            .iter()
            .map(|r| NewLink {
                launch_id: Some(r.launch_id),
                result_id: Some(r.id),
                fingerprint: None,
                series_ids: vec![r.series_id],
            })
            .collect(),
    }
}

async fn context(
    s: &AppState,
    p: &Project,
    t: &IssueFor,
    rows: &[IssueTarget],
    user: &str,
) -> ApiResult<(IssueContext, Option<fastproof_store::ResultDetail>)> {
    let first = &rows[0];
    let detail = s
        .store
        .result_detail(p.id, first.id, s.history.as_ref())
        .await?;
    let launch = s.store.launch(p.id, first.launch_id).await?;
    let err = detail
        .as_ref()
        .and_then(|d| d.attempts.iter().rev().find_map(|a| a.error.clone()));
    let mut c = IssueContext {
        project: p.key.clone(),
        name: first.name.clone(),
        full_name: first.full_name.clone(),
        status: detail
            .as_ref()
            .map(|d| d.summary.status.clone())
            .unwrap_or_default(),
        message: err.as_ref().and_then(|e| e.message.clone()),
        trace: err.as_ref().and_then(|e| e.trace.clone()),
        launch_url: crate::integrations::dispatch::launch_url(s, &p.key, first.launch_id),
        result_url: crate::integrations::dispatch::page_url(
            s,
            &format!("p/{}/results/{}", p.key, first.id),
        ),
        fingerprint: t
            .fingerprint
            .clone()
            .or_else(|| detail.as_ref().and_then(|d| d.summary.fingerprint.clone())),
        count: rows.len(),
        tests: rows.iter().map(|r| r.full_name.clone()).collect(),
        defect: detail
            .as_ref()
            .and_then(|d| d.summary.defect.as_ref().map(|x| x.name.clone())),
        user: user.to_string(),
        ..Default::default()
    };
    if let Some(l) = launch {
        c.launch = format!("#{} {}", l.number, l.name);
        c.branch = l.vcs.as_ref().and_then(|v| v.branch.clone());
        c.environment = crate::integrations::gates::env_of(&l).map(str::to_string);
    }
    Ok((c, detail))
}

/// Image attachments of the last attempt that may be attached.
fn screenshot_refs(d: &fastproof_store::ResultDetail) -> Vec<(Uuid, String, String)> {
    d.attempts
        .last()
        .map(|a| {
            a.attachments
                .iter()
                .filter(|x| x.kind == "image" && x.size.is_none_or(|n| n <= MAX_IMAGE_BYTES))
                .filter_map(|x| {
                    let ct = x.content_type.clone()?;
                    matches!(
                        ct.as_str(),
                        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                    )
                    .then(|| (x.id, x.name.clone(), ct))
                })
                .filter_map(|(id, n, ct)| id.map(|id| (id, n, ct)))
                .take(MAX_IMAGES)
                .collect()
        })
        .unwrap_or_default()
}

async fn read_image(s: &AppState, p: &Project, id: Uuid, name: &str) -> Option<Image> {
    let a = s.store.attachment(p.id, id).await.ok()??;
    if a.size < 0 || a.size as u64 > MAX_IMAGE_BYTES {
        return None;
    }
    let key = fastproof_blob::BlobKey::parse(&a.blob_key).ok()?;
    let body = s.blobs.get(&key).await.ok()??;
    if body.size > MAX_IMAGE_BYTES {
        return None;
    }
    let mut buf = Vec::with_capacity(body.size as usize);
    let mut stream = body.stream;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.ok()?;
        if (buf.len() + chunk.len()) as u64 > MAX_IMAGE_BYTES {
            return None;
        }
        buf.extend_from_slice(&chunk);
    }
    image_matches(&a.content_type, &buf).then(|| Image {
        name: name.to_string(),
        content_type: a.content_type,
        bytes: Bytes::from(buf),
    })
}

fn markup_of(t: &IssueTracker) -> &'static str {
    match t.tracker.as_str() {
        "jira" if t.api.as_ref().is_some_and(|a| a.jira_server()) => "jiraWiki",
        "jira" => "adf",
        "azure" => "html",
        _ => "markdown",
    }
}

/// Links the issue on the results' defects (and sets a defect type when asked).
async fn link_defects(
    s: &AppState,
    auth: &Auth,
    p: &Project,
    t: &IssueFor,
    issue: &TrackerIssue,
    defect_type_id: Option<Uuid>,
) -> ApiResult<u64> {
    let Some(url) = issue.url.clone() else {
        return Ok(0);
    };
    let req = TriageRequest {
        result_ids: t.result_ids.clone(),
        launch_id: t.launch_id,
        fingerprint: t.fingerprint.clone(),
        defect_type_id,
        issue: Some(TriageIssue {
            url: Some(url),
            key: issue.key.clone(),
        }),
        source: Some(source_of(s, auth, &p.key).into()),
        ..Default::default()
    };
    let (out, _) = super::analysis::apply_triage(s, p, &auth.principal.name, req).await?;
    Ok(out.updated)
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/tracker-issues", tag = "integrations", operation_id = "tracker_issues_create",
    description = "Creates an issue in a connected tracker for results or a failure cluster, from the tracker's title and \
        description templates (values from results are escaped for the tracker's markup; messages and traces go into code \
        blocks), optionally with up to 3 screenshots of the first result's last attempt (Jira, GitLab, Azure DevOps), \
        links it to the results or the cluster, puts it on their defects and starts reading its state back. `dryRun` only \
        renders the preview. Idempotent by `requestId`. At most 60 issues per project per hour and 10 000 tracked issues. \
        Needs `results:triage`; audited as `tracker.issue.create`.",
    request_body = CreateTrackerIssue, params(("key" = String, Path)),
    responses((status = 200, body = TrackerIssueCreated), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`no_issue_tracker`, `tracker_settings`, or the request id is still creating"),
        (status = 429, body = ErrorBody), (status = 502, body = ErrorBody, description = "`tracker_refused` or `tracker_unavailable`")))]
pub async fn create(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<CreateTrackerIssue>,
) -> ApiResult<Json<TrackerIssueCreated>> {
    let p = s.project_for(&auth, &key, &s.perms.results_triage).await?;
    check_target(&body.target)?;
    if body
        .title
        .as_ref()
        .is_some_and(|t| t.trim().is_empty() || t.chars().count() > 255)
    {
        return Err(ApiError::bad_request("title must have 1 to 255 characters"));
    }
    if body.note.as_ref().is_some_and(|n| n.len() > 4000) {
        return Err(ApiError::bad_request("note must have at most 4000 bytes"));
    }
    let settings = settings_of(&s, &p).await?;
    let tracker = connected(&settings, &body.tracker)?;
    let api = tracker.api.as_ref().ok_or_else(ApiError::internal)?;
    if let Some(d) = body.defect_type_id
        && s.store.defect_type(p.id, d).await?.is_none()
    {
        return Err(ApiError::bad_request("unknown defectTypeId"));
    }
    let rows = targets_of(&s, &p, &body.target).await?;
    let (ctx, detail) = context(&s, &p, &body.target, &rows, &auth.principal.name).await?;
    let title = match &body.title {
        Some(t) => t.split_whitespace().collect::<Vec<_>>().join(" "),
        None => tpl::render_title(
            api.title_template.as_deref().unwrap_or(tpl::DEFAULT_TITLE),
            &ctx,
        ),
    };
    let mut blocks = Vec::new();
    if let Some(n) = body.note.as_deref().filter(|n| !n.trim().is_empty()) {
        blocks.push(tpl::Block::Para(
            n.lines()
                .filter(|l| !l.trim().is_empty())
                .take(50)
                .map(|l| vec![tpl::Inline::Value(l.to_string())])
                .collect(),
        ));
    }
    blocks.extend(tpl::render(
        api.body_template.as_deref().unwrap_or(tpl::DEFAULT_BODY),
        &ctx,
    ));
    let attach = body.attach_screenshots.unwrap_or(api.attach_screenshots);
    let shots = if attach {
        detail.as_ref().map(screenshot_refs).unwrap_or_default()
    } else {
        Vec::new()
    };
    let markup = markup_of(tracker);
    let rendered = match markup {
        "jiraWiki" => tpl::jira_wiki(&blocks),
        "adf" => tpl::adf(&blocks).to_string(),
        "html" => tpl::html(&blocks),
        _ => tpl::markdown(&blocks),
    };
    let preview = IssuePreview {
        title: title.clone(),
        body: rendered,
        markup: markup.into(),
        text: tpl::plain(&blocks),
        screenshots: shots.iter().map(|x| x.1.clone()).collect(),
    };
    if body.dry_run {
        return Ok(Json(TrackerIssueCreated {
            issue: None,
            preview,
            attached: 0,
            notes: Vec::new(),
            updated_results: 0,
            repeated: false,
        }));
    }
    let (total, recent) = s
        .store
        .tracker_issue_counts(p.id, Utc::now() - chrono::Duration::hours(1))
        .await?;
    let client = Client::new(&s, p.id, tracker)
        .await
        .map_err(tracker_error)?;
    let id = match s
        .store
        .begin_tracker_issue(
            p.id,
            &body.tracker,
            &tracker.tracker,
            body.request_id,
            &auth.principal.name,
        )
        .await?
    {
        Begin::Existing(issue) if issue.category == "creating" => {
            return Err(ApiError::conflict(
                "this request is still creating its issue",
            ));
        }
        Begin::Existing(issue) => {
            return Ok(Json(TrackerIssueCreated {
                issue: Some(*issue),
                preview,
                attached: 0,
                notes: Vec::new(),
                updated_results: 0,
                repeated: true,
            }));
        }
        Begin::New(id) => id,
    };
    if total >= MAX_TRACKED_ISSUES || recent >= MAX_CREATES_PER_HOUR {
        s.store.abandon_tracker_issue(id).await?;
        return Err(if total >= MAX_TRACKED_ISSUES {
            ApiError::conflict(format!(
                "the project tracks {MAX_TRACKED_ISSUES} issues; stop tracking old ones first"
            ))
        } else {
            ApiError::too_many_requests(format!(
                "at most {MAX_CREATES_PER_HOUR} issues per project per hour"
            ))
            .with_header(header::RETRY_AFTER, HeaderValue::from_static("600"))
        });
    }
    let mut images = Vec::new();
    for (aid, name, _) in &shots {
        if let Some(img) = read_image(&s, &p, *aid, name).await {
            images.push(img);
        }
    }
    let (state, attached, mut notes) = match client.create(&title, &blocks, &images).await {
        Ok(x) => x,
        Err(e) => {
            s.store.abandon_tracker_issue(id).await?;
            s.audit(
                &auth,
                "tracker.issue.create",
                Some(&p.key),
                serde_json::json!({"tracker": body.tracker, "ok": false, "error": e.to_string()}),
            )
            .await;
            return Err(tracker_error(e));
        }
    };
    if images.len() < shots.len() {
        notes.push(format!(
            "{} screenshot(s) could not be read or are not images",
            shots.len() - images.len()
        ));
    }
    let next = tracker_sync::next_read(api, state.closed, s.cfg.integrations.tracker_poll);
    let issue_id = if s.store.complete_tracker_issue(id, &state, next).await? {
        id
    } else {
        s.store
            .upsert_tracker_issue(
                p.id,
                &body.tracker,
                &tracker.tracker,
                &state,
                &auth.principal.name,
                next,
            )
            .await?
    };
    s.store
        .add_tracker_links(
            p.id,
            issue_id,
            &links_of(&body.target, &rows),
            &auth.principal.name,
        )
        .await?;
    let issue = s
        .store
        .tracker_issue(p.id, issue_id)
        .await?
        .ok_or_else(ApiError::internal)?;
    let updated = link_defects(&s, &auth, &p, &body.target, &issue, body.defect_type_id).await?;
    s.audit(
        &auth,
        "tracker.issue.create",
        Some(&p.key),
        serde_json::json!({"tracker": body.tracker, "ok": true, "issueId": issue.id, "key": issue.key,
            "url": issue.url, "results": rows.len(), "launchId": body.target.launch_id,
            "fingerprint": body.target.fingerprint, "attached": attached}),
    )
    .await;
    Ok(Json(TrackerIssueCreated {
        issue: Some(issue),
        preview,
        attached,
        notes,
        updated_results: updated,
        repeated: false,
    }))
}

/// Body of `POST /tracker-issues/link`.
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkTrackerIssue {
    /// Tracker id.
    pub tracker: String,
    /// `SHOP-12` (Jira), `12` or `#12` (GitLab, GitHub, Azure DevOps).
    pub issue_key: String,
    #[serde(flatten)]
    pub target: IssueFor,
    pub defect_type_id: Option<Uuid>,
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/tracker-issues/link", tag = "integrations", operation_id = "tracker_issues_link",
    description = "Links an existing issue of a connected tracker to results or a failure cluster: reads it from the tracker \
        (it must exist), tracks its state, and puts it on the results' defects. Needs `results:triage`; audited as \
        `tracker.issue.link`.",
    request_body = LinkTrackerIssue, params(("key" = String, Path)),
    responses((status = 200, body = TrackerIssueCreated), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 429, body = ErrorBody),
        (status = 502, body = ErrorBody)))]
pub async fn link(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<LinkTrackerIssue>,
) -> ApiResult<Json<TrackerIssueCreated>> {
    let p = s.project_for(&auth, &key, &s.perms.results_triage).await?;
    check_target(&body.target)?;
    let settings = settings_of(&s, &p).await?;
    let tracker = connected(&settings, &body.tracker)?;
    let api = tracker.api.as_ref().ok_or_else(ApiError::internal)?;
    let issue_key = parse_key(&tracker.tracker, &body.issue_key).ok_or_else(|| {
        ApiError::bad_request(match tracker.tracker.as_str() {
            "jira" => "issueKey must be a Jira key such as SHOP-12",
            _ => "issueKey must be an issue number such as 12",
        })
    })?;
    if let Some(d) = body.defect_type_id
        && s.store.defect_type(p.id, d).await?.is_none()
    {
        return Err(ApiError::bad_request("unknown defectTypeId"));
    }
    let rows = targets_of(&s, &p, &body.target).await?;
    let known = s
        .store
        .tracker_issue_by_key(p.id, &body.tracker, &issue_key)
        .await?;
    if known.is_none() {
        let (total, _) = s.store.tracker_issue_counts(p.id, Utc::now()).await?;
        if total >= MAX_TRACKED_ISSUES {
            return Err(ApiError::conflict(format!(
                "the project tracks {MAX_TRACKED_ISSUES} issues; stop tracking old ones first"
            )));
        }
    }
    let client = Client::new(&s, p.id, tracker)
        .await
        .map_err(tracker_error)?;
    let (state, _) = client
        .get(&issue_key, None, None)
        .await
        .map_err(tracker_error)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "tracker_unavailable",
                "no issue in the answer",
            )
        })?;
    let next = tracker_sync::next_read(api, state.closed, s.cfg.integrations.tracker_poll);
    let issue_id = s
        .store
        .upsert_tracker_issue(
            p.id,
            &body.tracker,
            &tracker.tracker,
            &state,
            &auth.principal.name,
            next,
        )
        .await?;
    s.store
        .add_tracker_links(
            p.id,
            issue_id,
            &links_of(&body.target, &rows),
            &auth.principal.name,
        )
        .await?;
    let issue = s
        .store
        .tracker_issue(p.id, issue_id)
        .await?
        .ok_or_else(ApiError::internal)?;
    let updated = link_defects(&s, &auth, &p, &body.target, &issue, body.defect_type_id).await?;
    s.audit(
        &auth,
        "tracker.issue.link",
        Some(&p.key),
        serde_json::json!({"tracker": body.tracker, "issueId": issue.id, "key": issue.key, "results": rows.len(),
            "launchId": body.target.launch_id, "fingerprint": body.target.fingerprint}),
    )
    .await;
    Ok(Json(TrackerIssueCreated {
        preview: IssuePreview {
            title: issue.title.clone(),
            body: String::new(),
            markup: markup_of(tracker).into(),
            text: String::new(),
            screenshots: Vec::new(),
        },
        issue: Some(issue),
        attached: 0,
        notes: Vec::new(),
        updated_results: updated,
        repeated: known.is_some(),
    }))
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/tracker-issues/{id}", tag = "integrations", operation_id = "tracker_issues_delete",
    description = "Stops tracking an issue (its links go; the issue stays in the tracker and on the defects). Needs \
        `results:triage`; audited as `tracker.issue.untrack`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn delete(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<StatusCode> {
    let p = s.project_for(&auth, &key, &s.perms.results_triage).await?;
    if !s.store.delete_tracker_issue(p.id, id).await? {
        return Err(ApiError::not_found("tracker issue"));
    }
    s.audit(
        &auth,
        "tracker.issue.untrack",
        Some(&p.key),
        serde_json::json!({"issueId": id}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/tracker-issues/{id}/sync", tag = "integrations", operation_id = "tracker_issues_sync",
    description = "Reads the issue from its tracker now (at most once per 5 seconds per issue). Needs `results:triage`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = TrackerIssueDetail), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 429, body = ErrorBody)))]
pub async fn sync_now(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<TrackerIssueDetail>> {
    let p = s.project_for(&auth, &key, &s.perms.results_triage).await?;
    let issue = s
        .store
        .tracker_issue(p.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("tracker issue"))?;
    if issue.key.is_none() {
        return Err(ApiError::conflict("the issue is still being created"));
    }
    if issue
        .synced_at
        .is_some_and(|t| Utc::now() - t < chrono::Duration::from_std(MIN_GAP).unwrap_or_default())
    {
        return Err(
            ApiError::too_many_requests("read at most once per 5 seconds")
                .with_header(header::RETRY_AFTER, HeaderValue::from_static("5")),
        );
    }
    let settings = settings_of(&s, &p).await?;
    let job = s
        .store
        .tracker_sync_job(p.id, id)
        .await?
        .ok_or_else(|| ApiError::conflict("the issue is still being created"))?;
    tracker_sync::sync_one(
        &s,
        IssueTracker::connected(&settings.issue_trackers, &issue.tracker_id),
        &job,
    )
    .await;
    let issue = s
        .store
        .tracker_issue(p.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("tracker issue"))?;
    Ok(Json(TrackerIssueDetail {
        issue,
        can_unmute: may_mute(&s, &auth, &p.key),
    }))
}

/// Answer of `POST /tracker-issues/{id}/unmute`.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UnmuteOutcome {
    /// Tests whose mute was lifted.
    pub tests: u64,
    /// Results whose `muted` flag was cleared.
    pub results: u64,
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/tracker-issues/{id}/unmute", tag = "integrations", operation_id = "tracker_issues_unmute",
    description = "Lifts the mutes of the tests linked to the issue (usually once it is closed) and clears the suggestion. \
        Needs `results:triage` and `results:mute`; audited as `tracker.issue.unmute`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = UnmuteOutcome), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn unmute(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<UnmuteOutcome>> {
    let p = s.project_for(&auth, &key, &s.perms.results_triage).await?;
    require_mute(&s, &auth, &p.key)?;
    if s.store.tracker_issue(p.id, id).await?.is_none() {
        return Err(ApiError::not_found("tracker issue"));
    }
    let links = s.store.tracker_issue_links(p.id, id).await?;
    let muted = s.store.muted_linked_series(p.id, &links).await?;
    let mut results = 0;
    let source = source_of(&s, &auth, &p.key);
    let result_ids: Vec<Uuid> = links.iter().filter_map(|l| l.result_id).collect();
    for chunk in result_ids.chunks(MAX_TARGET_RESULTS) {
        let req = TriageRequest {
            result_ids: Some(chunk.to_vec()),
            unmute: Some(true),
            source: Some(source.into()),
            ..Default::default()
        };
        results += super::analysis::apply_triage(&s, &p, &auth.principal.name, req)
            .await?
            .0
            .updated;
    }
    for l in links.iter().filter(|l| l.result_id.is_none()) {
        if let (Some(launch), Some(f)) = (l.launch_id, l.fingerprint.clone())
            && s.store.launch(p.id, launch).await?.is_some()
        {
            let req = TriageRequest {
                launch_id: Some(launch),
                fingerprint: Some(f),
                unmute: Some(true),
                source: Some(source.into()),
                ..Default::default()
            };
            results += super::analysis::apply_triage(&s, &p, &auth.principal.name, req)
                .await?
                .0
                .updated;
        }
    }
    s.store.delete_series_mutes(p.id, &muted).await?;
    s.store.dismiss_unmute_suggestion(p.id, id).await?;
    s.audit(
        &auth,
        "tracker.issue.unmute",
        Some(&p.key),
        serde_json::json!({"issueId": id, "tests": muted.len(), "results": results}),
    )
    .await;
    Ok(Json(UnmuteOutcome {
        tests: muted.len() as u64,
        results,
    }))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/tracker-issues/{id}/dismiss", tag = "integrations", operation_id = "tracker_issues_dismiss",
    description = "Clears the un-mute suggestion of a closed issue and keeps the mutes. Needs `results:triage`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn dismiss(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<StatusCode> {
    let p = s.project_for(&auth, &key, &s.perms.results_triage).await?;
    if !s.store.dismiss_unmute_suggestion(p.id, id).await? {
        return Err(ApiError::not_found("tracker issue"));
    }
    s.audit(
        &auth,
        "tracker.issue.dismiss",
        Some(&p.key),
        serde_json::json!({"issueId": id}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Answer of a connection test.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrackerTest {
    pub ok: bool,
    /// The project as the tracker names it.
    pub project: Option<String>,
    pub error: Option<String>,
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/issue-trackers/{trackerId}/test", tag = "integrations", operation_id = "tracker_test",
    description = "Reads the configured project from the tracker with the stored token (saved settings). Needs `settings:write`.",
    params(("key" = String, Path), ("trackerId" = String, Path)),
    responses((status = 200, body = TrackerTest), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody)))]
pub async fn test_tracker(
    State(s): State<AppState>,
    auth: Auth,
    P((key, tracker_id)): P<(String, String)>,
) -> ApiResult<Json<TrackerTest>> {
    let p = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    let settings = settings_of(&s, &p).await?;
    let tracker = connected(&settings, &tracker_id)?;
    let r = match Client::new(&s, p.id, tracker).await {
        Ok(c) => c.check().await,
        Err(e) => Err(e),
    };
    s.audit(
        &auth,
        "tracker.test",
        Some(&p.key),
        serde_json::json!({"tracker": tracker_id, "ok": r.is_ok()}),
    )
    .await;
    Ok(Json(match r {
        Ok(name) => TrackerTest {
            ok: true,
            project: Some(name),
            error: None,
        },
        Err(e) => TrackerTest {
            ok: false,
            project: None,
            error: Some(e.to_string()),
        },
    }))
}

#[utoipa::path(post, path = "/api/v1/hooks/trackers/{key}/{trackerId}", tag = "integrations", operation_id = "tracker_webhook",
    description = "Receives the webhook of a tracker with `sync: webhook` (no bearer token). The request must prove the \
        secret named by `webhookSecretRef`: GitHub `X-Hub-Signature-256` and Jira `X-Hub-Signature` (`sha256=` HMAC of the \
        body), GitLab `X-Gitlab-Token`, Azure DevOps basic authentication with the secret as the password. A verified call \
        only schedules a read of the issue it names through the tracker's API (at most one per issue per 5 seconds); its \
        payload is never taken as the state. Every refusal is the same `401`. Body at most 256 KiB.",
    request_body(content = Object, content_type = "application/json", description = "The tracker's webhook payload"),
    params(("key" = String, Path), ("trackerId" = String, Path)),
    security(()),
    responses((status = 202, description = "accepted"), (status = 401, body = ErrorBody), (status = 413, body = ErrorBody)))]
pub async fn webhook(
    State(s): State<AppState>,
    P((key, tracker_id)): P<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<StatusCode> {
    if body.len() > MAX_WEBHOOK_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "the body is larger than 256 KiB",
        ));
    }
    let refused = || {
        metrics::counter!("fastproof_tracker_webhooks_total", "outcome" => "refused").increment(1);
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "webhook not verified",
        )
    };
    if !fastproof_blob::valid_project_key(&key) || tracker_id.len() > 32 {
        return Err(refused());
    }
    let Some(p) = s.store.project(&key).await? else {
        return Err(refused());
    };
    let doc = s.store.project_settings_json(p.id).await?;
    let settings = IntegrationSettings::parse(&doc).unwrap_or_default();
    let Some(t) = IssueTracker::connected(&settings.issue_trackers, &tracker_id) else {
        return Err(refused());
    };
    let Some(api) = t.api.as_ref().filter(|a| a.sync_mode() == "webhook") else {
        return Err(refused());
    };
    let Some(secret_ref) = api.webhook_secret_ref.as_deref() else {
        return Err(refused());
    };
    let Ok(secret) = s
        .project_secret(p.id, secret_ref, SecretPurpose::Webhook, None)
        .await
    else {
        return Err(refused());
    };
    if !tracker_sync::verify(&t.tracker, &headers, &body, secret.expose()) {
        return Err(refused());
    }
    let refs = tracker_sync::refs(&t.tracker, &body);
    let n = s
        .store
        .request_tracker_sync_by_ref(p.id, &tracker_id, &refs, MIN_GAP)
        .await?;
    metrics::counter!("fastproof_tracker_webhooks_total", "outcome" => if n > 0 { "scheduled" } else { "ignored" })
        .increment(1);
    Ok(StatusCode::ACCEPTED)
}
