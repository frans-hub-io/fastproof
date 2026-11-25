//! Analysis (stage C): fingerprints, defect types, triage, clusters and similar failures,
//! rules, flakiness and transitions, comparison, dashboard, rollup and retention, test plans;
//! and FastProof Analysis (contract `76`): the decision journal, the analysis queue and gate,
//! suggestions and memory, unique errors, gate analytics, log patterns, signals, flaky causes
//! and analytics. Every module of the contract is declared here, so later branches only fill
//! their own files.

pub mod analytics;
pub mod attr_index;
pub mod backtest;
pub mod boards;
pub mod clusters;
pub mod compare;
pub mod dashboard;
pub mod decisions;
pub mod defects;
pub mod errors;
pub mod flaky;
pub mod flaky_hints;
pub mod gate;
pub mod gates_history;
pub mod hook;
pub mod indexes;
pub mod launches;
pub mod log_rules;
pub mod memory;
pub mod plans;
pub mod queue;
pub mod rollup;
pub mod rules;
pub mod settings;
pub mod signals;
pub mod stats;
pub mod suggest;
pub mod tasks;
pub mod triage;
pub mod types;

pub use hook::{PersistedAttempt, after_persist, after_persist_deferred};
pub use settings::ProjectSettings;
