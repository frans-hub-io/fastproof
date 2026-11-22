//! Rows as the API shows them (camelCase JSON).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// A project.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    #[serde(skip)]
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub last_launch_at: Option<DateTime<Utc>>,
    pub launch_count: i64,
    /// The newest launch, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_launch: Option<LastLaunch>,
}

/// The newest launch of a project.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LastLaunch {
    pub id: Uuid,
    pub number: i64,
    pub name: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub counts: Counts,
}

/// A key/value attribute of a launch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Attribute {
    pub key: String,
    pub value: String,
}

/// CI metadata of a launch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct LaunchCi {
    pub provider: Option<String>,
    pub pipeline_id: Option<String>,
    pub job_id: Option<String>,
    pub url: Option<String>,
    pub build_order: Option<i64>,
    /// Merge request IID (GitLab) or pull request number (GitHub) of the pipeline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_request: Option<String>,
}

/// Version control metadata of a launch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct LaunchVcs {
    pub repository: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    /// Link to the commit; computed for GitHub and GitLab repositories when not given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_url: Option<String>,
    /// The default branch of the repository as the CI reports it (trusted by code sync when
    /// `caseSync.useCiDefaultBranch`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
}

impl LaunchVcs {
    /// `commit_url`, or one derived from a GitHub or GitLab `repository` URL.
    pub fn with_commit_url(mut self, ci_provider: Option<&str>) -> Self {
        if self.commit_url.is_some() {
            return self;
        }
        let (Some(repo), Some(commit)) = (self.repository.as_deref(), self.commit.as_deref())
        else {
            return self;
        };
        if !(4..=64).contains(&commit.len()) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return self;
        }
        let Ok(u) = url::Url::parse(repo.trim()) else {
            return self;
        };
        if !matches!(u.scheme(), "https" | "http") || u.host_str().is_none() {
            return self;
        }
        let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
        let base = repo.trim().trim_end_matches('/').trim_end_matches(".git");
        self.commit_url = if host == "github.com" || ci_provider == Some("github") {
            Some(format!("{base}/commit/{commit}"))
        } else if host.contains("gitlab") || ci_provider == Some("gitlab") {
            Some(format!("{base}/-/commit/{commit}"))
        } else {
            None
        };
        self
    }
}

/// The named environment of a launch; `key` is part of every series identity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct LaunchEnvironment {
    pub key: Option<String>,
    pub values: BTreeMap<String, String>,
}

/// Status counts of a launch (results, not attempts).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Counts {
    pub total: i64,
    pub passed: i64,
    pub failed: i64,
    pub broken: i64,
    pub skipped: i64,
    pub unknown: i64,
    pub flaky: i64,
    pub muted: i64,
}

/// Uploads of a launch that are not done.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadCounts {
    pub queued: i64,
    pub processing: i64,
    pub failed: i64,
}

/// A launch: one run of a test suite, made of any number of uploads.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    pub id: Uuid,
    pub number: i64,
    pub name: String,
    /// `running`, `finished` or `interrupted`.
    pub status: String,
    pub debug: bool,
    pub external_id: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    pub ci: Option<LaunchCi>,
    pub vcs: Option<LaunchVcs>,
    pub environment: Option<LaunchEnvironment>,
    pub attributes: Vec<Attribute>,
    pub counts: Counts,
    pub uploads: UploadCounts,
    /// Kept by retention.
    pub important: bool,
    /// The managed plan the launch was started from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_plan_id: Option<Uuid>,
    /// Progress of the manual run, when the launch has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual: Option<crate::tm::LaunchManual>,
    #[serde(skip)]
    pub project_id: Uuid,
    #[serde(skip)]
    pub created_at: DateTime<Utc>,
}

/// Counts of an upload.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct UploadItemCounts {
    pub results: i64,
    pub attachments: i64,
    pub ignored: i64,
}

/// Why an upload failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
}

/// One upload (shard) of a launch.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Upload {
    pub id: Uuid,
    pub launch_id: Uuid,
    /// Requested format (`auto`, `allure`, `junit`, `ctrf`, `playwright`), replaced by the
    /// detected one once processed.
    pub format: String,
    /// `queued`, `processing`, `done` or `failed`.
    pub status: String,
    pub size_bytes: i64,
    pub counts: UploadItemCounts,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<UploadError>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// A label of a result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LabelView {
    pub name: String,
    pub value: String,
}

/// Triage of a result: defect type (with its group and name when triaged), comment, issue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Defect {
    pub type_id: Uuid,
    /// `productBug`, `automationBug`, `systemIssue`, `noDefect` or `toInvestigate`.
    pub group: String,
    pub name: String,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub issue_url: Option<String>,
    #[serde(default)]
    pub issue_key: Option<String>,
    /// `user`, `ai` or `rule` (a rule without `ruleId` is the project default for failures).
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<Uuid>,
    pub at: DateTime<Utc>,
    pub by: String,
    /// An accepted (known, tolerated) failure.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accepted: bool,
}

/// One result per series per launch.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResultSummary {
    pub id: Uuid,
    pub launch_id: Uuid,
    pub test_case_id: Uuid,
    pub series_key: String,
    pub name: String,
    pub full_name: String,
    pub suite_path: Vec<String>,
    pub status: String,
    pub flaky_in_run: bool,
    pub attempt_count: i32,
    pub duration_ms: Option<i64>,
    pub started_at: Option<DateTime<Utc>>,
    pub labels: Vec<LabelView>,
    pub muted: bool,
    /// End of the mute, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mute_until: Option<DateTime<Utc>>,
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defect: Option<Defect>,
    /// Status change against the previous run of the series on the same branch: `new`,
    /// `fixed`, `regressed` or `malfunctioned`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transition: Option<String>,
    /// Flaky across runs (Allure 3 algorithm, project settings).
    pub flaky: bool,
    #[serde(skip)]
    pub series_id: Uuid,
    #[serde(skip)]
    pub launch_created_at: DateTime<Utc>,
    /// Error message of the final attempt (first 500 characters).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Client ids of the attempts sent through the results API (the `id` reporters send, also
    /// the `resultId` of their log lines), in result lists of a launch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub client_ids: Vec<String>,
    /// `manual` for results projected from a manual run (always sent; optional in the schema
    /// so that older clients and fixtures stay valid).
    #[serde(default)]
    pub source: crate::tm::ResultSource,
    /// Number of the case the result's identity is linked to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_number: Option<i64>,
    /// The run item a manual result is projected from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_item_id: Option<Uuid>,
}

/// An attachment as referenced by a result or step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentView {
    /// Attachment id; `null` when the file was not part of any upload.
    pub id: Option<Uuid>,
    pub name: String,
    pub content_type: Option<String>,
    /// `image`, `video`, `trace`, `imageDiff`, `httpExchange`, `text`, `log`, `json` or `other`.
    pub kind: String,
    pub size: Option<u64>,
}

/// Error details of an attempt or step.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ErrorView {
    pub message: Option<String>,
    pub trace: Option<String>,
    pub actual: Option<String>,
    pub expected: Option<String>,
}

/// A displayable parameter (masked and hidden ones are never returned).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ParameterView {
    pub name: String,
    pub value: Option<String>,
}

/// A step or fixture.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StepView {
    pub name: String,
    pub status: String,
    /// `step` or `fixture`.
    pub kind: String,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorView>,
    pub parameters: Vec<ParameterView>,
    pub attachments: Vec<AttachmentView>,
    #[schema(no_recursion)]
    pub steps: Vec<StepView>,
}

/// One execution of a result.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttemptView {
    pub no: i32,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorView>,
    pub steps: Vec<StepView>,
    pub attachments: Vec<AttachmentView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Client id of an attempt sent through the results API: the `resultId` of its log lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

/// A link of a result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LinkView {
    pub name: Option<String>,
    pub url: String,
    #[serde(rename = "type")]
    pub link_type: Option<String>,
}

/// One past result of the same series.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SeriesPoint {
    pub launch_id: Uuid,
    pub launch_number: i64,
    pub result_id: Uuid,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
}

/// A result with its attempts, steps and history.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResultDetail {
    #[serde(flatten)]
    pub summary: ResultSummary,
    /// Sanitized HTML.
    pub description: Option<String>,
    pub description_text: Option<String>,
    pub parameters: Vec<ParameterView>,
    pub links: Vec<LinkView>,
    pub launch_number: i64,
    /// Why the series is muted, when it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mute_reason: Option<String>,
    pub attempts: Vec<AttemptView>,
    pub history: Vec<SeriesPoint>,
}

/// A test case (logical identity across parameters and environments).
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestCaseView {
    pub id: Uuid,
    pub name: String,
    pub full_name: String,
}

/// A node of the launch tree.
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    pub name: String,
    pub path: Vec<String>,
    pub counts: Counts,
    #[schema(no_recursion)]
    pub children: Vec<TreeNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_ids: Option<Vec<Uuid>>,
    /// Sum of the durations of the results under the node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Results directly in this node (not in its children), with `includeTests`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_count: Option<i64>,
    /// The results directly in this node, with `includeTests`, when they fit the response;
    /// absent while `testCount` is above zero, they are read page by page from `.../tree/tests`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<Vec<TreeTest>>,
    /// On the root of a label grouping: the label names of the levels, without those no
    /// result has (pass them as `groupBy` to `.../tree/tests`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub levels: Option<Vec<String>>,
}

/// A result as a leaf of the launch tree.
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TreeTest {
    pub id: Uuid,
    /// The name shown in the tree (the test title, without the groups above it).
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Attempts, when more than one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_count: Option<i32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flaky_in_run: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flaky: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defect: Option<TreeDefect>,
}

/// The parts of a defect that name and color it in the tree.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TreeDefect {
    pub type_id: Uuid,
    pub group: String,
    pub name: String,
    pub source: String,
}

/// A stored attachment.
#[derive(Clone, Debug)]
pub struct StoredAttachment {
    pub id: Uuid,
    pub project_id: Uuid,
    pub sha256: String,
    pub size: i64,
    pub content_type: String,
    pub original_name: Option<String>,
    pub blob_key: String,
}

/// A page of a list.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Opaque cursor for the next page (`before=`), `null` on the last page.
    pub next: Option<String>,
}

/// A periodic job of the scheduler.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledJob {
    pub name: String,
    pub schedule: String,
    pub next_run_at: DateTime<Utc>,
    pub lease_owner: Option<String>,
    pub lease_until: Option<DateTime<Utc>>,
    pub last_started_at: Option<DateTime<Utc>>,
    pub last_finished_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub last_duration_ms: Option<i64>,
    pub run_count: i64,
    pub failure_count: i64,
}
