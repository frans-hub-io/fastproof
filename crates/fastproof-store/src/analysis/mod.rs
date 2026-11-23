//! Analysis (stage C): fingerprints, defect types, triage, clusters and similar failures,
//! rules, flakiness and transitions, comparison, dashboard, rollup and retention, test plans.

pub mod boards;
pub mod clusters;
pub mod compare;
pub mod dashboard;
pub mod defects;
pub mod flaky;
pub mod hook;
pub mod indexes;
pub mod launches;
pub mod plans;
pub mod rollup;
pub mod rules;
pub mod settings;
pub mod stats;
pub mod tasks;
pub mod triage;

pub use hook::{PersistedAttempt, after_persist, after_persist_deferred};
pub use settings::ProjectSettings;
