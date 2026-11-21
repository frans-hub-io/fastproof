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

/// Permissions and built-in roles of FastProof (`30-interfaces.md`).
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
        .role_described(
            "viewer",
            "Read projects and launches",
            &["projects:read", "launches:read"],
        )
        .role_described(
            "uploader",
            "Read and upload (CI tokens)",
            &["projects:read", "launches:read", "launches:write"],
        )
        .role_described(
            "editor",
            "Upload, triage and manage rules",
            &[
                "projects:read",
                "launches:read",
                "launches:write",
                "results:triage",
                "rules:write",
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
    /// Live log fan-out of this replica.
    pub logs: crate::stream::live::LogHub,
    /// Kafka (Kafka ingest mode only).
    pub kafka: Option<Arc<fastproof_stream::Kafka>>,
    /// The search backend; `None` searches PostgreSQL.
    pub search: Option<Arc<dyn fastproof_stream::sink::SearchBackend>>,
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
