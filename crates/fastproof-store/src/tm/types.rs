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
    CaseBulkAction { Move = "move", SetStatus = "setStatus", SetField = "setField", AddTags = "addTags", RemoveTags = "removeTags", Archive = "archive", Restore = "restore" }
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
    TmJobKind { Import = "import", Export = "export", Backfill = "backfill" }
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
    /// The folder; absent or `null` is the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<CaseStatus>,
    pub body: CaseBody,
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
    pub assign: PlanAssign,
    pub include_automated: bool,
}

/// A launch started from a plan, with the CI hand-off.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanLaunch {
    pub launch: Launch,
    pub testplan_url: String,
    pub ci_hint: String,
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
}

/// Outcome of adding run items.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunItemsAdded {
    pub added: i64,
    pub skipped: i64,
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
    pub untested_to: ManualUntestedTo,
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
    /// Field definitions the import creates (`unknownFieldValues: create`); a dry run lists
    /// the ones its apply will create.
    #[serde(default)]
    pub new_fields: Vec<String>,
    pub unknown_values: BTreeMap<String, Vec<String>>,
    /// The first 1000 errors.
    pub errors: Vec<ImportRowError>,
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

/// Options of a file import.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportOptions {
    pub format: ImportFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps_layout: Option<StepsLayout>,
    pub mode: ImportMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_folder_id: Option<Uuid>,
    pub unknown_field_values: UnknownFieldValues,
}

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
