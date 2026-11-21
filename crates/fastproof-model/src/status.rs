use serde::{Deserialize, Serialize};

/// Final outcome of a test result, attempt or step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Passed,
    Failed,
    Broken,
    Skipped,
    #[default]
    Unknown,
}

impl Status {
    /// Lenient conversion of the status strings used by Allure, CTRF and Playwright.
    pub fn parse_lenient(value: &str) -> Status {
        match value.trim().to_ascii_lowercase().as_str() {
            "passed" | "pass" | "success" | "ok" | "expected" => Status::Passed,
            "failed" | "fail" | "failure" | "unexpected" | "timedout" | "timed_out" => {
                Status::Failed
            }
            "broken" | "error" | "errored" | "interrupted" => Status::Broken,
            "skipped" | "skip" | "pending" | "disabled" | "ignored" => Status::Skipped,
            _ => Status::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Broken => "broken",
            Status::Skipped => "skipped",
            Status::Unknown => "unknown",
        }
    }

    /// Statuses that count for flaky detection and transitions (Allure 3).
    pub fn is_significant(self) -> bool {
        matches!(self, Status::Passed | Status::Failed | Status::Broken)
    }

    pub fn is_failure(self) -> bool {
        matches!(self, Status::Failed | Status::Broken)
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Execution stage as written by Allure writers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Scheduled,
    Running,
    #[default]
    Finished,
    Pending,
    Interrupted,
}

impl Stage {
    pub fn parse_lenient(value: &str) -> Option<Stage> {
        match value.trim().to_ascii_lowercase().as_str() {
            "scheduled" => Some(Stage::Scheduled),
            "running" => Some(Stage::Running),
            "finished" => Some(Stage::Finished),
            "pending" => Some(Stage::Pending),
            "interrupted" => Some(Stage::Interrupted),
            _ => None,
        }
    }
}
