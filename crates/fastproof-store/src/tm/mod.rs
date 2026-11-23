//! Test management (stage 2a): cases on top of test identities, folders, field definitions,
//! versions, managed plans and manual runs (`41-tm-interfaces.md`).
//!
//! Every module owns its tables; the DTOs and limits live in [`types`]. Errors that the API
//! answers with a specific status and code are [`TmError`]s; everything else is a plain
//! database error.

pub mod ai;
pub mod backfill;
pub mod cases;
pub mod export;
pub mod fields;
pub mod folders;
pub mod identity;
pub mod import;
pub mod keys;
pub mod lookup;
pub mod people;
pub mod plans;
pub mod projection;
pub mod runs;
pub mod sync;
#[cfg(any(test, feature = "test-util"))]
pub mod testutil;
pub mod types;
pub mod versions;

use std::fmt;

use serde_json::Value;
use uuid::Uuid;

pub use types::*;

/// An outcome the API answers with its own status and code.
#[derive(Debug)]
pub enum TmError {
    /// 404 `not_found` (unknown id or an id of another project).
    NotFound(&'static str),
    /// 400 with a code such as `invalid_case`, `invalid_markdown` or `bad_request`.
    Invalid {
        code: &'static str,
        message: String,
        details: Option<Value>,
    },
    /// 409 with a code such as `folder_name_taken`, `folder_not_empty` or `limit_reached`.
    Conflict {
        code: &'static str,
        message: String,
        details: Option<Value>,
    },
    /// 403 with a code such as `ai_self_review`: the caller may not do this to this object.
    Forbidden { code: &'static str, message: String },
    /// 412 `precondition_failed`: the revision moved.
    Stale {
        current_revision: i64,
        changed_by: Option<String>,
        source: Option<String>,
    },
    /// Anything else (logged, answered 500).
    Other(anyhow::Error),
}

/// Result of operations that may answer a [`TmError`].
pub type TmResult<T> = Result<T, TmError>;

impl TmError {
    /// 400 `bad_request`.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::Invalid {
            code: "bad_request",
            message: message.into(),
            details: None,
        }
    }

    /// 400 with a code.
    pub fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        Self::Invalid {
            code,
            message: message.into(),
            details: None,
        }
    }

    /// 409 with a code.
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::Conflict {
            code,
            message: message.into(),
            details: None,
        }
    }

    /// 409 `limit_reached` with `details {limit, max}`; `limit` is the camelCase name of the
    /// [`TmLimits`] field (e.g. `maxFolders`).
    pub fn limit(limit: &'static str, max: impl Into<u64>) -> Self {
        let max = max.into();
        Self::Conflict {
            code: "limit_reached",
            message: format!("{limit} ({max}) reached"),
            details: Some(serde_json::json!({"limit": limit, "max": max})),
        }
    }
}

impl fmt::Display for TmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(what) => write!(f, "{what} not found"),
            Self::Invalid { code, message, .. }
            | Self::Conflict { code, message, .. }
            | Self::Forbidden { code, message } => write!(f, "{code}: {message}"),
            Self::Stale {
                current_revision, ..
            } => write!(f, "precondition failed (revision {current_revision})"),
            Self::Other(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for TmError {}

impl From<anyhow::Error> for TmError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

impl From<sqlx::Error> for TmError {
    fn from(e: sqlx::Error) -> Self {
        Self::Other(e.into())
    }
}

impl From<serde_json::Error> for TmError {
    fn from(e: serde_json::Error) -> Self {
        Self::Other(e.into())
    }
}

/// The name of the unique or check constraint a database error violated, if any.
pub fn violated_constraint(e: &sqlx::Error) -> Option<&str> {
    match e {
        sqlx::Error::Database(d) => d.constraint(),
        _ => None,
    }
}

/// Serializes changes of one kind (`area`) within a project until the transaction ends.
pub async fn lock_project(
    conn: &mut sqlx::PgConnection,
    project_id: Uuid,
    area: &str,
) -> sqlx::Result<()> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1 || ':' || $2::text, 0))")
        .bind(format!("fastproof:tm:{area}"))
        .bind(project_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// A name of a case, folder, field or plan: 1 to `max` bytes after trimming, no control
/// characters. Returns the trimmed name.
pub fn check_name(what: &str, name: &str, max: usize) -> TmResult<String> {
    let t = name.trim();
    if t.is_empty() || t.len() > max {
        return Err(TmError::bad_request(format!(
            "{what} must have 1 to {max} bytes"
        )));
    }
    if t.chars().any(char::is_control) {
        return Err(TmError::bad_request(format!(
            "{what} must not contain control characters"
        )));
    }
    Ok(t.to_string())
}
