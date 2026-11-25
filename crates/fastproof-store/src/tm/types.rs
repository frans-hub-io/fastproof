//! DTOs of test management as the API shows them (`41-tm-interfaces.md`: the Rust name is the
//! OpenAPI schema name), the stored rows, and the limits.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::Launch;

/// Bytes of one tag, at most.
pub const MAX_TAG_BYTES: usize = 100;
/// Separator of the value path in the ids of `groupBy=behavior|suite` tree nodes.
pub const TREE_ID_SEPARATOR: char = '\u{1f}';

/// Limits of test management (`FASTPROOF_TM_<NAME>`, see the server's `config.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TmLimits {
    /// Steps per case on all levels.
    pub max_steps: usize,
    /// Nesting of steps.
    pub max_step_depth: usize,
    /// Bytes of one Markdown field.
    pub max_markdown_bytes: usize,
    /// Bytes of a whole case body as JSON.
    pub max_body_bytes: usize,
    /// Bytes of one attachment (case or run).
    pub max_attachment_bytes: u64,
    /// Attachments per case.
    pub max_case_attachments: usize,
    /// Attachments per run step result.
    pub max_step_attachments: usize,
    /// Field definitions per project.
    pub max_fields: usize,
    /// Options of one select field.
    pub max_field_options: usize,
    /// Folders per project.
    pub max_folders: usize,
    /// Depth of folders (a top-level folder has depth 1).
    pub max_folder_depth: usize,
    /// Cases per project.
    pub max_cases: usize,
    /// Cases per plan.
    pub max_plan_cases: usize,
    /// Run items per launch.
    pub max_run_items: usize,
    /// Cases per bulk request.
    pub max_bulk: usize,
    /// Bytes of a CSV or JSON import.
    pub max_import_bytes: u64,
    /// Uncompressed bytes of a zip import.
    pub max_import_zip_bytes: u64,
    /// Rows of an import.
    pub max_import_rows: usize,
    /// Bytes of one import cell.
    pub max_import_cell_bytes: usize,
    /// Tags per case (each at most [`MAX_TAG_BYTES`]).
    pub max_tags: usize,
    /// Bytes of a name (case, folder, field, plan).
    pub max_name_bytes: usize,
    /// Writes that multiply work (plan launches, plan item changes, bulk case changes, ...)
    /// per caller and minute on one replica.
    pub multiplying_writes_per_minute: usize,
    /// Launches with an open manual run that one caller started from plans, per project.
    pub max_open_plan_launches: usize,
    /// Shared steps per project.
    pub max_shared_steps: usize,
    /// `shared` steps per case.
    pub max_shared_refs: usize,
    /// Steps of one shared step on all levels.
    pub max_shared_step_steps: usize,
    /// Bytes of the steps of one shared step as JSON.
    pub max_shared_step_bytes: usize,
    /// Parameter columns per case.
    pub max_parameter_columns: usize,
    /// Parameter rows per case.
    pub max_parameter_rows: usize,
    /// Bytes of one parameter value.
    pub max_parameter_value_bytes: usize,
    /// Run iterations per launch.
    pub max_run_iterations: usize,
    /// Requirements per project.
    pub max_requirements: usize,
    /// Depth of requirements (a top-level requirement has depth 1).
    pub max_requirement_depth: usize,
    /// Case links per requirement.
    pub max_requirement_cases: usize,
    /// Saved queries per owner and entity (shared ones: twice as many per project).
    pub max_saved_queries: usize,
    /// Items of one bulk review.
    pub max_bulk_review: usize,
    /// Whether writes of new formats wait for every replica (`FASTPROOF_TM_GATE`).
    pub gate_mode: TmGateMode,
    /// Seconds after the replica-level migration before the gate may open
    /// (`FASTPROOF_TM_GATE_SETTLE_SECONDS`).
    pub gate_settle_seconds: u64,
}

impl Default for TmLimits {
    fn default() -> Self {
        Self {
            max_steps: 500,
            max_step_depth: 3,
            max_markdown_bytes: 16_384,
            max_body_bytes: 1_048_576,
            max_attachment_bytes: 52_428_800,
            max_case_attachments: 100,
            max_step_attachments: 20,
            max_fields: 100,
            max_field_options: 1000,
            max_folders: 20_000,
            max_folder_depth: 10,
            max_cases: 200_000,
            max_plan_cases: 50_000,
            max_run_items: 50_000,
            max_bulk: 10_000,
            max_import_bytes: 104_857_600,
            max_import_zip_bytes: 2_147_483_648,
            max_import_rows: 200_000,
            max_import_cell_bytes: 65_536,
            max_tags: 50,
            max_name_bytes: 500,
            multiplying_writes_per_minute: 30,
            max_open_plan_launches: 20,
            max_shared_steps: 5000,
            max_shared_refs: 50,
            max_shared_step_steps: 100,
            max_shared_step_bytes: 65_536,
            max_parameter_columns: 20,
            max_parameter_rows: 1000,
            max_parameter_value_bytes: 1024,
            max_run_iterations: 50_000,
            max_requirements: 50_000,
            max_requirement_depth: 6,
            max_requirement_cases: 500,
            max_saved_queries: 100,
            max_bulk_review: 500,
            gate_mode: TmGateMode::Auto,
            gate_settle_seconds: 600,
        }
    }
}

/// `Some(None)` for an explicit `null`, `Some(Some(v))` for a value, `None` (with
/// `#[serde(default)]`) when the key is absent.
pub fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

macro_rules! str_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $v:ident = $s:tt),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
        pub enum $name {
            $($(#[$vm])* #[serde(rename = $s)] $v),+
        }

        impl $name {
            /// Every value.
            pub const ALL: &'static [Self] = &[$(Self::$v),+];

            /// The stored and JSON spelling.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$v => $s),+
                }
            }

            /// From the stored spelling.
            pub fn parse(s: &str) -> Option<Self> {
                match s {
                    $($s => Some(Self::$v),)+
                    _ => None,
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

str_enum!(
    /// Lifecycle of a case.
    CaseStatus { Draft = "draft", Active = "active", Outdated = "outdated" }
);

#[allow(clippy::derivable_impls)]
impl Default for CaseStatus {
    fn default() -> Self {
        Self::Active
    }
}

str_enum!(
    /// Where a case came from.
    CaseOrigin { Ui = "ui", Code = "code", Import = "import", Ai = "ai" }
);

str_enum!(
    /// Kind of a case link.
    CaseLinkType { Link = "link", Issue = "issue", Tms = "tms" }
);

#[allow(clippy::derivable_impls)]
impl Default for CaseLinkType {
    fn default() -> Self {
        Self::Link
    }
}

str_enum!(
    /// A code identity (from uploads) or the manual identity of a case.
    IdentityKind { Code = "code", Manual = "manual" }
);

str_enum!(
    /// Kind of an external case key.
    CaseKeyKind { AllureId = "allure_id", Testrail = "testrail", Qase = "qase", Testops = "testops", External = "external" }
);

str_enum!(
    /// Type of a case field.
    FieldType { Select = "select", Multiselect = "multiselect", Text = "text", Number = "number", Date = "date" }
);

str_enum!(
    /// What made a case version.
    VersionSource { Ui = "ui", Code = "code", Import = "import", Ai = "ai", Restore = "restore", Bulk = "bulk" }
);

str_enum!(
    /// Whether a result comes from a manual run or from an upload; also the automation filter.
    ResultSource { Manual = "manual", Automated = "automated" }
);

#[allow(clippy::derivable_impls)]
impl Default for ResultSource {
    fn default() -> Self {
        Self::Automated
    }
}

str_enum!(
    /// Action of `POST /cases/bulk`.
    CaseBulkAction { Move = "move", SetStatus = "setStatus", SetField = "setField", AddTags = "addTags", RemoveTags = "removeTags", Archive = "archive", Restore = "restore", SetState = "setState" }
);

str_enum!(
    /// State of an AI suggestion.
    SuggestionStatus { Pending = "pending", Accepted = "accepted", Rejected = "rejected", Stale = "stale" }
);

str_enum!(
    /// Status of a run item.
    RunStatus { Untested = "untested", InProgress = "in_progress", Passed = "passed", Failed = "failed", Blocked = "blocked", Skipped = "skipped" }
);

impl RunStatus {
    /// A status that is projected into `results`.
    pub fn is_final(self) -> bool {
        matches!(
            self,
            Self::Passed | Self::Failed | Self::Blocked | Self::Skipped
        )
    }
}

str_enum!(
    /// Status of one step of a run item.
    StepStatus { Untested = "untested", Passed = "passed", Failed = "failed", Blocked = "blocked", Skipped = "skipped" }
);

str_enum!(
    /// Kind of a test plan: `adhoc` (stage 1, expires) or `managed`.
    TestPlanKind { Adhoc = "adhoc", Managed = "managed" }
);

str_enum!(
    /// Who the items of a launch started from a plan are assigned to.
    PlanAssign { Plan = "plan", Me = "me", None = "none" }
);

str_enum!(
    /// State of a manual run.
    ManualState { Open = "open", Closed = "closed" }
);

str_enum!(
    /// What `untestedStepsTo` sets the untested steps of an item to.
    UntestedStepsTo { Skipped = "skipped" }
);

str_enum!(
    /// What closing a manual run does with untested items.
    ManualUntestedTo { Skipped = "skipped", Leave = "leave" }
);

str_enum!(
    /// Kind of a background test-management job.
    TmJobKind { Import = "import", Export = "export", Backfill = "backfill", LastStatus = "last_status", SharedInline = "shared_inline", WorkflowRemap = "workflow_remap", Requirements = "requirements", CoverageExport = "coverage_export" }
);

str_enum!(
    /// Status of a background test-management job.
    TmJobStatus { Queued = "queued", DryRun = "dry_run", Ready = "ready", Running = "running", Done = "done", Failed = "failed", Cancelled = "cancelled" }
);

str_enum!(
    /// Format of an import file.
    ImportFormat { GenericCsv = "generic-csv", TestrailCsv = "testrail-csv", QaseCsv = "qase-csv", QaseJson = "qase-json", TestopsCsv = "testops-csv", FastproofNdjson = "fastproof-ndjson" }
);

str_enum!(
    /// How steps are laid out in a CSV import.
    StepsLayout { Rows = "rows", Cells = "cells" }
);

str_enum!(
    /// Whether an import only creates cases or also updates matching ones.
    ImportMode { CreateOnly = "createOnly", Upsert = "upsert" }
);

str_enum!(
    /// What an import does with select values that are not options yet.
    UnknownFieldValues { Create = "create", Skip = "skip", Fail = "fail" }
);

str_enum!(
    /// Source of an API import.
    TestOpsSource { AllureTestops = "allure-testops" }
);

str_enum!(
    /// What deleting a non-empty folder does with its content.
    FolderDeleteMode { MoveUp = "moveUp", ArchiveCases = "archiveCases" }
);

str_enum!(
    /// Grouping of `GET /cases/tree`.
    CaseTreeGroupBy { Folder = "folder", Behavior = "behavior", Suite = "suite" }
);

/// Value of a case field: select and text are strings, multiselect a list of strings, number
/// a number, date an ISO `yyyy-mm-dd` string; `null` when unset.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum FieldValue {
    Text(String),
    Many(Vec<String>),
    Number(f64),
    #[default]
    Null,
}

impl FieldValue {
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

/// Field values of a case by field key.
pub type FieldValues = BTreeMap<String, FieldValue>;

/// One step of a case; steps nest up to `max_step_depth` levels. Text fields are Markdown.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseStep {
    /// Stable id across versions (`s_<ulid>`); an empty id on write is assigned by the server.
    #[serde(default)]
    #[schema(required = true)]
    pub id: String,
    #[serde(default)]
    #[schema(required = true)]
    pub action: String,
    #[serde(default)]
    #[schema(required = true)]
    pub expected: String,
    #[serde(default)]
    #[schema(required = true)]
    pub data: String,
    /// Ids of attachments of the case.
    #[serde(default)]
    #[schema(required = true)]
    pub attachments: Vec<Uuid>,
    #[serde(default)]
    #[schema(no_recursion, required = true)]
    pub steps: Vec<CaseStep>,
    /// A reference to a shared step (top level only; the other fields stay empty on write).
    /// In a run item, `steps` are the pinned version's steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<SharedStepRef>,
}

/// A link of a case.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseLink {
    #[serde(default)]
    #[schema(required = true)]
    pub name: String,
    pub url: String,
    #[serde(default, rename = "type")]
    #[schema(required = true)]
    pub link_type: CaseLinkType,
}

/// Content of a case (`cases.body`, also the body of a version snapshot). Text is Markdown.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseBody {
    #[serde(default)]
    #[schema(required = true)]
    pub description: String,
    #[serde(default)]
    #[schema(required = true)]
    pub preconditions: String,
    #[serde(default)]
    #[schema(required = true)]
    pub steps: Vec<CaseStep>,
    #[serde(default)]
    #[schema(required = true)]
    pub links: Vec<CaseLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<CaseParameters>,
}

/// A step rendered to sanitized HTML.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseStepHtml {
    pub id: String,
    pub action: String,
    pub expected: String,
    pub data: String,
    #[schema(no_recursion)]
    pub steps: Vec<CaseStepHtml>,
}

/// The Markdown fields of a case body rendered to sanitized HTML.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseHtml {
    pub description: String,
    pub preconditions: String,
    pub steps: Vec<CaseStepHtml>,
}

/// Body of `POST /cases` and `PUT /cases/{n}`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseWrite {
    pub name: String,
    /// The folder: `null` is the root. Absent is the root on `POST` and keeps the folder on
    /// `PUT`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<CaseStatus>,
    /// Absent: an empty body on `POST`, the current body on `PUT`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<CaseBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<BTreeMap<String, FieldValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// Body of `PATCH /cases/{n}` (merge): absent keys are kept, `folderId: null` moves to the root.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CasePatch {
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<Uuid>)]
    pub folder_id: Option<Option<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<CaseStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<BTreeMap<String, FieldValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// The newest result of a case over all its identities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LastResult {
    pub status: String,
    pub at: DateTime<Utc>,
    pub source: ResultSource,
    pub launch_id: Uuid,
    pub result_id: Uuid,
}

/// A case in lists.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseSummary {
    pub number: i64,
    pub name: String,
    pub status: CaseStatus,
    pub automated: bool,
    pub folder_id: Option<Uuid>,
    pub fields: BTreeMap<String, FieldValue>,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<LastResult>,
    pub updated_at: DateTime<Utc>,
    pub ai_unreviewed: bool,
    pub archived: bool,
    /// The key of the configured workflow status (`null` without `caseWorkflow`).
    pub state: Option<String>,
    pub review_state: Option<ReviewState>,
}

/// A test identity (row of `test_cases`) linked to a case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseIdentity {
    pub test_case_id: Uuid,
    pub full_name: String,
    pub kind: IdentityKind,
    pub last_seen_at: DateTime<Utc>,
    pub branches: Vec<String>,
}

/// An external key of a case (`case_keys`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseKey {
    pub kind: CaseKeyKind,
    pub value: String,
}

/// One folder on the path of a case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseFolderPathEntry {
    pub id: Uuid,
    pub name: String,
}

/// The folder of a case with its path from the root (the folder itself last).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseFolderRef {
    pub id: Uuid,
    pub path: Vec<CaseFolderPathEntry>,
}

/// A case.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Case {
    pub number: i64,
    pub id: Uuid,
    pub name: String,
    pub status: CaseStatus,
    pub automated: bool,
    pub origin: CaseOrigin,
    pub ai_unreviewed: bool,
    /// The folder id (`null` at the root); the same value a `PUT` keeps when it leaves
    /// `folderId` out.
    pub folder_id: Option<Uuid>,
    pub folder: Option<CaseFolderRef>,
    pub body: CaseBody,
    pub html: CaseHtml,
    pub fields: BTreeMap<String, FieldValue>,
    pub tags: Vec<String>,
    /// Field keys whose values come from code (policy `code` on an automated case).
    pub code_owned: Vec<String>,
    /// The first 100 identities (manual first, then the newest seen).
    pub identities: Vec<CaseIdentity>,
    /// How many identities the case has (`identities` may hold fewer).
    pub identities_total: i64,
    pub keys: Vec<CaseKey>,
    pub revision: i64,
    pub version: i32,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_by: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    /// The key of the configured workflow status (`null` without `caseWorkflow`).
    pub state: Option<String>,
    /// The newest approved version under review (`null`: none, or never reviewed).
    pub approved_version: Option<i32>,
    pub review_state: Option<ReviewState>,
    /// Shared steps the case references, at their runnable version.
    pub shared_steps: Vec<SharedStepBrief>,
}

/// A page of cases.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseList {
    pub items: Vec<CaseSummary>,
    pub next: Option<String>,
}

/// Cases in a folder and its subfolders (not archived).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FolderCounts {
    pub total: i64,
    pub manual: i64,
    pub automated: i64,
}

/// A case folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseFolder {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub position: i32,
    /// 1 for a top-level folder.
    pub depth: i32,
    pub counts: FolderCounts,
    pub has_children: bool,
}

/// Body of `POST /case-folders`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseFolderWrite {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i32>,
}

/// Body of `PATCH /case-folders/{id}`: rename, reorder, or move (`parentId`, `null` = root).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseFolderPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<Uuid>)]
    pub parent_id: Option<Option<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i32>,
}

/// `{items}` of folders of one level, or of the whole tree with `all=true`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CaseFolderList {
    pub items: Vec<CaseFolder>,
    /// With `all=true`: the project has more folders than `maxFolders` and the list stops there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

/// A node of `GET /cases/tree`; for `groupBy=behavior|suite` the id is the value path joined
/// by U+001F.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseTreeNode {
    pub id: String,
    pub name: String,
    pub counts: FolderCounts,
    pub has_children: bool,
}

/// One level of the case tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CaseTree {
    pub nodes: Vec<CaseTreeNode>,
}

/// An option of a select or multiselect field.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FieldOption {
    pub value: String,
    /// `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub archived: bool,
}

/// A case field definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseFieldDef {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    pub options: Vec<FieldOption>,
    pub required: bool,
    pub default_value: FieldValue,
    /// Allure label whose value code sync writes into this field.
    pub source_label: Option<String>,
    pub built_in: bool,
    pub position: i32,
    pub archived: bool,
}

/// Body of `POST /case-fields`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseFieldWrite {
    /// `^[a-z][a-z0-9_]{0,62}$`, unique in the project.
    pub key: String,
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<FieldOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<FieldValue>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub source_label: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i32>,
}

/// Body of `PATCH /case-fields/{id}`: every key optional; `type` may be sent only unchanged;
/// `archived` archives or restores the field.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseFieldPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub field_type: Option<FieldType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<FieldOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<FieldValue>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub source_label: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

/// `{items}` of field definitions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CaseFieldList {
    pub items: Vec<CaseFieldDef>,
}

/// A version of a case in its history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseVersionSummary {
    pub version: i32,
    pub source: VersionSource,
    /// Top-level keys that changed.
    pub changed: Vec<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<Uuid>,
}

/// The content of a case at one version (stored zstd-compressed in `case_versions.snapshot_z`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseSnapshot {
    pub name: String,
    pub status: CaseStatus,
    pub folder_id: Option<Uuid>,
    pub body: CaseBody,
    #[serde(default)]
    #[schema(required = true)]
    pub fields: BTreeMap<String, FieldValue>,
    #[serde(default)]
    #[schema(required = true)]
    pub tags: Vec<String>,
}

/// A version with its snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseVersion {
    #[serde(flatten)]
    pub summary: CaseVersionSummary,
    pub snapshot: CaseSnapshot,
}

/// A result of a case (any identity) in its history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseHistoryEntry {
    pub launch_id: Uuid,
    pub launch_number: i64,
    pub launch_name: String,
    pub result_id: Uuid,
    pub status: String,
    pub source: ResultSource,
    pub at: DateTime<Utc>,
    pub branch: Option<String>,
    pub environment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executed_by: Option<String>,
}

/// A test identity, linked or not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestIdentity {
    pub test_case_id: Uuid,
    pub full_name: String,
    pub display_name: String,
    pub kind: IdentityKind,
    pub case_number: Option<i64>,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub branches: Vec<String>,
}

/// Structured case filter (case list, bulk, plans).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_ids: Option<Vec<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recursive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statuses: Option<Vec<CaseStatus>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation: Option<ResultSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<BTreeMap<String, Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
}

/// Body of `POST /cases/bulk`: `move` takes `folderId|null`, `setStatus` a status, `setField`
/// `{key, value}`, the tag actions a list of tags.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseBulk {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numbers: Option<Vec<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<CaseFilter>,
    /// An FPQL query (exclusive with `numbers` and `filter`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    pub action: CaseBulkAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

/// A case a bulk action skipped, with the reason (an error code).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseBulkSkip {
    pub number: i64,
    pub code: String,
}

/// Outcome of a bulk action.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseBulkResult {
    pub updated: i64,
    pub skipped: Vec<CaseBulkSkip>,
}

/// What an AI suggestion proposes: a patch plus optionally a new name and body.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseProposal {
    #[serde(flatten)]
    pub patch: CasePatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<CaseBody>,
}

/// An AI suggestion for a case, never applied without a person accepting it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseSuggestion {
    pub id: Uuid,
    pub case_number: i64,
    pub base_revision: i64,
    pub proposal: CaseProposal,
    pub rationale: String,
    pub agent: String,
    pub status: SuggestionStatus,
    pub created_at: DateTime<Utc>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
}

/// A stored attachment as a case or step result references it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRef {
    pub id: Uuid,
    pub sha256: String,
    pub size: i64,
    pub content_type: String,
    pub name: String,
}

/// A test plan (adhoc or managed).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestPlan {
    pub id: Uuid,
    pub kind: TestPlanKind,
    pub name: String,
    pub description: String,
    pub filter: Option<CaseFilter>,
    /// An FPQL query selecting the plan's cases (exclusive with `filter`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    pub counts: FolderCounts,
    pub revision: i64,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
    pub archived: bool,
}

/// Body of creating or replacing a managed plan.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestPlanWrite {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<CaseFilter>)]
    pub filter: Option<Option<CaseFilter>>,
    /// An FPQL query (exclusive with `filter`); `null` removes it.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub query: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_numbers: Option<Vec<i64>>,
}

/// A case of a plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestPlanItem {
    pub case_number: i64,
    pub name: String,
    pub folder_id: Option<Uuid>,
    pub automated: bool,
    pub assignee: Option<String>,
    pub via_filter: bool,
    pub position: i32,
}

/// A new assignee of a plan case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestPlanAssign {
    pub number: i64,
    pub assignee: Option<String>,
}

/// Body of `POST /test-plans/{id}/items`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestPlanItemsChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add: Option<Vec<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove: Option<Vec<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assign: Option<Vec<TestPlanAssign>>,
}

/// What syncing a filter plan adds and removes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanSyncDiff {
    pub added: Vec<i64>,
    pub removed: Vec<i64>,
}

/// A launch attribute given when starting a launch from a plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanLaunchAttribute {
    pub key: String,
    pub value: String,
}

/// Body of `POST /test-plans/{id}/launches`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartPlanLaunch {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Vec<PlanLaunchAttribute>>,
    /// Absent: `none`.
    #[serde(default = "PlanAssign::default_none")]
    pub assign: PlanAssign,
    /// Absent: `false`.
    #[serde(default)]
    pub include_automated: bool,
    /// Absent: `true`; `false` creates no run items (a CI start).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_manual: Option<bool>,
    /// Run the head version of cases whose head is not approved (needs `cases:review`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_unapproved: bool,
    /// Dispatch the automated cases to CI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<PlanCi>,
}

impl PlanAssign {
    fn default_none() -> Self {
        Self::None
    }
}

/// A launch started from a plan, with the CI hand-off.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanLaunch {
    pub launch: Launch,
    pub testplan_url: String,
    pub ci_hint: String,
    /// One launch per CI environment; the first is `launch`.
    pub launches: Vec<Launch>,
    pub dispatches: Vec<PlanDispatch>,
    /// Cases left out (the first 1000).
    pub skips: Vec<RunItemSkip>,
}

/// Manual progress of a launch (present when the launch has a manual run).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LaunchManual {
    pub state: ManualState,
    pub total: i64,
    pub untested: i64,
    pub in_progress: i64,
    pub passed: i64,
    pub failed: i64,
    pub blocked: i64,
    pub skipped: i64,
    /// When the first run item was started (the manual part has its own times; the launch's
    /// `durationMs` covers the automated results only).
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    /// When the last run item got a final status.
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
}

/// A run item in lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunItemSummary {
    pub id: Uuid,
    pub case_number: i64,
    pub name: String,
    pub folder_path: Vec<String>,
    pub status: RunStatus,
    pub assignee: Option<String>,
    pub execution_no: i32,
    pub updated_at: DateTime<Utc>,
    /// Iterations of a parameterized case (0 without parameters; `run_items.iterations`).
    /// Named `iterationCount` because [`RunItem`] flattens this struct next to its own
    /// `iterations` list.
    pub iteration_count: i32,
}

/// The result of one step of a run item; `path` is `1`, `1.2`, `1.2.3` over the executed
/// version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunStepResult {
    pub path: String,
    pub status: StepStatus,
    pub actual: String,
    pub comment: String,
    pub attachments: Vec<AttachmentRef>,
    pub updated_by: String,
    pub updated_at: DateTime<Utc>,
}

/// A run item with the executed version of its case.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunItem {
    #[serde(flatten)]
    pub summary: RunItemSummary,
    pub launch_id: Uuid,
    pub case_version: i32,
    pub preconditions: String,
    pub preconditions_html: String,
    pub steps: Vec<CaseStep>,
    pub steps_html: Vec<CaseStepHtml>,
    pub step_results: Vec<RunStepResult>,
    pub comment: String,
    pub defect: Option<serde_json::Value>,
    pub result_id: Option<Uuid>,
    pub executed_by: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub revision: i64,
    /// Shared steps pinned for this item.
    pub shared_pins: Vec<SharedPin>,
    pub iterations: Vec<RunIteration>,
    pub parameter_columns: Vec<String>,
}

/// Body of `PUT /run-items/{itemId}/steps/{path}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunStepWrite {
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_ids: Option<Vec<Uuid>>,
}

/// Answer to a step write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunStepAck {
    pub item_status: RunStatus,
    pub suggested_status: RunStatus,
    pub revision: i64,
}

/// Body of `PATCH /run-items/{itemId}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunItemPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RunStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub assignee: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untested_steps_to: Option<UntestedStepsTo>,
    /// Apply `status` to every iteration (explicit only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all_iterations: Option<bool>,
}

/// Body of `POST /launches/{id}/run-items`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunItemsAdd {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_numbers: Option<Vec<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<Uuid>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub assignee: Option<Option<String>>,
    /// Run the head version of cases whose head is not approved (needs `cases:review`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_unapproved: bool,
}

/// Outcome of adding run items.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunItemsAdded {
    pub added: i64,
    /// Cases left out: already in the launch or automated.
    pub skipped: i64,
    /// Why named cases were left out (the first 1000).
    #[serde(default)]
    pub skips: Vec<RunItemSkip>,
}

str_enum!(
    /// Why a case was not added to a launch as a run item: `automated` (CI runs it; only manual
    /// cases become run items), `in_launch` (it has a run item already), `not_approved` (under
    /// review without an approved version) or `blocked_by_state` (its workflow state blocks runs).
    RunItemSkipCode { Automated = "automated", InLaunch = "in_launch", NotApproved = "not_approved", BlockedByState = "blocked_by_state" }
);

/// A case left out when adding run items.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunItemSkip {
    pub case_number: i64,
    pub code: RunItemSkipCode,
}

/// An earlier execution of a run item, kept when the item is rerun.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunItemExecution {
    pub execution_no: i32,
    pub status: RunStatus,
    pub case_version: i32,
    pub comment: String,
    pub executed_by: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub defect: Option<serde_json::Value>,
    pub step_results: Vec<RunStepResult>,
    /// Who started the next execution, and when.
    pub rerun_by: String,
    pub rerun_at: DateTime<Utc>,
}

/// An issue linked to a run item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunDefectIssue {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// Body of `POST /run-items/{itemId}/defect`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunDefect {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defect_type_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<RunDefectIssue>,
}

/// Body of `POST /launches/{id}/manual/close`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualClose {
    /// Absent: `skipped`.
    #[serde(default = "ManualClose::default_untested_to")]
    pub untested_to: ManualUntestedTo,
}

impl ManualClose {
    fn default_untested_to() -> ManualUntestedTo {
        ManualUntestedTo::Skipped
    }
}

/// Progress of a job.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TmJobProgress {
    pub done: i64,
    pub total: Option<i64>,
}

/// One row error of an import.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportRowError {
    pub row: i64,
    pub code: String,
    pub message: String,
}

/// What an import does (dry run) or did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub create: i64,
    pub update: i64,
    pub skip: i64,
    pub folders: i64,
    /// Fields of the source the import leaves out (no such field in the project).
    pub unknown_fields: Vec<String>,
    pub unknown_values: BTreeMap<String, Vec<String>>,
    /// The first 1000 errors.
    pub errors: Vec<ImportRowError>,
    /// Field definitions the import creates (dry run) or created for unknown columns
    /// (`unknownFieldValues: create`).
    #[serde(default)]
    pub created_fields: Vec<String>,
    /// Rows imported with a change, such as a case set to `draft` because required fields
    /// have no value (`required_fields_missing`); the first 1000.
    #[serde(default)]
    pub warnings: Vec<ImportRowError>,
}

/// A background job (import, export, backfill).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TmJob {
    pub id: Uuid,
    pub kind: TmJobKind,
    pub status: TmJobStatus,
    pub progress: TmJobProgress,
    pub report: Option<ImportReport>,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
}

/// First rows of an import file and the suggested column mapping.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub format: ImportFormat,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub suggested_mapping: BTreeMap<String, String>,
    pub steps_layout: StepsLayout,
}

impl ImportMode {
    fn default_create_only() -> Self {
        Self::CreateOnly
    }
}

impl UnknownFieldValues {
    fn default_skip() -> Self {
        Self::Skip
    }
}

/// Options of a file import.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportOptions {
    pub format: ImportFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps_layout: Option<StepsLayout>,
    /// Absent: `createOnly`.
    #[serde(default = "ImportMode::default_create_only")]
    pub mode: ImportMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_folder_id: Option<Uuid>,
    /// Absent: `skip`.
    #[serde(default = "UnknownFieldValues::default_skip")]
    pub unknown_field_values: UnknownFieldValues,
    /// Status of imported cases (`draft` or `active`); absent: the status the source maps, else
    /// `active`. A case whose required fields have no value is imported as `draft`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ImportCaseStatus>,
}

str_enum!(
    /// Status an import gives the cases it creates.
    ImportCaseStatus { Draft = "draft", Active = "active" }
);

/// An import from the Allure TestOps API. The token is stored as a project secret at once and
/// never returned, logged or audited.
#[derive(Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestOpsImport {
    pub source: TestOpsSource,
    pub base_url: String,
    #[schema(write_only)]
    pub token: String,
    pub remote_project_id: i64,
    /// Absent: `createOnly`.
    #[serde(default = "ImportMode::default_create_only")]
    pub mode: ImportMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_folder_id: Option<Uuid>,
}

impl std::fmt::Debug for TestOpsImport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestOpsImport")
            .field("source", &self.source)
            .field("base_url", &self.base_url)
            .field("token", &"<redacted>")
            .field("remote_project_id", &self.remote_project_id)
            .field("mode", &self.mode)
            .field("target_folder_id", &self.target_folder_id)
            .finish()
    }
}

/// A row of `cases` (without `search_text`).
#[derive(Clone, Debug, PartialEq)]
pub struct CaseRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub number: i64,
    pub name: String,
    pub status: CaseStatus,
    pub automated: bool,
    pub origin: CaseOrigin,
    pub ai_unreviewed: bool,
    pub folder_id: Option<Uuid>,
    pub body: CaseBody,
    pub fields: BTreeMap<String, FieldValue>,
    pub tags: Vec<String>,
    pub code_hash: Option<String>,
    pub revision: i64,
    pub version: i32,
    pub last_trusted_at: Option<DateTime<Utc>>,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_by: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    /// Key of the configured workflow status.
    pub state: Option<String>,
    pub approved_version: Option<i32>,
    pub review_state: Option<ReviewState>,
    /// The newest final result over all identities (denormalized).
    pub last_status: Option<String>,
    pub last_status_at: Option<DateTime<Utc>>,
    pub last_source: Option<ResultSource>,
    pub last_launch_id: Option<Uuid>,
    pub last_result_id: Option<Uuid>,
}

impl CaseRow {
    /// `TC-<n>`.
    pub fn key(&self) -> String {
        format!("TC-{}", self.number)
    }
}

/// A row of `case_folders`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub position: i32,
    /// Ancestors and the folder itself, from the root.
    pub path_ids: Vec<Uuid>,
    pub depth: i32,
}

// ---- TM next level (`74-tm-next-interfaces.md` §6) ----

str_enum!(
    /// Whether writes of new formats wait until every replica knows them.
    TmGateMode { Auto = "auto", Open = "open", Closed = "closed" }
);

/// A reference of a case step to a shared step; `version` only in expanded run item steps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedStepRef {
    pub id: Uuid,
    pub number: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i32>,
}

str_enum!(
    /// Where a shared step came from.
    SharedStepOrigin { Ui = "ui", Import = "import", Ai = "ai" }
);

/// A shared step a case references, at its runnable version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedStepBrief {
    pub id: Uuid,
    pub number: i64,
    pub name: String,
    pub version: i32,
    pub archived: bool,
    pub steps: Vec<CaseStep>,
    pub steps_html: Vec<CaseStepHtml>,
}

/// The Markdown of a shared step rendered to sanitized HTML.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedStepHtml {
    pub description: String,
    pub steps: Vec<CaseStepHtml>,
}

/// How many cases and open runs use a shared step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedStepUsageCount {
    pub cases: i64,
    pub open_runs: i64,
}

/// A shared step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedStep {
    pub id: Uuid,
    pub number: i64,
    pub name: String,
    pub description: String,
    pub steps: Vec<CaseStep>,
    pub html: SharedStepHtml,
    pub version: i32,
    pub approved_version: Option<i32>,
    pub review_state: Option<ReviewState>,
    pub origin: SharedStepOrigin,
    pub ai_unreviewed: bool,
    pub usage: SharedStepUsageCount,
    pub revision: i64,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_by: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
}

/// A shared step in lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedStepSummary {
    pub number: i64,
    pub name: String,
    pub version: i32,
    pub approved_version: Option<i32>,
    pub review_state: Option<ReviewState>,
    pub step_count: i64,
    pub cases: i64,
    pub ai_unreviewed: bool,
    pub archived: bool,
    pub updated_at: DateTime<Utc>,
}

/// A page of shared steps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedStepList {
    pub items: Vec<SharedStepSummary>,
    pub next: Option<String>,
}

/// Body of creating or replacing a shared step (steps without `shared`, depth at most 2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedStepWrite {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub steps: Vec<CaseStep>,
}

/// A version of a shared step in its history (`changed` within name, description, steps).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedStepVersionSummary {
    pub version: i32,
    pub source: VersionSource,
    pub changed: Vec<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// The content of a shared step at one version.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedStepSnapshot {
    pub name: String,
    pub description: String,
    pub steps: Vec<CaseStep>,
}

/// A version of a shared step with its snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedStepVersion {
    #[serde(flatten)]
    pub summary: SharedStepVersionSummary,
    pub snapshot: SharedStepSnapshot,
}

/// A page of shared step versions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedStepVersionList {
    pub items: Vec<SharedStepVersionSummary>,
    pub next: Option<String>,
}

/// A case that uses a shared step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedStepUsage {
    pub case_number: i64,
    pub name: String,
    pub status: CaseStatus,
    pub archived: bool,
}

/// A page of the cases that use a shared step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedStepUsageList {
    pub items: Vec<SharedStepUsage>,
    pub next: Option<String>,
    pub open_runs: i64,
}

/// Body of `POST /cases/{n}/extract-shared`: adjacent top-level step ids.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtractSharedStep {
    pub step_ids: Vec<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The shared step extracted from a case, and the case after it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedSharedStep {
    pub shared_step: SharedStep,
    pub case: Case,
}

/// A version of a shared step a case references (the case's "Versions" stream).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedStepChange {
    pub shared_step_number: i64,
    pub name: String,
    pub version: i32,
    pub source: VersionSource,
    pub changed: Vec<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// A page of shared step changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedStepChangeList {
    pub items: Vec<SharedStepChange>,
    pub next: Option<String>,
}

/// A shared step pinned for a run item: `version` runs, `current` is the runnable one now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedPin {
    pub id: Uuid,
    pub number: i64,
    pub name: String,
    pub version: i32,
    pub current: i32,
}

str_enum!(
    /// Review state of the head version of a case or shared step.
    ReviewState { Pending = "pending", ChangesRequested = "changes_requested", Approved = "approved" }
);

str_enum!(
    /// A reviewer's decision.
    ReviewDecision { Approve = "approve", RequestChanges = "request_changes" }
);

str_enum!(
    /// What a review is about.
    ReviewKind { Case = "case", SharedStep = "sharedStep" }
);

str_enum!(
    /// Which cases need review: none, manual ones, or all.
    ReviewMode { Off = "off", Manual = "manual", All = "all" }
);

str_enum!(
    /// A condition of a workflow transition.
    WorkflowGuard { Approved = "approved", RequiredFields = "requiredFields", NotAuthor = "notAuthor" }
);

str_enum!(
    /// The permission a workflow transition needs.
    WorkflowPermission { CasesWrite = "cases:write", CasesReview = "cases:review" }
);

str_enum!(
    /// Which changes reset an approval.
    ReviewResetOn { Content = "content", Any = "any" }
);

str_enum!(
    /// Whether imported changes need review.
    ReviewImports { Review = "review", Bypass = "bypass" }
);

/// A configured workflow status: a key within a category (`cases.status`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseWorkflowStatus {
    pub key: String,
    pub name: String,
    pub category: CaseStatus,
    /// Cases in this state do not enter run items or `testplan.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks_runs: Option<bool>,
    /// `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// A workflow transition: from any of `from` (keys or `*`) to `to` (a key or `*`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseWorkflowTransition {
    pub from: Vec<String>,
    pub to: String,
    /// Absent: `cases:write`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<WorkflowPermission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<Vec<WorkflowGuard>>,
}

/// The workflow of manual or of automated cases.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseWorkflowFlow {
    pub initial: String,
    pub transitions: Vec<CaseWorkflowTransition>,
}

/// `caseWorkflow` of the project settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseWorkflowSettings {
    pub statuses: Vec<CaseWorkflowStatus>,
    pub manual: CaseWorkflowFlow,
    pub automated: CaseWorkflowFlow,
}

impl Default for CaseWorkflowSettings {
    /// The suggested workflow (draft, ready for review, active, outdated); a project without
    /// `caseWorkflow` has no workflow at all.
    fn default() -> Self {
        let st = |key: &str, name: &str, category, blocks: bool, color: Option<&str>| {
            CaseWorkflowStatus {
                key: key.into(),
                name: name.into(),
                category,
                blocks_runs: blocks.then_some(true),
                color: color.map(Into::into),
            }
        };
        let tr = |from: &str, to: &str, permission, requires: Option<Vec<WorkflowGuard>>| {
            CaseWorkflowTransition {
                from: vec![from.into()],
                to: to.into(),
                permission,
                requires,
            }
        };
        Self {
            statuses: vec![
                st("draft", "Draft", CaseStatus::Draft, false, None),
                st(
                    "ready_for_review",
                    "Ready for review",
                    CaseStatus::Draft,
                    true,
                    None,
                ),
                st("active", "Active", CaseStatus::Active, false, None),
                st(
                    "outdated",
                    "Outdated",
                    CaseStatus::Outdated,
                    false,
                    Some("#868e96"),
                ),
            ],
            manual: CaseWorkflowFlow {
                initial: "draft".into(),
                transitions: vec![
                    tr("draft", "ready_for_review", None, None),
                    tr(
                        "ready_for_review",
                        "active",
                        None,
                        Some(vec![WorkflowGuard::Approved]),
                    ),
                    tr("*", "outdated", Some(WorkflowPermission::CasesReview), None),
                ],
            },
            automated: CaseWorkflowFlow {
                initial: "active".into(),
                transitions: vec![tr("*", "*", None, None)],
            },
        }
    }
}

impl CaseWorkflowSettings {
    /// The status with this key.
    pub fn status(&self, key: &str) -> Option<&CaseWorkflowStatus> {
        self.statuses.iter().find(|s| s.key == key)
    }

    /// Whether cases in this state stay out of runs.
    pub fn blocks_runs(&self, key: &str) -> bool {
        self.status(key)
            .is_some_and(|s| s.blocks_runs.unwrap_or(false))
    }

    /// Keys of the states that block runs.
    pub fn blocking_keys(&self) -> Vec<String> {
        self.statuses
            .iter()
            .filter(|s| s.blocks_runs.unwrap_or(false))
            .map(|s| s.key.clone())
            .collect()
    }
}

/// `caseReview` of the project settings: version review of cases and shared steps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseReviewSettings {
    pub mode: ReviewMode,
    /// Distinct approvals a head version needs (1 to 5).
    pub approvals: i32,
    pub reset_on: ReviewResetOn,
    pub allow_self_approval: bool,
    pub imports: ReviewImports,
}

impl Default for CaseReviewSettings {
    fn default() -> Self {
        Self {
            mode: ReviewMode::Off,
            approvals: 1,
            reset_on: ReviewResetOn::Content,
            allow_self_approval: false,
            imports: ReviewImports::Review,
        }
    }
}

impl CaseReviewSettings {
    /// Whether review is on for a case (`automated`) or shared step (`false`).
    pub fn in_scope(&self, automated: bool) -> bool {
        match self.mode {
            ReviewMode::Off => false,
            ReviewMode::Manual => !automated,
            ReviewMode::All => true,
        }
    }
}

/// `GET /case-workflow`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseWorkflowView {
    /// Whether the project has `caseWorkflow` (else `workflow` is the suggested one).
    pub configured: bool,
    pub workflow: CaseWorkflowSettings,
    pub review: CaseReviewSettings,
}

/// Body of `PUT /case-workflow`: `null` removes the workflow; `remap` moves cases of removed
/// states (old key to new key) in a job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseWorkflowPut {
    pub workflow: Option<CaseWorkflowSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remap: Option<BTreeMap<String, String>>,
}

/// Answer of `PUT /case-workflow`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseWorkflowSaved {
    pub view: CaseWorkflowView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<TmJob>,
}

/// Body of `POST /cases/{n}/transition`: a state key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseTransition {
    pub to: String,
}

/// A transition from the current state, with what it still misses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AllowedTransition {
    pub to: String,
    pub name: String,
    pub allowed: bool,
    pub missing: Vec<String>,
}

/// A review vote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewVote {
    pub reviewer: String,
    pub decision: ReviewDecision,
    pub comment: String,
    pub version: i32,
    pub created_at: DateTime<Utc>,
}

/// The review of a case or shared step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewThread {
    pub kind: ReviewKind,
    pub number: i64,
    pub name: String,
    pub head_version: i32,
    pub approved_version: Option<i32>,
    pub review_state: Option<ReviewState>,
    pub author: Option<String>,
    pub approvals: i64,
    pub required: i32,
    pub votes: Vec<ReviewVote>,
    pub can_vote: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why_not: Option<String>,
    pub transitions: Vec<AllowedTransition>,
}

/// Body of a vote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewVoteWrite {
    pub version: i32,
    pub decision: ReviewDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// An object waiting for review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewQueueItem {
    pub kind: ReviewKind,
    pub number: i64,
    pub name: String,
    pub head_version: i32,
    pub approved_version: Option<i32>,
    pub review_state: ReviewState,
    pub author: Option<String>,
    pub changed: Vec<String>,
    pub approvals: i64,
    pub required: i32,
    pub folder_id: Option<Uuid>,
    pub updated_at: DateTime<Utc>,
}

/// A page of the review queue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewQueue {
    pub items: Vec<ReviewQueueItem>,
    pub next: Option<String>,
}

/// One object of a bulk review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewBulkItem {
    pub kind: ReviewKind,
    pub number: i64,
    pub version: i32,
}

/// Body of `POST /reviews/bulk` (1 to `maxBulkReview` items).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewBulk {
    pub items: Vec<ReviewBulkItem>,
    pub decision: ReviewDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// An object a bulk review skipped: `version_moved`, `self_approval`, `not_pending`,
/// `not_found` or `review_not_enabled`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewBulkSkip {
    pub kind: ReviewKind,
    pub number: i64,
    pub code: String,
}

/// Outcome of a bulk review.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewBulkResult {
    pub applied: i64,
    pub skipped: Vec<ReviewBulkSkip>,
}

str_enum!(
    /// What an FPQL query selects.
    QueryEntity { Cases = "cases", Requirements = "requirements" }
);

str_enum!(
    /// Type of an FPQL attribute.
    QueryAttributeType { Number = "number", String = "string", Text = "text", Enum = "enum", Bool = "bool", Date = "date", User = "user", Folder = "folder", Field = "field", Ref = "ref" }
);

/// An FPQL error; `position` and `length` in UTF-16 code units, 0-based.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryError {
    pub position: u32,
    pub length: u32,
    pub expected: Vec<String>,
    pub near: String,
    pub message: String,
}

/// Body of `POST /query/validate`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryValidate {
    pub entity: QueryEntity,
    pub query: String,
}

/// Outcome of validating a query.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryValidation {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count_capped: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<QueryError>,
}

/// An attribute of FPQL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryAttribute {
    pub name: String,
    #[serde(rename = "type")]
    pub attr_type: QueryAttributeType,
    pub operators: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
    pub description: String,
}

/// A function of FPQL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryFunction {
    pub name: String,
    pub signature: String,
    pub description: String,
}

/// A case field usable as `field[key]` in FPQL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryField {
    pub key: String,
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
}

/// What FPQL offers for an entity (completion and help).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuerySchema {
    pub entity: QueryEntity,
    pub attributes: Vec<QueryAttribute>,
    pub functions: Vec<QueryFunction>,
    pub fields: Vec<QueryField>,
    pub keywords: Vec<String>,
}

/// A saved query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedQuery {
    pub id: Uuid,
    pub entity: QueryEntity,
    pub name: String,
    pub query: String,
    pub owner: String,
    pub shared: bool,
    pub position: i32,
    pub mine: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Body of `POST /saved-queries`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedQueryWrite {
    pub entity: QueryEntity,
    pub name: String,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i32>,
}

/// Body of `PATCH /saved-queries/{id}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedQueryPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i32>,
}

/// Saved queries of an entity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedQueryList {
    pub items: Vec<SavedQuery>,
}

/// A CI environment of a plan start (`name` `^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanCiEnvironment {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<BTreeMap<String, String>>,
}

/// CI dispatch of a plan start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanCi {
    pub dispatch: bool,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environments: Option<Vec<PlanCiEnvironment>>,
}

str_enum!(
    /// State of a CI dispatch from a plan.
    PlanDispatchStatus { Dispatching = "dispatching", Dispatched = "dispatched", Failed = "failed" }
);

/// A CI dispatch of a plan launch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanDispatch {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub launch_id: Uuid,
    pub launch_number: Option<i64>,
    pub environment: Option<String>,
    pub provider: String,
    pub status: PlanDispatchStatus,
    pub error: Option<String>,
    pub ci_ref: Option<String>,
    pub git_ref: String,
    pub variables: BTreeMap<String, String>,
    pub requested_by: String,
    pub created_at: DateTime<Utc>,
    pub dispatched_at: Option<DateTime<Utc>>,
}

/// A page of plan dispatches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanDispatchList {
    pub items: Vec<PlanDispatch>,
    pub next: Option<String>,
}

/// What a finished launch of a plan ran compared to the plan: `extraNames` holds the first 20;
/// `warning` when `extra > 0` or nothing of a non-empty plan ran.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanCheck {
    pub plan_id: Uuid,
    pub in_plan: i64,
    pub ran: i64,
    pub extra: i64,
    pub missing: i64,
    pub extra_names: Vec<String>,
    pub warning: bool,
}

/// A CI variable that plan starts may set (`rerun.variables`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RerunVariable {
    /// `^[A-Z][A-Z0-9_]{0,63}$`, not starting with `FASTPROOF_`.
    pub name: String,
    /// Allowed values (1 to 50); absent: any value of 1 to 256 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

str_enum!(
    /// How parameter rows are generated.
    ParameterMode { All = "all", Pairwise = "pairwise" }
);

/// A row of parameter values (`id` `r_<ulid>`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ParameterRow {
    pub id: String,
    pub values: BTreeMap<String, String>,
    pub enabled: bool,
}

/// The generator that filled the rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ParameterGenerator {
    pub mode: ParameterMode,
    pub values: BTreeMap<String, Vec<String>>,
}

/// Parameters of a data-driven case: `${column}` in preconditions, action, expected and data.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseParameters {
    pub columns: Vec<String>,
    pub rows: Vec<ParameterRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<ParameterGenerator>,
}

/// Body of `POST /cases/{n}/parameters/generate`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ParameterGenerate {
    pub mode: ParameterMode,
    pub values: BTreeMap<String, Vec<String>>,
}

/// Generated parameter rows (nothing saved).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ParameterRows {
    pub rows: Vec<ParameterRow>,
}

/// An iteration of a parameterized run item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunIteration {
    pub row_id: String,
    pub position: i32,
    pub values: BTreeMap<String, String>,
    pub status: RunStatus,
    pub comment: String,
    pub result_id: Option<Uuid>,
    pub executed_by: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub step_results: Vec<RunStepResult>,
}

/// Body of `PATCH /run-items/{itemId}/iterations/{rowId}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunIterationPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RunStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untested_steps_to: Option<UntestedStepsTo>,
}

/// Answer to an iteration step write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunIterationStepAck {
    pub iteration_status: RunStatus,
    pub item_status: RunStatus,
    pub suggested_status: RunStatus,
    pub revision: i64,
}

str_enum!(
    /// Lifecycle of a requirement.
    RequirementStatus { Draft = "draft", Active = "active", Done = "done", Obsolete = "obsolete" }
);

str_enum!(
    /// Where a requirement came from.
    RequirementOrigin { Ui = "ui", Import = "import", Tracker = "tracker", Ai = "ai" }
);

str_enum!(
    /// Where a requirement link came from.
    RequirementLinkOrigin { Ui = "ui", Code = "code", Import = "import", Ai = "ai" }
);

str_enum!(
    /// Whether a requirement link is confirmed or an AI proposal.
    RequirementLinkStatus { Active = "active", Proposed = "proposed" }
);

str_enum!(
    /// Coverage of a requirement with its children.
    CoverageStatus { Uncovered = "uncovered", Failed = "failed", NotRun = "notRun", Partial = "partial", Passed = "passed" }
);

str_enum!(
    /// Which results a coverage page counts.
    CoverageScopeKind { Default = "default", Plan = "plan", Launches = "launches", Window = "window" }
);

str_enum!(
    /// File format of a coverage export.
    CoverageExportFormat { Csv = "csv", Xlsx = "xlsx" }
);

/// A requirement on the path of another.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequirementPathEntry {
    pub id: Uuid,
    pub number: i64,
    pub title: String,
}

/// The tracker issue a requirement comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequirementTracker {
    pub tracker_id: Option<String>,
    pub issue_key: String,
    pub url: Option<String>,
    pub external_state: Option<String>,
    pub synced_at: Option<DateTime<Utc>>,
}

/// A requirement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Requirement {
    pub id: Uuid,
    pub number: i64,
    pub title: String,
    pub description: String,
    pub description_html: String,
    #[serde(rename = "type")]
    pub req_type: String,
    pub status: RequirementStatus,
    pub parent_id: Option<Uuid>,
    pub path: Vec<RequirementPathEntry>,
    pub depth: i32,
    pub tracker: Option<RequirementTracker>,
    pub content_changed_at: DateTime<Utc>,
    pub origin: RequirementOrigin,
    pub cases: i64,
    pub children: i64,
    pub revision: i64,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_by: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
}

/// A requirement in lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequirementSummary {
    pub number: i64,
    pub title: String,
    #[serde(rename = "type")]
    pub req_type: String,
    pub status: RequirementStatus,
    pub parent_id: Option<Uuid>,
    pub depth: i32,
    pub has_children: bool,
    pub issue_key: Option<String>,
    pub external_state: Option<String>,
    pub cases: i64,
    pub archived: bool,
    pub updated_at: DateTime<Utc>,
}

/// A page of requirements.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequirementList {
    pub items: Vec<RequirementSummary>,
    pub next: Option<String>,
}

/// Body of creating or replacing a requirement.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementWrite {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub req_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RequirementStatus>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<i64>)]
    pub parent_number: Option<Option<i64>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub tracker_id: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub issue_key: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub url: Option<Option<String>>,
}

/// Body of `PATCH /requirements/{n}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub req_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RequirementStatus>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<i64>)]
    pub parent_number: Option<Option<i64>>,
}

/// Body of `POST /requirements/{n}/links`: case numbers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementLinksChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add: Option<Vec<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove: Option<Vec<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<Vec<i64>>,
}

/// A case linked to a requirement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequirementLink {
    pub case_number: i64,
    pub name: String,
    pub status: CaseStatus,
    pub automated: bool,
    pub origin: RequirementLinkOrigin,
    pub link_status: RequirementLinkStatus,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<LastResult>,
}

/// A page of the cases of a requirement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequirementLinkList {
    pub items: Vec<RequirementLink>,
    pub next: Option<String>,
}

/// A requirement of a case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseRequirement {
    pub number: i64,
    pub title: String,
    #[serde(rename = "type")]
    pub req_type: String,
    pub status: RequirementStatus,
    pub origin: RequirementLinkOrigin,
    pub link_status: RequirementLinkStatus,
}

/// Body of `POST /requirements/import` (a dry run first).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementImport {
    pub tracker_id: String,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_number: Option<i64>,
}

/// Cases of a requirement by their newest result in the scope.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoverageCounts {
    pub total: i64,
    pub passed: i64,
    pub failed: i64,
    pub not_run: i64,
    pub skipped: i64,
}

/// Coverage of a requirement with its children.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequirementCoverage {
    pub number: i64,
    pub title: String,
    #[serde(rename = "type")]
    pub req_type: String,
    pub status: RequirementStatus,
    pub depth: i32,
    pub has_children: bool,
    pub coverage: CoverageStatus,
    pub cases: CoverageCounts,
    pub changed_after_run: bool,
}

/// The results a coverage page counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoverageScopeView {
    pub kind: CoverageScopeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_ids: Option<Vec<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<i32>,
    pub launches: i64,
}

/// A page of requirement coverage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoveragePage {
    pub scope: CoverageScopeView,
    pub items: Vec<RequirementCoverage>,
    pub next: Option<String>,
}

/// Body of `POST /coverage/exports`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoverageExportRequest {
    pub format: CoverageExportFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_ids: Option<Vec<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<i32>,
}

/// The test-management level a replica reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaLevel {
    pub instance: String,
    pub tm_level: i32,
    /// The analysis level (0 for builds before FastProof Analysis).
    pub analysis_level: i32,
    pub version: String,
    pub started_at: DateTime<Utc>,
    pub seen_at: DateTime<Utc>,
}

/// `GET /api/v1/admin/replicas`: replicas seen within a day and whether new formats may be
/// written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaLevels {
    pub gate_open: bool,
    pub mode: TmGateMode,
    pub settle_until: Option<DateTime<Utc>>,
    pub replicas: Vec<ReplicaLevel>,
    /// The analysis gate (`FASTPROOF_ANALYSIS_GATE*`).
    pub analysis: crate::analysis::types::GateView,
}

/// A row of `shared_steps` (without `search_text`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedStepRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub number: i64,
    pub name: String,
    pub description: String,
    pub steps: Vec<CaseStep>,
    pub version: i32,
    pub approved_version: Option<i32>,
    pub review_state: Option<ReviewState>,
    pub revision: i64,
    pub origin: SharedStepOrigin,
    pub ai_unreviewed: bool,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_by: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
}

impl SharedStepRow {
    /// `SS-<n>`.
    pub fn key(&self) -> String {
        format!("SS-{}", self.number)
    }
}

/// A row of `requirements`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequirementRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub number: i64,
    pub title: String,
    pub description: String,
    pub req_type: String,
    pub status: RequirementStatus,
    pub parent_id: Option<Uuid>,
    pub path_ids: Vec<Uuid>,
    pub depth: i16,
    pub tracker_id: Option<String>,
    pub issue_key: Option<String>,
    pub url: Option<String>,
    pub external_state: Option<String>,
    pub synced_at: Option<DateTime<Utc>>,
    pub content_changed_at: DateTime<Utc>,
    pub origin: RequirementOrigin,
    pub revision: i64,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_by: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
}

impl RequirementRow {
    /// `REQ-<n>`.
    pub fn key(&self) -> String {
        format!("REQ-{}", self.number)
    }
}
