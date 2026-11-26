//! The REST API (`/api/v1`), documented with utoipa.

pub mod admin;
pub mod analysis;
pub mod analytics;
pub mod attachments;
pub mod case_fields;
pub mod case_folders;
pub mod case_workflow;
pub mod cases;
pub mod coverage;
pub mod dashboards;
pub mod decisions;
pub mod direct_uploads;
pub mod errors;
pub mod events;
pub mod exports;
pub mod flaky_hints;
pub mod gates_history;
pub mod history_imports;
pub mod imports;
pub mod integrations;
pub mod launch_exports;
pub mod launches;
pub mod logs;
pub mod media;
pub mod merges;
pub mod parameters;
pub mod plans;
pub mod projects;
pub mod queries;
pub mod range;
pub mod requirements;
pub mod reruns;
pub mod results;
pub mod reviews;
pub mod runs;
pub mod search;
pub mod shared_steps;
pub mod signals;
pub mod suggestions;
pub mod trackers;
pub mod uploads;
pub mod version;

use axum::extract::{FromRequestParts, Path, Query};
use axum::http::request::Parts;
use fastproof_store::{Launch, Project};
use franshub_access::Permission;
use franshub_auth::{Auth, require};
use franshub_http::{ApiError, ApiResult};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};
use uuid::Uuid;

use crate::state::AppState;
use fastproof_store::analysis::types as an;
use fastproof_store::tm::{self, TmError};

/// Default and largest page size of lists.
pub const DEFAULT_LIMIT: i64 = 50;
pub const MAX_LIMIT: i64 = 500;

/// The uniform error body (`franshub_http::ErrorBody`), for the documentation.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

struct BearerAuth;

impl Modify for BearerAuth {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let c = openapi.components.get_or_insert_with(Default::default);
        c.add_security_scheme(
            "bearer",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
    }
}

/// Types the integration parts of `ProjectSettings` (stored as free JSON by the store and
/// validated by [`crate::integrations::settings`]) in the document.
pub fn type_project_settings(doc: &mut utoipa::openapi::OpenApi) {
    let Some(c) = doc.components.as_mut() else {
        return;
    };
    let Some(schema) = c.schemas.get("ProjectSettings") else {
        return;
    };
    let Ok(mut v) = serde_json::to_value(schema) else {
        return;
    };
    let reference =
        |name: &str| serde_json::json!({"$ref": format!("#/components/schemas/{name}")});
    let nullable = |name: &str| serde_json::json!({"oneOf": [reference(name), {"type": "null"}]});
    if let Some(props) = v.get_mut("properties").and_then(|p| p.as_object_mut()) {
        for (key, name) in [
            ("gates", "Gate"),
            ("notifications", "Notification"),
            ("issueTrackers", "IssueTracker"),
        ] {
            if let Some(p) = props.get_mut(key) {
                p["items"] = reference(name);
            }
        }
        props.insert("fastmock".into(), nullable("FastMockSettings"));
        props.insert("vcs".into(), nullable("VcsSettings"));
        props.insert("rerun".into(), nullable("RerunSettings"));
    }
    if let Ok(schema) = serde_json::from_value(v) {
        c.schemas.insert("ProjectSettings".into(), schema);
    }
}

#[derive(OpenApi)]
#[openapi(
    info(title = "FastProof API", description = "Test reporting server: projects, launches, uploads, results and attachments.", license(name = "Apache-2.0")),
    modifiers(&BearerAuth),
    security(("bearer" = [])),
    components(schemas(ErrorBody, fastproof_model::ReportResult, logs::LogStreamBatch, logs::LogGap,
        crate::integrations::settings::Gate, crate::integrations::settings::Notification,
        crate::integrations::settings::NotificationFilter, crate::integrations::settings::VcsSettings,
        crate::integrations::settings::IssueTracker, crate::integrations::settings::FastMockSettings, crate::integrations::settings::RerunSettings,
        tm::Case, tm::CaseList, tm::CaseSummary, tm::CaseWrite, tm::CasePatch, tm::CaseTree, tm::CaseTreeNode,
        tm::CaseTreeGroupBy, tm::CaseFolder, tm::CaseFolderList, tm::CaseFolderWrite, tm::CaseFolderPatch,
        tm::CaseFieldDef, tm::CaseFieldList, tm::CaseFieldWrite, tm::CaseFieldPatch, tm::CaseVersionSummary,
        tm::CaseVersion, tm::CaseSnapshot, tm::CaseHistoryEntry, tm::TestIdentity, tm::CaseBulk, tm::CaseBulkResult,
        tm::CaseFilter, tm::CaseSuggestion, tm::CaseProposal, tm::AttachmentRef, tm::TestPlan, tm::TestPlanWrite,
        tm::TestPlanItem, tm::TestPlanItemsChange, tm::PlanSyncDiff, tm::StartPlanLaunch, tm::PlanLaunch,
        tm::LaunchManual, tm::RunItemSummary, tm::RunStepResult, tm::RunItem, tm::RunStepWrite, tm::RunStepAck,
        tm::RunItemPatch, tm::RunItemsAdd, tm::RunItemsAdded, tm::RunDefect, tm::ManualClose, tm::TmJob,
        tm::ImportPreview, tm::ImportOptions, tm::TestOpsImport, tm::ImportReport, tm::FolderDeleteMode,
        tm::RunItemExecution, tm::RunItemSkip, tm::RunItemSkipCode, tm::ImportCaseStatus,
        tm::TmJobKind, tm::TmGateMode, tm::SharedStepRef, tm::SharedStepOrigin, tm::SharedStepBrief, tm::SharedStepHtml,
        tm::SharedStep, tm::SharedStepUsageCount, tm::SharedStepSummary, tm::SharedStepList, tm::SharedStepWrite,
        tm::SharedStepVersionSummary, tm::SharedStepSnapshot, tm::SharedStepVersion, tm::SharedStepVersionList,
        tm::SharedStepUsage, tm::SharedStepUsageList, tm::ExtractSharedStep, tm::ExtractedSharedStep,
        tm::SharedStepChange, tm::SharedStepChangeList, tm::SharedPin, tm::ReviewState, tm::ReviewDecision,
        tm::ReviewKind, tm::ReviewMode, tm::WorkflowGuard, tm::WorkflowPermission, tm::ReviewResetOn, tm::ReviewImports,
        tm::CaseWorkflowStatus, tm::CaseWorkflowTransition, tm::CaseWorkflowFlow, tm::CaseWorkflowSettings,
        tm::CaseReviewSettings, tm::CaseWorkflowView, tm::CaseWorkflowPut, tm::CaseWorkflowSaved, tm::CaseTransition,
        tm::AllowedTransition, tm::ReviewVote, tm::ReviewThread, tm::ReviewVoteWrite, tm::ReviewQueueItem,
        tm::ReviewQueue, tm::ReviewBulkItem, tm::ReviewBulk, tm::ReviewBulkSkip, tm::ReviewBulkResult,
        tm::QueryEntity, tm::QueryAttributeType, tm::QueryError, tm::QueryValidate, tm::QueryValidation,
        tm::QueryAttribute, tm::QueryFunction, tm::QueryField, tm::QuerySchema, tm::SavedQuery, tm::SavedQueryWrite,
        tm::SavedQueryPatch, tm::SavedQueryList, tm::PlanCiEnvironment, tm::PlanCi, tm::PlanDispatchStatus,
        tm::PlanDispatch, tm::PlanDispatchList, tm::PlanCheck, tm::RerunVariable, tm::ParameterMode, tm::ParameterRow,
        tm::ParameterGenerator, tm::CaseParameters, tm::ParameterGenerate, tm::ParameterRows, tm::RunIteration,
        tm::RunIterationPatch, tm::runs::ManualReportIteration, tm::RunIterationStepAck, tm::RequirementStatus, tm::RequirementOrigin,
        tm::RequirementLinkOrigin, tm::RequirementLinkStatus, tm::CoverageStatus, tm::CoverageScopeKind,
        tm::CoverageExportFormat, tm::RequirementPathEntry, tm::RequirementTracker, tm::Requirement,
        tm::RequirementSummary, tm::RequirementList, tm::RequirementWrite, tm::RequirementPatch,
        tm::RequirementLinksChange, tm::RequirementLink, tm::RequirementLinkList, tm::CaseRequirement,
        tm::RequirementImport, tm::CoverageCounts, tm::RequirementCoverage, tm::CoverageScopeView, tm::CoveragePage,
        tm::CoverageExportRequest, tm::ReplicaLevel, tm::ReplicaLevels,
        crate::integrations::settings::Metric, crate::integrations::gates::GateStatus,
        crate::integrations::gates::RuleResult, fastproof_store::analysis::rules::RulePhase,
        an::DecisionVia, an::DecisionSource, an::TriageState, an::TriageDecision, an::TriageHistory, an::TriageUndo,
        an::TriageUndoSkip, an::TriageUndoSkipCode, an::TriageUndoResult, an::GateView, an::AnalysisFeatures,
        an::AnalysisWindows, an::AnalysisCapabilities, an::AnalysisLaunchState, an::AnalysisLaunchStatus,
        an::AnalysisStepStatus, an::StepOutcome, an::LaunchSteps, an::AnalysisSettings, an::AnalysisSuggestSettings,
        an::AnalysisAutoApplySettings, an::AnalysisBacktestSettings, an::AnalysisFlakySettings, an::QueueReason,
        an::SuggestionBand, an::SuggestionSignal, an::SuggestionState, an::WhySignal, an::WhyItem,
        an::DecisionSample, an::Suggestion, an::CommitRange, an::ClusterDecided, an::NoSuggestion,
        an::ClusterSuggestions, an::LaunchSuggestionsState, an::LaunchSuggestions, an::SuggestionApplyTo,
        an::SuggestionAccept, an::SuggestionAccepted, an::SuggestionReject, an::MemoryEntry, an::MemoryList,
        an::DecisionEvent, an::BacktestRequest, an::BacktestBand, an::BacktestConfusion, an::BacktestSource,
        an::BacktestReport, an::ErrorState, an::ErrorSort, an::ErrorIssueState, an::ErrorIssue, an::ErrorSummary,
        an::ErrorList, an::ErrorBranch, an::ErrorTest, an::ErrorTimelinePoint, an::AnalyticsBackend,
        an::AnalyticsWindow, an::ErrorDetail, an::ErrorTimeline, an::ErrorPatchState, an::ErrorPatch,
        an::GateRuleOutcome, an::GateHistoryItem, an::GateHistoryGate, an::GateHistoryBranch, an::GateHistory,
        an::GateSimulate, an::GateSimulationItem, an::GateSimulation, an::LogStreamName, an::RuleLogMatch,
        an::RuleSignalMatch, an::ResultPattern, an::PatternCount, an::PatternCounts, an::WidgetPatterns,
        an::SignalSource, an::SignalConfidence, an::SignalItem, an::SignalPush, an::SignalRejection,
        an::SignalsAccepted, an::AttemptSignal, an::AttemptSignalBrief, an::AttemptSignals, an::FlakyHintKind,
        an::HintConfidence, an::HintAction, an::FlakyHint, an::FlakyHintBrief, an::FlakyHints, an::FailureGroupBy,
        an::FailureGroup, an::FailureAnalytics, an::DurationRegression, an::DurationRegressions, an::RetryCost,
        an::RetryCosts, an::FlakyTrendPoint, an::FlakyTrend, an::MttrBucket, an::MttrStats, an::SlowStep,
        an::SlowSteps, an::PortfolioProject, an::Portfolio, an::CrossProjectSimilarItem, an::CrossProjectSimilar)),
    tags(
        (name = "projects"), (name = "launches"), (name = "uploads"), (name = "results"),
        (name = "attachments"), (name = "events"), (name = "admin"), (name = "integrations"),
        (name = "analysis"), (name = "logs"),
        (name = "cases", description = "Test cases, folders, fields, versions and AI suggestions"),
        (name = "plans", description = "Managed test plans"),
        (name = "runs", description = "Manual runs: run items and step results"),
        (name = "imports", description = "Imports and exports of test cases"),
        (name = "shared-steps", description = "Shared steps: reusable step sequences that cases reference, with versions and usages"),
        (name = "reviews", description = "Case workflow and version review of cases and shared steps"),
        (name = "queries", description = "FPQL: validation, the query schema and saved queries"),
        (name = "requirements", description = "Requirements, their links to cases, and coverage"),
        (name = "analytics", description = "Failure trends and portfolio, on ClickHouse with a PostgreSQL fallback"),
        (name = "signals", description = "Signals from grids, CI and Kubernetes about test attempts")
    )
)]
pub struct ApiDoc;

/// Query parameters whose rejection answers the uniform 400.
pub struct Q<T>(pub T);

impl<T: DeserializeOwned + Send, S: Send + Sync> FromRequestParts<S> for Q<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(v)) => Ok(Q(v)),
            Err(e) => Err(ApiError::bad_request(format!(
                "invalid query: {}",
                e.body_text()
            ))),
        }
    }
}

/// Path parameters; a malformed id is a 404 like an unknown one.
pub struct P<T>(pub T);

impl<T: DeserializeOwned + Send, S: Send + Sync> FromRequestParts<S> for P<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(Path(v)) => Ok(P(v)),
            Err(_) => Err(ApiError::not_found("resource")),
        }
    }
}

/// The answer to a [`TmError`]: 404 `not_found`, 400 or 409 with its code and details, 412
/// `precondition_failed` with `{currentRevision, changedBy, source}`, otherwise 500 (logged).
pub fn tm_error(e: TmError) -> ApiError {
    use axum::http::StatusCode;
    match e {
        TmError::NotFound(what) => ApiError::not_found(what),
        TmError::Invalid {
            code,
            message,
            details,
        } => with_details(
            ApiError::new(StatusCode::BAD_REQUEST, code, message),
            details,
        ),
        TmError::Conflict {
            code,
            message,
            details,
        } => with_details(ApiError::new(StatusCode::CONFLICT, code, message), details),
        TmError::Forbidden { code, message } => ApiError::new(StatusCode::FORBIDDEN, code, message),
        TmError::Stale {
            current_revision,
            changed_by,
            source,
        } => ApiError::new(
            StatusCode::PRECONDITION_FAILED,
            "precondition_failed",
            "the resource changed since it was read",
        )
        .with_details(serde_json::json!({"currentRevision": current_revision,
            "changedBy": changed_by, "source": source})),
        TmError::Other(e) => ApiError::from(e),
    }
}

fn with_details(e: ApiError, details: Option<serde_json::Value>) -> ApiError {
    match details {
        Some(d) => e.with_details(d),
        None => e,
    }
}

/// The revision an `If-Match: "r<revision>"` header names (also `W/"r<n>"`): missing is 428
/// `precondition_required`; a value that names no revision (`*`, `"r0"`, garbage) is `-1`, which
/// matches nothing, so the store answers 412. Revisions start at 1.
pub fn if_match(h: &axum::http::HeaderMap) -> ApiResult<i64> {
    let Some(v) = h.get(axum::http::header::IF_MATCH) else {
        return Err(ApiError::new(
            axum::http::StatusCode::PRECONDITION_REQUIRED,
            "precondition_required",
            "send If-Match with the ETag of the resource",
        ));
    };
    let v = v.to_str().unwrap_or_default().trim();
    let v = v.strip_prefix("W/").unwrap_or(v);
    Ok(v.trim_matches('"')
        .strip_prefix('r')
        .and_then(|r| r.parse::<i64>().ok())
        .filter(|r| *r > 0)
        .unwrap_or(-1))
}

/// Page size from `limit`.
pub fn page_limit(limit: Option<i64>) -> ApiResult<i64> {
    match limit {
        None => Ok(DEFAULT_LIMIT),
        Some(n) if (1..=MAX_LIMIT).contains(&n) => Ok(n),
        Some(_) => Err(ApiError::bad_request(format!(
            "limit must be 1..={MAX_LIMIT}"
        ))),
    }
}

/// `k:v` filters.
pub fn key_value(s: Option<&str>, what: &str) -> ApiResult<Option<(String, String)>> {
    match s {
        None => Ok(None),
        Some(s) => match s.split_once(':') {
            Some((k, v)) if !k.is_empty() => Ok(Some((k.to_string(), v.to_string()))),
            _ => Err(ApiError::bad_request(format!("{what} must be key:value"))),
        },
    }
}

impl AppState {
    /// The project, after checking `perm` in its scope. The permission is checked first, so a
    /// caller without access gets the same 403 whether the project exists or not.
    pub async fn project_for(
        &self,
        auth: &Auth,
        key: &str,
        perm: &Permission,
    ) -> ApiResult<Project> {
        require(auth, perm, Some(key))?;
        if !fastproof_blob::valid_project_key(key) {
            return Err(ApiError::not_found("project"));
        }
        self.store
            .project(key)
            .await?
            .ok_or_else(|| ApiError::not_found("project"))
    }

    pub async fn launch_of(&self, project: &Project, id: Uuid) -> ApiResult<Launch> {
        self.store
            .launch(project.id, id)
            .await?
            .ok_or_else(|| ApiError::not_found("launch"))
    }

    /// An upload or results batch into a launch. A caller with `cases:sync` makes a launch
    /// started from a plan trusted for code sync (`Store::join_plan_launch`). A caller
    /// without it makes the launch untrusted for good (`sync_blocked`), and a finished
    /// launch refuses it with 409 `launch_finished` unless it created the launch.
    pub async fn note_plan_contributor(
        &self,
        auth: &Auth,
        project: &Project,
        launch: &Launch,
    ) -> ApiResult<()> {
        self.refuse_merged(launch)?;
        let can_sync = auth
            .principal
            .denial(&self.perms.cases_sync, Some(&project.key))
            .is_none();
        if !can_sync {
            return self
                .untrusted_contributor(auth, project, launch, true)
                .await;
        }
        if launch.test_plan_id.is_some() {
            self.store
                .join_plan_launch(project.id, launch.id, None)
                .await?;
        }
        Ok(())
    }

    /// See [`AppState::note_plan_contributor`]; `mark = false` for a finish request.
    pub async fn untrusted_contributor(
        &self,
        auth: &Auth,
        project: &Project,
        launch: &Launch,
        mark: bool,
    ) -> ApiResult<()> {
        match self
            .store
            .untrusted_contributor(project.id, launch.id, &auth.principal.name, mark)
            .await?
        {
            None => Err(ApiError::not_found("launch")),
            Some(true) => Ok(()),
            Some(false) => Err(ApiError::new(
                axum::http::StatusCode::CONFLICT,
                "launch_finished",
                "the launch is finished: only its creator or a caller with cases:sync may add to it",
            )),
        }
    }

    /// Whether the caller has `cases:review` in the project.
    pub fn can_review(&self, auth: &Auth, key: &str) -> bool {
        auth.principal.allowed(&self.perms.cases_review, Some(key))
    }

    pub async fn audit(
        &self,
        auth: &Auth,
        action: &str,
        scope: Option<&str>,
        detail: serde_json::Value,
    ) {
        self.platform
            .audit(franshub_access::AuditEvent::new(
                &auth.principal,
                action,
                scope,
                detail,
            ))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, StatusCode, header};

    use super::if_match;

    #[test]
    fn if_match_is_one_rule_for_cases_plans_and_runs() {
        let header_of = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::IF_MATCH, HeaderValue::from_str(v).unwrap());
            h
        };
        for (v, want) in [
            ("\"r1\"", 1),
            ("W/\"r7\"", 7),
            ("r3", 3),
            ("\"r0\"", -1),
            ("\"r-2\"", -1),
            ("*", -1),
            ("\"x\"", -1),
            ("\"r\"", -1),
        ] {
            let h = header_of(v);
            assert_eq!(if_match(&h).unwrap(), want, "{v}");
            assert_eq!(super::cases::if_match(&h).unwrap(), want, "cases {v}");
            assert_eq!(super::plans::if_match(&h).unwrap(), want, "plans {v}");
        }
        assert_eq!(
            if_match(&HeaderMap::new()).unwrap_err().status(),
            StatusCode::PRECONDITION_REQUIRED
        );
    }
}
