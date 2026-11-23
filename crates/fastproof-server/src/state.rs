//! Shared state of the HTTP handlers and background workers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fastproof_blob::BlobStore;
use fastproof_ingest::Limits;
use fastproof_store::{HistoryStore, Store};
use franshub_access::{Catalog, Permission};
use franshub_auth::{Platform, PlatformState, ScopeSource};
use tokio::sync::{Notify, Semaphore};
use uuid::Uuid;

use crate::config::Config;
use crate::live::LiveHub;

/// Permissions and built-in roles of FastProof (`30-interfaces.md`, test management in
/// `41-tm-interfaces.md`).
pub fn catalog() -> anyhow::Result<Catalog> {
    Ok(Catalog::builder()
        .permission("projects:read", "See a project and its settings")
        .global_permission("projects:write", "Create and delete projects")
        .permission("launches:read", "Read launches, results and attachments")
        .permission(
            "launches:write",
            "Create launches, upload results, finish launches",
        )
        .permission(
            "results:triage",
            "Set defect types, comments, issue links and mutes",
        )
        .permission("rules:write", "Manage triage rules")
        .permission("settings:write", "Change project settings, delete launches")
        .permission("audit:read", "Read the audit log")
        .permission("cases:read", "Read test cases, folders, fields and plans")
        .permission(
            "cases:write",
            "Create, change and archive test cases and folders",
        )
        .permission(
            "cases:import",
            "Import test cases from files and other tools",
        )
        .permission(
            "cases:sync",
            "Update test cases from code through uploaded results (CI tokens)",
        )
        .permission("plans:write", "Create and change test plans")
        .permission("runs:execute", "Execute manual runs")
        .permission(
            "dashboards:write",
            "Create, change, lock and delete dashboards",
        )
        .role_described(
            "viewer",
            "Read projects, launches and test cases",
            &["projects:read", "launches:read", "cases:read"],
        )
        .role_described(
            "tester",
            "Read and execute manual runs",
            &[
                "projects:read",
                "launches:read",
                "cases:read",
                "runs:execute",
            ],
        )
        .role_described(
            "uploader",
            "Read and upload, update cases from code (CI tokens)",
            &[
                "projects:read",
                "launches:read",
                "launches:write",
                "cases:read",
                "cases:sync",
            ],
        )
        .role_described(
            "editor",
            "Upload, triage, manage rules, test cases, plans and manual runs",
            &[
                "projects:read",
                "launches:read",
                "launches:write",
                "cases:read",
                "cases:sync",
                "results:triage",
                "rules:write",
                "cases:write",
                "cases:import",
                "plans:write",
                "runs:execute",
                "dashboards:write",
            ],
        )
        .role_described("admin", "Everything", &["*"])
        .build()?)
}

/// Resolved permission handles.
#[derive(Clone, Debug)]
pub struct Perms {
    pub projects_read: Permission,
    pub projects_write: Permission,
    pub launches_read: Permission,
    pub launches_write: Permission,
    pub results_triage: Permission,
    pub rules_write: Permission,
    pub settings_write: Permission,
    pub audit_read: Permission,
    pub cases_read: Permission,
    pub cases_write: Permission,
    pub cases_import: Permission,
    pub cases_sync: Permission,
    pub plans_write: Permission,
    pub runs_execute: Permission,
    pub dashboards_write: Permission,
}

impl Perms {
    pub fn new(p: &Platform) -> Self {
        Self {
            projects_read: p.permission("projects:read"),
            projects_write: p.permission("projects:write"),
            launches_read: p.permission("launches:read"),
            launches_write: p.permission("launches:write"),
            results_triage: p.permission("results:triage"),
            rules_write: p.permission("rules:write"),
            settings_write: p.permission("settings:write"),
            audit_read: p.permission("audit:read"),
            cases_read: p.permission("cases:read"),
            cases_write: p.permission("cases:write"),
            cases_import: p.permission("cases:import"),
            cases_sync: p.permission("cases:sync"),
            plans_write: p.permission("plans:write"),
            runs_execute: p.permission("runs:execute"),
            dashboards_write: p.permission("dashboards:write"),
        }
    }
}

/// Projects as the platform's scopes.
pub struct ProjectScopes(pub Store);

#[async_trait]
impl ScopeSource for ProjectScopes {
    async fn exists(&self, key: &str) -> anyhow::Result<bool> {
        Ok(self.0.project(key).await?.is_some())
    }

    async fn list(&self) -> anyhow::Result<Vec<String>> {
        self.0.project_keys().await
    }
}

/// Everything handlers and workers share.
pub struct Inner {
    pub platform: Arc<Platform>,
    pub perms: Perms,
    pub store: Store,
    pub history: Arc<dyn HistoryStore>,
    pub blobs: BlobStore,
    pub cfg: Arc<Config>,
    pub limits: Limits,
    pub live: LiveHub,
    /// Raw uploads received by this replica that are still on its local disk.
    pub local_uploads: Mutex<HashMap<Uuid, PathBuf>>,
    /// Wakes the ingest workers of this replica.
    pub wake: Arc<Notify>,
    /// Bounds concurrent event streams.
    pub sse: Arc<Semaphore>,
    /// Identifies this replica in leases.
    pub instance: String,
    /// The egress-checked HTTP client of integrations.
    pub egress: crate::integrations::egress::Egress,
    /// Analysis operations used by the MCP tools.
    pub analysis: Arc<dyn crate::integrations::analysis::AnalysisOps>,
    /// Live log fan-out of this replica.
    pub logs: crate::stream::live::LogHub,
    /// Kafka (Kafka ingest mode only).
    pub kafka: Option<Arc<fastproof_stream::Kafka>>,
    /// The search backend; `None` searches PostgreSQL.
    pub search: Option<Arc<dyn fastproof_stream::sink::SearchBackend>>,
    /// Per-caller and per-replica guards (`crate::limits`).
    pub guards: crate::limits::Guards,
    /// Signing of media URLs (`crate::api::media`).
    pub media: crate::api::media::Media,
    /// Cancelled when the process starts to stop: event streams end so that the HTTP drain
    /// waits only for real requests.
    pub stopping: tokio_util::sync::CancellationToken,
}

/// The axum state.
#[derive(Clone)]
pub struct AppState(pub Arc<Inner>);

impl std::ops::Deref for AppState {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl PlatformState for AppState {
    fn platform(&self) -> &Platform {
        &self.0.platform
    }
}
