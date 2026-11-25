//! Project settings (`projects.settings`): flaky detection, retention, rollup, and the
//! documents of gates, notifications and issue trackers.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::tm::{CaseReviewSettings, CaseStatus, CaseWorkflowFlow, CaseWorkflowSettings};
use crate::{Store, json_no_nul};

fn is_default_review(r: &CaseReviewSettings) -> bool {
    *r == CaseReviewSettings::default()
}

/// Statuses of a case workflow, at most.
pub const MAX_WORKFLOW_STATUSES: usize = 20;
/// Transitions of one workflow flow, at most.
pub const MAX_WORKFLOW_TRANSITIONS: usize = 100;

fn valid_state_key(k: &str) -> bool {
    let b = k.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

fn valid_color(c: &str) -> bool {
    let b = c.as_bytes();
    b.len() == 7 && b[0] == b'#' && b[1..].iter().all(u8::is_ascii_hexdigit)
}

/// Checks a case workflow (§4.4); the message names the field.
pub fn check_case_workflow(w: &CaseWorkflowSettings) -> Result<(), String> {
    if w.statuses.is_empty() || w.statuses.len() > MAX_WORKFLOW_STATUSES {
        return Err(format!(
            "caseWorkflow.statuses: 1 to {MAX_WORKFLOW_STATUSES} statuses"
        ));
    }
    let mut keys = std::collections::HashSet::new();
    for (i, st) in w.statuses.iter().enumerate() {
        if !valid_state_key(&st.key) {
            return Err(format!(
                "caseWorkflow.statuses[{i}].key must match ^[a-z][a-z0-9_]{{0,31}}$"
            ));
        }
        if !keys.insert(st.key.as_str()) {
            return Err(format!("caseWorkflow.statuses[{i}].key is not unique"));
        }
        let n = st.name.trim();
        if n.is_empty() || st.name.len() > 100 || st.name.chars().any(char::is_control) {
            return Err(format!(
                "caseWorkflow.statuses[{i}].name must have 1 to 100 bytes without control characters"
            ));
        }
        if st.color.as_deref().is_some_and(|c| !valid_color(c)) {
            return Err(format!("caseWorkflow.statuses[{i}].color must be #rrggbb"));
        }
    }
    for c in CaseStatus::ALL {
        if !w.statuses.iter().any(|s| s.category == *c) {
            return Err(format!("caseWorkflow.statuses: no status of category {c}"));
        }
    }
    let flow = |name: &str, f: &CaseWorkflowFlow| -> Result<(), String> {
        if !keys.contains(f.initial.as_str()) {
            return Err(format!("caseWorkflow.{name}.initial names no status"));
        }
        if f.transitions.len() > MAX_WORKFLOW_TRANSITIONS {
            return Err(format!(
                "caseWorkflow.{name}.transitions: at most {MAX_WORKFLOW_TRANSITIONS}"
            ));
        }
        for (i, t) in f.transitions.iter().enumerate() {
            if t.from.is_empty() || t.from.len() > MAX_WORKFLOW_STATUSES + 1 {
                return Err(format!(
                    "caseWorkflow.{name}.transitions[{i}].from: 1 to {} entries",
                    MAX_WORKFLOW_STATUSES + 1
                ));
            }
            for k in t.from.iter().chain(std::iter::once(&t.to)) {
                if k != "*" && !keys.contains(k.as_str()) {
                    return Err(format!(
                        "caseWorkflow.{name}.transitions[{i}]: {k:?} names no status"
                    ));
                }
            }
            if t.requires.as_ref().is_some_and(|r| r.len() > 3) {
                return Err(format!(
                    "caseWorkflow.{name}.transitions[{i}].requires: at most 3 guards"
                ));
            }
        }
        Ok(())
    };
    flow("manual", &w.manual)?;
    flow("automated", &w.automated)
}

/// Checks a review policy (§4.4); the message names the field.
pub fn check_case_review(r: &CaseReviewSettings) -> Result<(), String> {
    if !(1..=5).contains(&r.approvals) {
        return Err("caseReview.approvals must be 1..=5".into());
    }
    Ok(())
}

/// Largest settings document, in bytes of JSON.
pub const MAX_SETTINGS_BYTES: usize = 64 * 1024;
/// Gates or notifications per project, at most.
pub const MAX_SETTINGS_ITEMS: usize = 50;
/// Issue trackers per project, at most.
pub const MAX_ISSUE_TRACKERS: usize = 10;

/// Flaky detection across runs (Allure 3 `flakyDetection`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct FlakySettings {
    /// Past runs that are compared: `-1` disables detection, `0` uses all (up to 50).
    pub history_depth: i32,
    /// Identical outcomes in a row that reset earlier instability.
    pub stabilization_period: u32,
    /// Also mark passed tests as flaky.
    pub include_passed_tests: bool,
}

impl Default for FlakySettings {
    fn default() -> Self {
        Self {
            history_depth: 10,
            stabilization_period: 5,
            include_passed_tests: false,
        }
    }
}

impl FlakySettings {
    pub fn options(&self) -> fastproof_model::FlakyOptions {
        fastproof_model::FlakyOptions {
            history_depth: self.history_depth,
            stabilization_period: self.stabilization_period.max(1),
            include_passed_tests: self.include_passed_tests,
        }
    }

    /// Past results to read for detection.
    pub fn depth(&self) -> i64 {
        match self.history_depth {
            d if d <= 0 => 50,
            d => (d as i64).min(100),
        }
    }
}

/// How long raw results are kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct RetentionSettings {
    /// Launches older than this many days are deleted (after their history is rolled up).
    pub raw_days: u32,
    /// Keep at most this many launches (important ones are not counted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launches_keep: Option<u32>,
    /// Never delete launches marked important.
    pub keep_important: bool,
    /// Launches with a manual run older than this many days are deleted; `0` keeps them
    /// forever (they are evidence for quality audits).
    pub manual_days: u32,
    /// Test case versions: keep at least the newest this many of each case; `0` sets no count.
    /// With `caseVersionsDays` too, a version is pruned only when it is beyond the count and
    /// older than the days. Both `0` (the default) keep every version.
    #[serde(skip_serializing_if = "is_zero")]
    pub case_versions_keep: u32,
    /// Test case versions older than this many days are pruned (beyond `caseVersionsKeep`);
    /// `0` sets no age.
    #[serde(skip_serializing_if = "is_zero")]
    pub case_versions_days: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            raw_days: 90,
            launches_keep: None,
            keep_important: true,
            manual_days: 0,
            case_versions_keep: 0,
            case_versions_days: 0,
        }
    }
}

/// Where code sync puts the cases it creates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum FolderFrom {
    /// Folders from the suite labels (`parentSuite`, `suite`, `subSuite`).
    #[default]
    Suite,
    /// Folders from the package of the full name.
    Package,
    /// All in the root folder.
    None,
}

/// Who owns a case field on automated cases: `code` (sync overwrites it, the UI shows it
/// read-only) or `case` (code fills it only when it creates the case).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum FieldPolicy {
    #[default]
    Code,
    Case,
}

/// Field policy of code sync, per field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct CaseSyncFields {
    pub name: FieldPolicy,
    pub description: FieldPolicy,
    pub links: FieldPolicy,
    pub tags: FieldPolicy,
    pub epic: FieldPolicy,
    pub feature: FieldPolicy,
    pub story: FieldPolicy,
    pub suite: FieldPolicy,
    pub severity: FieldPolicy,
    pub owner: FieldPolicy,
    pub layer: FieldPolicy,
    pub component: FieldPolicy,
}

impl CaseSyncFields {
    /// The policy of a field key (`name`, `description`, `links`, `tags` or a field with an
    /// Allure label); `None` for fields that always belong to the case.
    pub fn policy(&self, key: &str) -> Option<FieldPolicy> {
        Some(match key {
            "name" => self.name,
            "description" => self.description,
            "links" => self.links,
            "tags" => self.tags,
            "epic" => self.epic,
            "feature" => self.feature,
            "story" => self.story,
            "suite" => self.suite,
            "severity" => self.severity,
            "owner" => self.owner,
            "layer" => self.layer,
            "component" => self.component,
            _ => return None,
        })
    }
}

/// Branch patterns of code sync, at most.
pub const MAX_SYNC_BRANCHES: usize = 50;
/// Bytes of one branch pattern, at most.
pub const MAX_BRANCH_PATTERN_BYTES: usize = 200;

/// How cases follow the tests in code (`caseSync`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct CaseSyncSettings {
    pub enabled: bool,
    /// Trusted branches: globs where `*` matches within a segment and `**` across `/`.
    pub branches: Vec<String>,
    /// Also trust the CI default branch (`vcs.defaultBranch`).
    pub use_ci_default_branch: bool,
    /// Also trust debug launches.
    pub include_debug: bool,
    /// Create cases for new identities of trusted launches.
    pub create_cases: bool,
    pub folder_from: FolderFrom,
    /// Folder under which created cases are placed.
    pub root_folder: String,
    /// Days without a trusted result after which a case counts as stale.
    pub stale_after_days: u32,
    /// Match identities to cases by TMS links (`tms` labels) of TestRail or Qase keys.
    pub match_tms_links: bool,
    pub fields: CaseSyncFields,
    /// Allure link types whose links name requirements (skipped when the default).
    #[serde(skip_serializing_if = "is_default_link_types")]
    pub requirement_link_types: Vec<String>,
}

/// Requirement link types, at most.
pub const MAX_REQUIREMENT_LINK_TYPES: usize = 10;

fn default_link_types() -> Vec<String> {
    vec!["requirement".into()]
}

fn is_default_link_types(v: &[String]) -> bool {
    v.len() == 1 && v[0] == "requirement"
}

impl CaseSyncSettings {
    /// Whether `requirementLinkTypes` differs from the default (a gated write, §3).
    pub fn custom_link_types(&self) -> bool {
        !is_default_link_types(&self.requirement_link_types)
    }
}

impl Default for CaseSyncSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            branches: vec!["main".into(), "master".into()],
            use_ci_default_branch: true,
            include_debug: false,
            create_cases: true,
            folder_from: FolderFrom::Suite,
            root_folder: "Automated".into(),
            stale_after_days: 30,
            match_tms_links: false,
            fields: CaseSyncFields::default(),
            requirement_link_types: default_link_types(),
        }
    }
}

impl CaseSyncSettings {
    /// Checks the branch globs, the root folder and the ranges; the message names the field.
    pub fn validate(&self) -> Result<(), String> {
        if self.branches.len() > MAX_SYNC_BRANCHES {
            return Err(format!(
                "caseSync.branches: at most {MAX_SYNC_BRANCHES} patterns"
            ));
        }
        for b in &self.branches {
            check_branch_glob(b).map_err(|e| format!("caseSync.branches: {e}"))?;
        }
        let root = self.root_folder.trim();
        if root.is_empty() || root.len() > 200 || root.chars().any(char::is_control) {
            return Err(
                "caseSync.rootFolder must have 1 to 200 bytes without control characters".into(),
            );
        }
        if !(1..=3650).contains(&self.stale_after_days) {
            return Err("caseSync.staleAfterDays must be 1..=3650".into());
        }
        if self.requirement_link_types.len() > MAX_REQUIREMENT_LINK_TYPES {
            return Err(format!(
                "caseSync.requirementLinkTypes: at most {MAX_REQUIREMENT_LINK_TYPES} types"
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for t in &self.requirement_link_types {
            let b = t.as_bytes();
            let ok = !b.is_empty()
                && b.len() <= 32
                && b[0].is_ascii_alphabetic()
                && b[1..]
                    .iter()
                    .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-');
            if !ok {
                return Err(format!(
                    "caseSync.requirementLinkTypes: {t:?} must match ^[A-Za-z][A-Za-z0-9_-]{{0,31}}$"
                ));
            }
            if !seen.insert(t.as_str()) {
                return Err(format!(
                    "caseSync.requirementLinkTypes: {t:?} is listed twice"
                ));
            }
        }
        Ok(())
    }

    /// Whether a branch is trusted: it matches a pattern, or equals the CI default branch when
    /// `useCiDefaultBranch` is on.
    pub fn branch_trusted(&self, branch: Option<&str>, default_branch: Option<&str>) -> bool {
        let Some(branch) = branch.filter(|b| !b.is_empty()) else {
            return false;
        };
        if self.use_ci_default_branch && default_branch == Some(branch) {
            return true;
        }
        self.branches.iter().any(|p| branch_glob_match(p, branch))
    }
}

/// A branch pattern: 1 to [`MAX_BRANCH_PATTERN_BYTES`] bytes, no whitespace or control
/// characters, no empty segments, `**` only as a whole segment, no other run of `*`.
pub fn check_branch_glob(p: &str) -> Result<(), String> {
    if p.is_empty() || p.len() > MAX_BRANCH_PATTERN_BYTES {
        return Err(format!(
            "patterns must have 1 to {MAX_BRANCH_PATTERN_BYTES} bytes"
        ));
    }
    if p.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("{p:?}: no spaces or control characters"));
    }
    for seg in p.split('/') {
        if seg.is_empty() {
            return Err(format!("{p:?}: empty path segment"));
        }
        if seg != "**" && seg.contains("**") {
            return Err(format!("{p:?}: `**` must be a whole segment"));
        }
    }
    Ok(())
}

/// Glob match of a branch: `*` matches any text within a segment, `**` any number of whole
/// segments (including none). Linear in the pattern times the branch, no backtracking blowup.
pub fn branch_glob_match(pattern: &str, branch: &str) -> bool {
    let ps: Vec<&str> = pattern.split('/').collect();
    let bs: Vec<&str> = branch.split('/').collect();
    // reach[j]: the first i pattern segments can match the first j branch segments.
    let mut reach = vec![false; bs.len() + 1];
    reach[0] = true;
    for p in &ps {
        let mut next = vec![false; bs.len() + 1];
        if *p == "**" {
            let mut any = false;
            for j in 0..=bs.len() {
                any |= reach[j];
                next[j] = any;
            }
        } else {
            for j in 0..bs.len() {
                if reach[j] && segment_match(p, bs[j]) {
                    next[j + 1] = true;
                }
            }
        }
        reach = next;
    }
    reach[bs.len()]
}

fn segment_match(pattern: &str, text: &str) -> bool {
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// When launches are aggregated into daily history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct RollupSettings {
    pub after_days: u32,
}

impl Default for RollupSettings {
    fn default() -> Self {
        Self { after_days: 30 }
    }
}

/// Settings of a project. The analysis parts (`flaky`, `retention`, `rollup`) are typed here;
/// the integration parts (`gates`, `notifications`, `issueTrackers`, `fastmock`, and any other
/// key such as `vcs`) are kept as given and validated by the integrations feature.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectSettings {
    pub flaky: FlakySettings,
    pub retention: RetentionSettings,
    pub rollup: RollupSettings,
    /// Code sync of test cases.
    pub case_sync: CaseSyncSettings,
    /// Configured workflow of cases (absent: any status to any, no states).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub case_workflow: Option<CaseWorkflowSettings>,
    /// Version review of cases and shared steps (skipped when the default, review off).
    #[serde(skip_serializing_if = "is_default_review")]
    pub case_review: CaseReviewSettings,
    /// Quality gates.
    #[schema(value_type = Vec<Object>)]
    pub gates: Vec<serde_json::Value>,
    /// Notification targets.
    #[schema(value_type = Vec<Object>)]
    pub notifications: Vec<serde_json::Value>,
    /// Issue trackers `{type: jira|gitlab|github, urlTemplate, keyRegex}`; triage builds issue
    /// URLs from them when only a key is given.
    #[schema(value_type = Vec<Object>)]
    pub issue_trackers: Vec<serde_json::Value>,
    /// FastMock integration `{url, tokenSecretRef?}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub fastmock: Option<serde_json::Value>,
    /// FastProof Analysis of the project (skipped when the default).
    #[serde(skip_serializing_if = "super::types::AnalysisSettings::is_default")]
    pub analysis: super::types::AnalysisSettings,
    /// Other parts of the document (e.g. `vcs`), kept unchanged.
    #[serde(flatten)]
    #[schema(ignore)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

impl ProjectSettings {
    /// The issue URL for a key, from the first tracker whose `keyRegex` matches the whole key.
    pub fn issue_url(&self, key: &str) -> Option<String> {
        self.issue_trackers.iter().find_map(|t| {
            let template = t.get("urlTemplate")?.as_str()?;
            let re =
                super::rules::compile_regex(&format!("^(?:{})$", t.get("keyRegex")?.as_str()?))
                    .ok()?;
            re.is_match(key)
                .then(|| template.replace("{key}", &url_component(key)))
        })
    }
}

fn url_component(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

impl ProjectSettings {
    /// Checks ranges and sizes; the message names the field.
    pub fn validate(&self) -> Result<(), String> {
        let f = &self.flaky;
        if !(-1..=100).contains(&f.history_depth) {
            return Err("flaky.historyDepth must be -1..=100".into());
        }
        if !(1..=100).contains(&f.stabilization_period) {
            return Err("flaky.stabilizationPeriod must be 1..=100".into());
        }
        let r = &self.retention;
        if !(1..=36_500).contains(&r.raw_days) {
            return Err("retention.rawDays must be 1..=36500".into());
        }
        if let Some(k) = r.launches_keep
            && !(1..=10_000_000).contains(&k)
        {
            return Err("retention.launchesKeep must be 1..=10000000".into());
        }
        if !(1..=36_500).contains(&self.rollup.after_days) {
            return Err("rollup.afterDays must be 1..=36500".into());
        }
        if r.manual_days > 36_500 {
            return Err("retention.manualDays must be 0..=36500".into());
        }
        if r.case_versions_keep > 100_000 {
            return Err("retention.caseVersionsKeep must be 0..=100000".into());
        }
        if r.case_versions_days > 36_500 {
            return Err("retention.caseVersionsDays must be 0..=36500".into());
        }
        self.case_sync.validate()?;
        if let Some(w) = &self.case_workflow {
            check_case_workflow(w)?;
        }
        check_case_review(&self.case_review)?;
        if self.gates.len() > MAX_SETTINGS_ITEMS || self.notifications.len() > MAX_SETTINGS_ITEMS {
            return Err(format!(
                "at most {MAX_SETTINGS_ITEMS} gates and {MAX_SETTINGS_ITEMS} notifications"
            ));
        }
        if self
            .gates
            .iter()
            .chain(&self.notifications)
            .any(|v| !v.is_object())
        {
            return Err("gates and notifications are objects".into());
        }
        if self.issue_trackers.len() > MAX_ISSUE_TRACKERS {
            return Err(format!("at most {MAX_ISSUE_TRACKERS} issue trackers"));
        }
        if self.issue_trackers.iter().any(|v| !v.is_object())
            || self.fastmock.as_ref().is_some_and(|v| !v.is_object())
        {
            return Err("issueTrackers and fastmock are objects".into());
        }
        self.analysis.validate()?;
        let size = serde_json::to_vec(self)
            .map(|v| v.len())
            .unwrap_or(usize::MAX);
        if size > MAX_SETTINGS_BYTES {
            return Err(format!(
                "settings must have at most {MAX_SETTINGS_BYTES} bytes"
            ));
        }
        Ok(())
    }

    /// Settings from the stored document; invalid parts fall back to defaults.
    pub fn from_value(v: serde_json::Value) -> Self {
        let mut s: ProjectSettings = serde_json::from_value(v.clone()).unwrap_or_else(|_| {
            let mut d = ProjectSettings::default();
            if let Some(o) = v.as_object() {
                for (k, slot) in [
                    ("flaky", 0),
                    ("retention", 1),
                    ("rollup", 2),
                    ("caseSync", 3),
                    ("caseWorkflow", 4),
                    ("caseReview", 5),
                    ("analysis", 6),
                ] {
                    if let Some(x) = o.get(k) {
                        match slot {
                            0 => d.flaky = serde_json::from_value(x.clone()).unwrap_or_default(),
                            1 => {
                                d.retention = serde_json::from_value(x.clone()).unwrap_or_default()
                            }
                            2 => d.rollup = serde_json::from_value(x.clone()).unwrap_or_default(),
                            3 => {
                                d.case_sync = serde_json::from_value(x.clone()).unwrap_or_default()
                            }
                            4 => d.case_workflow = serde_json::from_value(x.clone()).ok(),
                            5 => {
                                d.case_review =
                                    serde_json::from_value(x.clone()).unwrap_or_default()
                            }
                            _ => d.analysis = serde_json::from_value(x.clone()).unwrap_or_default(),
                        }
                    }
                }
            }
            d
        });
        if s.flaky.stabilization_period == 0 {
            s.flaky.stabilization_period = 1;
        }
        if s.retention.raw_days == 0 {
            s.retention.raw_days = RetentionSettings::default().raw_days;
        }
        if s.rollup.after_days == 0 {
            s.rollup.after_days = RollupSettings::default().after_days;
        }
        s
    }
}

impl Store {
    pub async fn project_settings(&self, project_id: Uuid) -> anyhow::Result<ProjectSettings> {
        let v: Option<serde_json::Value> =
            sqlx::query_scalar("select settings from projects where id = $1")
                .bind(project_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(ProjectSettings::from_value(v.unwrap_or_default()))
    }

    /// Replaces the settings document (validate it first).
    pub async fn put_project_settings(
        &self,
        project_id: Uuid,
        s: &ProjectSettings,
    ) -> anyhow::Result<()> {
        sqlx::query("update projects set settings = $2, updated_at = now() where id = $1")
            .bind(project_id)
            .bind(json_no_nul(serde_json::to_value(s)?))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Settings of every project that is not deleted.
    pub async fn all_project_settings(
        &self,
    ) -> anyhow::Result<Vec<(Uuid, String, ProjectSettings)>> {
        let rows: Vec<(Uuid, String, serde_json::Value)> = sqlx::query_as(
            "select id, key, settings from projects where deleted_at is null order by key limit 100000",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, key, v)| (id, key, ProjectSettings::from_value(v)))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_validation() {
        let s = ProjectSettings::from_value(serde_json::json!({}));
        assert_eq!(s.flaky.history_depth, 10);
        assert_eq!(s.retention.raw_days, 90);
        assert!(s.retention.keep_important);
        assert_eq!(s.rollup.after_days, 30);
        assert!(s.validate().is_ok());
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["flaky"]["stabilizationPeriod"], 5);
        let mut bad = s.clone();
        bad.flaky.history_depth = 1000;
        assert!(bad.validate().is_err());
        let mut with = s.clone();
        with.issue_trackers.push(serde_json::json!({"type": "jira",
            "urlTemplate": "https://issues.example.com/browse/{key}", "keyRegex": "[A-Z]+-\\d+"}));
        assert_eq!(
            with.issue_url("OPS-12").as_deref(),
            Some("https://issues.example.com/browse/OPS-12")
        );
        assert_eq!(with.issue_url("ops 12"), None);
        let kept = ProjectSettings::from_value(serde_json::json!({"vcs": {"comments": true}}));
        assert_eq!(
            serde_json::to_value(&kept).unwrap()["vcs"]["comments"],
            true
        );
        let partly = ProjectSettings::from_value(
            serde_json::json!({"flaky": {"historyDepth": 3}, "gates": 5}),
        );
        assert_eq!(partly.flaky.history_depth, 3);
    }

    #[test]
    fn case_sync_defaults_and_validation() {
        let s = ProjectSettings::from_value(serde_json::json!({}));
        assert_eq!(s.retention.manual_days, 0);
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(
            v["caseSync"],
            serde_json::json!({"enabled": true, "branches": ["main", "master"],
                "useCiDefaultBranch": true, "includeDebug": false, "createCases": true,
                "folderFrom": "suite", "rootFolder": "Automated", "staleAfterDays": 30,
                "matchTmsLinks": false,
                "fields": {"name": "code", "description": "code", "links": "code", "tags": "code",
                    "epic": "code", "feature": "code", "story": "code", "suite": "code",
                    "severity": "code", "owner": "code", "layer": "code", "component": "code"}})
        );
        assert_eq!(v["retention"]["manualDays"], 0);
        let custom: ProjectSettings = serde_json::from_value(serde_json::json!({
            "caseSync": {"branches": ["release/*", "**/hotfix"], "fields": {"owner": "case"}},
            "retention": {"manualDays": 365}}))
        .unwrap();
        assert!(custom.validate().is_ok());
        assert_eq!(
            custom.case_sync.fields.policy("owner"),
            Some(FieldPolicy::Case)
        );
        assert_eq!(
            custom.case_sync.fields.policy("name"),
            Some(FieldPolicy::Code)
        );
        assert_eq!(custom.case_sync.fields.policy("steps"), None);
        assert!(custom.case_sync.enabled && custom.case_sync.create_cases);
        assert!(
            serde_json::from_value::<ProjectSettings>(
                serde_json::json!({"caseSync": {"fields": {"name": "mine"}}})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ProjectSettings>(
                serde_json::json!({"caseSync": {"folderFrom": "tree"}})
            )
            .is_err()
        );
        for bad in ["", "a b", "rel**", "a//b", "/x", "x/"] {
            let mut b = s.clone();
            b.case_sync.branches = vec![bad.into()];
            assert!(b.validate().is_err(), "{bad:?}");
        }
        let mut b = s.clone();
        b.case_sync.branches = vec!["x".into(); 51];
        assert!(b.validate().is_err());
        let mut b = s.clone();
        b.case_sync.root_folder = " ".into();
        assert!(b.validate().is_err());
        let mut b = s.clone();
        b.retention.manual_days = 36_501;
        assert!(b.validate().is_err());
        let broken = ProjectSettings::from_value(
            serde_json::json!({"caseSync": {"enabled": false}, "gates": 5}),
        );
        assert!(!broken.case_sync.enabled);
    }

    #[test]
    fn branch_globs() {
        assert!(branch_glob_match("main", "main"));
        assert!(!branch_glob_match("main", "main2"));
        assert!(branch_glob_match("release/*", "release/1.2"));
        assert!(!branch_glob_match("release/*", "release/1/2"));
        assert!(branch_glob_match("release/**", "release/1/2"));
        assert!(branch_glob_match("release/**", "release"));
        assert!(branch_glob_match("**/hotfix", "a/b/hotfix"));
        assert!(branch_glob_match("**/hotfix", "hotfix"));
        assert!(branch_glob_match("feat-*-x", "feat-12-x"));
        assert!(!branch_glob_match("*", "a/b"));
        assert!(branch_glob_match("**", "a/b"));
        let long = "a/".repeat(500) + "b";
        let pat = format!("{}c", "**/".repeat(50));
        assert!(!branch_glob_match(&pat, &long));
        let s = CaseSyncSettings::default();
        assert!(s.branch_trusted(Some("main"), None));
        assert!(!s.branch_trusted(Some("feature/x"), None));
        assert!(s.branch_trusted(Some("trunk"), Some("trunk")));
        assert!(!s.branch_trusted(None, Some("trunk")));
        let off = CaseSyncSettings {
            use_ci_default_branch: false,
            ..CaseSyncSettings::default()
        };
        assert!(!off.branch_trusted(Some("trunk"), Some("trunk")));
    }
}
