//! The MCP endpoint (`/mcp`, streamable HTTP, stateless) for local agents such as GitHub
//! Copilot (X6). It authenticates with the same bearer tokens as the API; every tool checks
//! the caller's permission in the project scope. Write tools need `results:triage`, always
//! record `source: ai` and are audited.

use std::sync::Arc;

use axum::Router;
use axum::extract::{FromRequestParts, Request, State};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use fastproof_store::HistoryFilter;
use fastproof_store::launches::LaunchFilter;
use franshub_access::Permission;
use franshub_auth::Auth;
use franshub_http::ApiError;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::integrations::analysis::{MAX_TRIAGE_IDS, TriageIssue, TriageMute, TriageRequest};
use crate::integrations::fastmock;
use crate::integrations::issues::issue_link;
use crate::integrations::settings::IntegrationSettings;
use crate::state::AppState;

/// Largest tool result text.
pub const MAX_OUTPUT: usize = 256 * 1024;
/// Largest log or trace text per field in `result_logs`.
pub const MAX_LOG: usize = 64 * 1024;
/// Largest MCP request body.
pub const MAX_BODY: usize = 256 * 1024;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListLaunchesArgs {
    /// Project key.
    project: String,
    branch: Option<String>,
    environment: Option<String>,
    /// `running`, `finished` or `interrupted`.
    status: Option<String>,
    /// 1 to 100 (default 20).
    limit: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LaunchArgs {
    project: String,
    launch_id: Uuid,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResultArgs {
    project: String,
    result_id: Uuid,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttemptArgs {
    project: String,
    result_id: Uuid,
    /// Attempt number, 1-based (default: the last attempt).
    attempt: Option<i32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SimilarArgs {
    project: String,
    result_id: Uuid,
    /// 1 to 50 (default 10).
    limit: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HistoryArgs {
    project: String,
    test_case_id: Uuid,
    branch: Option<String>,
    environment: Option<String>,
    /// 1 to 100 (default 30).
    limit: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FlakyArgs {
    project: String,
    branch: Option<String>,
    environment: Option<String>,
    /// 1 to 100 (default 20).
    limit: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompareArgs {
    project: String,
    /// The earlier launch.
    launch_a: Uuid,
    /// The later launch.
    launch_b: Uuid,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TriageArgs {
    project: String,
    /// Results to change (at most 500), or use `launchId` with `fingerprint` for a cluster.
    result_ids: Option<Vec<Uuid>>,
    launch_id: Option<Uuid>,
    fingerprint: Option<String>,
    /// Defect type: an id from `list_defect_types`, or a group name (`productBug`,
    /// `automationBug`, `systemIssue`, `noDefect`, `toInvestigate`; `product_bug` works too) for
    /// the group's built-in type.
    defect_type_id: Option<String>,
    comment: Option<String>,
    /// Issue link: a URL, or a key the project's issue trackers turn into a URL.
    issue: Option<TriageIssue>,
    mute: Option<TriageMute>,
    unmute: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuleDraftArgs {
    project: String,
    name: String,
    /// `{messageRegex?, traceRegex?, testCaseId?, fingerprint?, environment?, statuses?}`.
    #[serde(rename = "match")]
    matcher: Value,
    /// `{defectTypeId?, comment?, issue?, mute?: {days?}, accept?}`; `defectTypeId` is an id
    /// from `list_defect_types` or a group name such as `productBug`.
    action: Value,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectOnlyArgs {
    project: String,
}

/// A defect type given to `triage` or `create_rule_draft`: a UUID as is, or a built-in group
/// name (any case, `_` ignored) as the id of the group's built-in type.
async fn defect_type_ref(
    s: &AppState,
    p: &fastproof_store::Project,
    v: &str,
) -> Result<String, ToolError> {
    let v = v.trim();
    if Uuid::parse_str(v).is_ok() {
        return Ok(v.to_string());
    }
    let norm = |x: &str| x.replace('_', "").to_ascii_lowercase();
    let want = norm(v);
    s.store
        .defect_types(p.id)
        .await?
        .into_iter()
        .find(|t| t.built_in && norm(&t.group) == want)
        .map(|t| t.id.to_string())
        .ok_or_else(|| {
            ToolError(format!(
                "unknown defect type {:?}: give an id from list_defect_types or a group: productBug, automationBug, systemIssue, noDefect, toInvestigate",
                v.chars().take(100).collect::<String>()
            ))
        })
}

pub(crate) struct ToolSpec {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) schema: fn() -> Value,
    pub(crate) write: bool,
}

pub(crate) fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or_else(|_| json!({"type": "object"}))
}

const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "list_projects",
        description: "Projects the token can read.",
        schema: schema::<NoArgs>,
        write: false,
    },
    ToolSpec {
        name: "list_launches",
        description: "Launches of a project, newest first, with status counts.",
        schema: schema::<ListLaunchesArgs>,
        write: false,
    },
    ToolSpec {
        name: "get_launch",
        description: "One launch with counts, CI and VCS metadata and its quality gate result.",
        schema: schema::<LaunchArgs>,
        write: false,
    },
    ToolSpec {
        name: "launch_failures",
        description: "Failures of a launch grouped into clusters by error fingerprint, with a sample result per cluster.",
        schema: schema::<LaunchArgs>,
        write: false,
    },
    ToolSpec {
        name: "get_result",
        description: "A result with its attempts, steps, errors, parameters, links and recent history.",
        schema: schema::<ResultArgs>,
        write: false,
    },
    ToolSpec {
        name: "result_logs",
        description: "Error, stack trace, stdout, stderr and attachment list of one attempt of a result.",
        schema: schema::<AttemptArgs>,
        write: false,
    },
    ToolSpec {
        name: "similar_failures",
        description: "Results in other launches that failed the same way.",
        schema: schema::<SimilarArgs>,
        write: false,
    },
    ToolSpec {
        name: "test_history",
        description: "Results of a test case across launches, newest first.",
        schema: schema::<HistoryArgs>,
        write: false,
    },
    ToolSpec {
        name: "flaky_tests",
        description: "Tests that are flaky in recent launches.",
        schema: schema::<FlakyArgs>,
        write: false,
    },
    ToolSpec {
        name: "compare_launches",
        description: "New failures, fixed, still failing, new and missing tests between two launches.",
        schema: schema::<CompareArgs>,
        write: false,
    },
    ToolSpec {
        name: "fastmock_interactions",
        description: "Requests the project's FastMock answered during an attempt (by trace id, else by time window).",
        schema: schema::<AttemptArgs>,
        write: false,
    },
    ToolSpec {
        name: "list_defect_types",
        description: "Defect types of a project (built-in groups first, then custom subtypes): id, group, name, shortName, builtIn. triage and create_rule_draft take an id from here or a group name. Needs results:triage.",
        schema: schema::<ProjectOnlyArgs>,
        write: false,
    },
    ToolSpec {
        name: "triage",
        description: "Set the defect type, comment, issue link or mute of results or of a whole failure cluster. Recorded as AI triage and audited. Needs results:triage.",
        schema: schema::<TriageArgs>,
        write: true,
    },
    ToolSpec {
        name: "create_rule_draft",
        description: "Create a disabled triage rule for people to review and enable; audited as AI-created. Needs rules:write or ai:propose (the agent role).",
        schema: schema::<RuleDraftArgs>,
        write: true,
    },
];

/// The MCP server of one request.
#[derive(Clone)]
pub struct McpServer {
    s: AppState,
}

pub(crate) struct ToolError(pub(crate) String);

impl From<ApiError> for ToolError {
    fn from(e: ApiError) -> Self {
        let b = e.body();
        ToolError(format!("{}: {}", b.code, b.message))
    }
}

impl From<anyhow::Error> for ToolError {
    fn from(e: anyhow::Error) -> Self {
        ToolError(format!("{e:#}"))
    }
}

pub(crate) type ToolResult = Result<Value, ToolError>;

pub(crate) fn args<T: DeserializeOwned>(v: &Value) -> Result<T, ToolError> {
    serde_json::from_value(v.clone()).map_err(|e| ToolError(format!("invalid arguments: {e}")))
}

pub(crate) fn limit(v: Option<i64>, default: i64, max: i64) -> Result<i64, ToolError> {
    match v {
        None => Ok(default),
        Some(n) if (1..=max).contains(&n) => Ok(n),
        Some(_) => Err(ToolError(format!("limit must be 1 to {max}"))),
    }
}

pub(crate) fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    format!("{}… [truncated, {} bytes in total]", &s[..i], s.len())
}

fn output(v: &Value) -> String {
    let text = serde_json::to_string(v).unwrap_or_default();
    if text.len() <= MAX_OUTPUT {
        text
    } else {
        clip(&text, MAX_OUTPUT)
    }
}

impl McpServer {
    fn auth(ctx: &RequestContext<RoleServer>) -> Result<Auth, ToolError> {
        ctx.extensions
            .get::<axum::http::request::Parts>()
            .and_then(|p| p.extensions.get::<Auth>())
            .cloned()
            .ok_or_else(|| ToolError("unauthorized: authentication required".into()))
    }

    async fn project(
        &self,
        auth: &Auth,
        key: &str,
        perm: &Permission,
    ) -> Result<fastproof_store::Project, ToolError> {
        Ok(self.s.project_for(auth, key, perm).await?)
    }

    async fn run(&self, name: &str, a: &Value, auth: &Auth) -> ToolResult {
        let s = &self.s;
        let read = &s.perms.launches_read;
        match name {
            "list_projects" => {
                let _: NoArgs = args(a)?;
                let items: Vec<Value> = s
                    .store
                    .projects()
                    .await?
                    .into_iter()
                    .filter(|p| auth.allowed(&s.perms.projects_read, Some(&p.key)))
                    .map(|p| {
                        json!({"key": p.key, "name": p.name, "description": p.description,
                        "launchCount": p.launch_count, "lastLaunch": p.last_launch})
                    })
                    .collect();
                Ok(json!({ "items": items }))
            }
            "list_launches" => {
                let x: ListLaunchesArgs = args(a)?;
                let p = self.project(auth, &x.project, read).await?;
                let f = LaunchFilter {
                    branch: x.branch,
                    environment: x.environment,
                    status: x.status,
                    limit: limit(x.limit, 20, 100)?,
                    ..Default::default()
                };
                Ok(serde_json::to_value(s.store.launches(p.id, &f).await?).unwrap_or_default())
            }
            "get_launch" => {
                let x: LaunchArgs = args(a)?;
                let p = self.project(auth, &x.project, read).await?;
                let l = s.launch_of(&p, x.launch_id).await?;
                let mut v = serde_json::to_value(&l).unwrap_or_default();
                if let Some((state, status, report)) = s.store.stored_gate(l.id).await? {
                    v["gate"] = json!({"state": state, "status": status, "report": report});
                }
                Ok(v)
            }
            "launch_failures" => {
                let x: LaunchArgs = args(a)?;
                let p = self.project(auth, &x.project, read).await?;
                let l = s.launch_of(&p, x.launch_id).await?;
                Ok(s.analysis.clusters(s, &p, &l).await?)
            }
            "get_result" => {
                let x: ResultArgs = args(a)?;
                let p = self.project(auth, &x.project, read).await?;
                let d = s
                    .store
                    .result_detail(p.id, x.result_id, s.history.as_ref())
                    .await?
                    .ok_or_else(|| ToolError("not_found: result not found".into()))?;
                Ok(serde_json::to_value(d).unwrap_or_default())
            }
            "result_logs" => {
                let x: AttemptArgs = args(a)?;
                let p = self.project(auth, &x.project, read).await?;
                let d = s
                    .store
                    .result_detail(p.id, x.result_id, s.history.as_ref())
                    .await?
                    .ok_or_else(|| ToolError("not_found: result not found".into()))?;
                let at = match x.attempt {
                    None => d.attempts.last(),
                    Some(n) => d.attempts.iter().find(|t| t.no == n),
                }
                .ok_or_else(|| ToolError("not_found: attempt not found".into()))?;
                let attachments: Vec<Value> = at
                    .attachments
                    .iter()
                    .map(|f| json!({"name": f.name, "kind": f.kind, "contentType": f.content_type, "size": f.size,
                        "url": f.id.map(|id| format!("/api/v1/projects/{}/attachments/{id}/content", p.key))}))
                    .collect();
                let c = |v: &Option<String>| v.as_deref().map(|t| clip(t, MAX_LOG));
                Ok(json!({
                    "resultId": d.summary.id, "name": d.summary.full_name, "attempt": at.no,
                    "attempts": d.attempts.len(), "status": at.status,
                    "error": at.error.as_ref().map(|e| json!({"message": c(&e.message), "trace": c(&e.trace),
                        "expected": c(&e.expected), "actual": c(&e.actual)})),
                    "stdout": c(&at.stdout), "stderr": c(&at.stderr), "traceId": at.trace_id,
                    "attachments": attachments,
                }))
            }
            "similar_failures" => {
                let x: SimilarArgs = args(a)?;
                let p = self.project(auth, &x.project, read).await?;
                Ok(s.analysis
                    .similar(s, &p, x.result_id, limit(x.limit, 10, 50)?)
                    .await?)
            }
            "test_history" => {
                let x: HistoryArgs = args(a)?;
                let p = self.project(auth, &x.project, read).await?;
                let f = HistoryFilter {
                    branch: x.branch,
                    environment: x.environment,
                    debug: false,
                    limit: limit(x.limit, 30, 100)?,
                };
                let items = s
                    .history
                    .test_case_history(p.id, x.test_case_id, &f)
                    .await?;
                Ok(json!({ "items": items }))
            }
            "flaky_tests" => {
                let x: FlakyArgs = args(a)?;
                let p = self.project(auth, &x.project, read).await?;
                Ok(s.analysis
                    .flaky(
                        s,
                        &p,
                        x.branch.as_deref(),
                        x.environment.as_deref(),
                        limit(x.limit, 20, 100)?,
                    )
                    .await?)
            }
            "compare_launches" => {
                let x: CompareArgs = args(a)?;
                let p = self.project(auth, &x.project, read).await?;
                let la = s.launch_of(&p, x.launch_a).await?;
                let lb = s.launch_of(&p, x.launch_b).await?;
                Ok(s.analysis.compare(s, &p, &la, &lb).await?)
            }
            "fastmock_interactions" => {
                let x: AttemptArgs = args(a)?;
                let p = self.project(auth, &x.project, read).await?;
                let m = fastmock::interactions(s, &p, x.result_id, x.attempt).await?;
                Ok(serde_json::to_value(m).unwrap_or_default())
            }
            "list_defect_types" => {
                let x: ProjectOnlyArgs = args(a)?;
                let p = self
                    .project(auth, &x.project, &s.perms.results_triage)
                    .await?;
                let items: Vec<Value> = s
                    .store
                    .defect_types(p.id)
                    .await?
                    .into_iter()
                    .map(|t| {
                        json!({"id": t.id, "group": t.group, "name": t.name, "shortName": t.short_name,
                            "builtIn": t.built_in})
                    })
                    .collect();
                Ok(json!({ "items": items }))
            }
            "triage" => {
                let x: TriageArgs = args(a)?;
                let p = self
                    .project(auth, &x.project, &s.perms.results_triage)
                    .await?;
                let defect_type_id = match &x.defect_type_id {
                    Some(v) => Some(defect_type_ref(s, &p, v).await?),
                    None => None,
                };
                let ids = x.result_ids.clone().unwrap_or_default();
                if ids.len() > MAX_TRIAGE_IDS {
                    return Err(ToolError(format!("at most {MAX_TRIAGE_IDS} resultIds")));
                }
                if ids.is_empty() && (x.launch_id.is_none() || x.fingerprint.is_none()) {
                    return Err(ToolError(
                        "give resultIds, or launchId with fingerprint".into(),
                    ));
                }
                if x.comment.as_ref().is_some_and(|c| c.len() > 4000) {
                    return Err(ToolError("comment: at most 4000 bytes".into()));
                }
                let mut issue = x.issue.clone();
                if let Some(i) = &mut issue {
                    if let Some(u) = &i.url
                        && !(u.starts_with("https://") || u.starts_with("http://"))
                    {
                        return Err(ToolError("issue.url must be an http(s) URL".into()));
                    }
                    if i.url.is_none()
                        && let Some(k) = &i.key
                    {
                        let doc = s.store.project_settings_json(p.id).await?;
                        let trackers = IntegrationSettings::parse(&doc)
                            .map(|t| t.issue_trackers)
                            .unwrap_or_default();
                        i.url = issue_link(&trackers, k).map(|l| l.url);
                    }
                }
                let req = TriageRequest {
                    result_ids: x.result_ids,
                    launch_id: x.launch_id,
                    fingerprint: x.fingerprint,
                    defect_type_id,
                    comment: x.comment,
                    issue,
                    mute: x.mute,
                    unmute: x.unmute,
                    source: Some("ai".into()),
                };
                let out = s.analysis.triage(s, &p, &auth.principal.name, &req).await?;
                s.audit(
                    auth,
                    "triage.apply",
                    Some(&p.key),
                    json!({"via": "mcp", "source": "ai", "request": req, "result": out}),
                )
                .await;
                Ok(out)
            }
            "create_rule_draft" => {
                let x: RuleDraftArgs = args(a)?;
                let p = crate::mcp_tm::project_any(
                    s,
                    auth,
                    &x.project,
                    &[&s.perms.rules_write, &s.perms.ai_propose],
                )
                .await?;
                if x.name.trim().is_empty() || x.name.len() > 200 {
                    return Err(ToolError("name must have 1 to 200 bytes".into()));
                }
                let mut action = x.action;
                if let Some(v) = action.get("defectTypeId").and_then(Value::as_str) {
                    if v.len() > 200 {
                        return Err(ToolError("action.defectTypeId: at most 200 bytes".into()));
                    }
                    let id = defect_type_ref(s, &p, v).await?;
                    action["defectTypeId"] = Value::String(id);
                }
                let rule =
                    json!({"name": x.name, "enabled": false, "match": x.matcher, "action": action});
                if rule.to_string().len() > 16 * 1024 {
                    return Err(ToolError("the rule is larger than 16 KiB".into()));
                }
                let out = s
                    .analysis
                    .create_rule_draft(s, &p, &auth.principal.name, &rule)
                    .await?;
                s.audit(
                    auth,
                    "rule.create",
                    Some(&p.key),
                    json!({"via": "mcp", "source": "ai", "rule": rule}),
                )
                .await;
                Ok(out)
            }
            other => match crate::mcp_tm::run(s, other, a, auth).await {
                Some(r) => r,
                None => Err(ToolError(format!("unknown tool {other:?}"))),
            },
        }
    }
}

fn all_tools() -> impl Iterator<Item = &'static ToolSpec> {
    TOOLS.iter().chain(crate::mcp_tm::TOOLS.iter())
}

/// Tool definitions as listed.
pub fn tools() -> Vec<Tool> {
    all_tools()
        .map(|t| {
            let schema = match (t.schema)() {
                Value::Object(m) => m,
                _ => serde_json::Map::new(),
            };
            let mut tool = Tool::new(t.name, t.description, Arc::new(schema));
            tool.annotations = Some(if t.write {
                rmcp::model::ToolAnnotations::new()
                    .read_only(false)
                    .destructive(false)
            } else {
                rmcp::model::ToolAnnotations::new().read_only(true)
            });
            tool
        })
        .collect()
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("fastproof", crate::VERSION))
            .with_instructions(
                "FastProof test reports: list launches, read failures, clusters, logs, history and FastMock interactions; triage results (recorded as AI triage). Test management: search and read test cases, plans and manual runs, read Playwright trace actions, draft cases and propose changes (drafts and proposals are reviewed by people).",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if !all_tools().any(|t| t.name == request.name) {
            return Err(McpError::invalid_params(
                format!("unknown tool {:?}", request.name),
                None,
            ));
        }
        let auth = match Self::auth(&context) {
            Ok(a) => a,
            Err(e) => return Ok(CallToolResult::error(vec![ContentBlock::text(e.0)]).into()),
        };
        let a = Value::Object(request.arguments.clone().unwrap_or_default());
        metrics::counter!("fastproof_mcp_calls_total", "tool" => request.name.to_string())
            .increment(1);
        let r = match self.run(&request.name, &a, &auth).await {
            Ok(v) => CallToolResult::success(vec![ContentBlock::text(output(&v))]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e.0)]),
        };
        Ok(r.into())
    }
}

async fn require_auth(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    match Auth::from_request_parts(&mut parts, &s).await {
        Ok(_) => next.run(Request::from_parts(parts, body)).await,
        Err(e) => e.into_response(),
    }
}

/// `/mcp`, behind bearer authentication.
pub fn router(state: &AppState) -> Router<AppState> {
    let st = state.clone();
    let cfg = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .disable_allowed_hosts()
        .with_max_request_body_bytes(MAX_BODY);
    let svc = StreamableHttpService::new(
        move || Ok(McpServer { s: st.clone() }),
        Arc::new(NeverSessionManager::default()),
        cfg,
    );
    Router::new()
        .route_service("/mcp", svc)
        .layer(middleware::from_fn_with_state(state.clone(), require_auth))
}
