//! FastMock correlation (X7): the requests a FastMock instance received during an attempt,
//! from its admin journal (`GET /__admin/requests`), by the attempt's trace id or, without a
//! match, by the attempt's time window (and FastMock space, from the result label or launch
//! attribute `fastmock.space`).
//!
//! The journal API filters by `since` and `space` only, so FastProof reads at most
//! [`MAX_PAGES`] pages of [`PAGE`] entries (newest first) and filters them itself.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use fastproof_store::Project;
use franshub_http::ApiError;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::settings::IntegrationSettings;
use crate::state::AppState;

pub const PAGE: usize = 500;
pub const MAX_PAGES: usize = 4;
pub const MAX_ITEMS: usize = 500;

/// One request FastMock answered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MockInteraction {
    pub at: DateTime<Utc>,
    pub method: String,
    /// Path and query.
    pub url: String,
    pub status: u16,
    /// Id of the stub that answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_stub: Option<String>,
    /// `true` when no stub matched (see FastMock's near misses for the closest ones).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub near_miss: Option<bool>,
    /// A fault FastMock injected (not recorded in the journal of FastMock 0.1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fault: Option<String>,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
}

/// The requests correlated with an attempt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MockInteractions {
    /// `traceId` or `timeWindow`.
    pub source: String,
    /// 0 to 1: how likely the items belong to this attempt.
    pub confidence: f64,
    pub items: Vec<MockInteraction>,
    pub attempt: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
    /// More journal entries than were read could belong to the window.
    pub truncated: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    at: DateTime<Utc>,
    method: String,
    path: String,
    #[serde(default)]
    query: String,
    status: u16,
    #[serde(default)]
    stub_id: Option<String>,
    #[serde(default)]
    matched: bool,
    #[serde(default)]
    duration_ms: u64,
    #[serde(default)]
    trace_id: Option<String>,
    #[serde(default)]
    space: Option<String>,
}

#[derive(Deserialize)]
struct Page {
    requests: Vec<Entry>,
}

/// The trace id of a `traceparent` (`00-<trace>-<span>-<flags>`) or a bare trace id.
pub fn trace_id_of(v: &str) -> Option<String> {
    let v = v.trim().to_ascii_lowercase();
    let t = if v.contains('-') {
        v.split('-').nth(1)?.to_string()
    } else {
        v
    };
    (t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit()) && t != "0".repeat(32)).then_some(t)
}

fn label(v: &serde_json::Value, key_field: &str, name: &str, value_field: &str) -> Option<String> {
    v.as_array()?
        .iter()
        .find(|l| l.get(key_field).and_then(|n| n.as_str()) == Some(name))
        .and_then(|l| l.get(value_field))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn item(e: Entry) -> MockInteraction {
    let url = if e.query.is_empty() {
        e.path
    } else {
        format!("{}?{}", e.path, e.query.trim_start_matches('?'))
    };
    MockInteraction {
        at: e.at,
        method: e.method,
        url,
        status: e.status,
        matched_stub: e.stub_id,
        near_miss: (!e.matched).then_some(true),
        fault: None,
        duration_ms: e.duration_ms,
        trace_id: e.trace_id,
        space: e.space,
    }
}

fn unavailable(msg: impl Into<String>) -> ApiError {
    ApiError::new(axum::http::StatusCode::BAD_GATEWAY, "upstream", msg)
}

/// Correlates FastMock requests with one attempt of a result.
pub async fn interactions(
    s: &AppState,
    project: &Project,
    result_id: Uuid,
    attempt: Option<i32>,
) -> Result<MockInteractions, ApiError> {
    let doc = s.store.project_settings(project.id).await?;
    let settings = IntegrationSettings::parse(&doc)
        .map_err(|e| ApiError::conflict(format!("invalid project settings: {e}")))?;
    let Some(fm) = settings.fastmock else {
        return Err(ApiError::conflict(
            "FastMock is not configured for this project (settings.fastmock)",
        ));
    };
    let w = s
        .store
        .attempt_window(project.id, result_id, attempt)
        .await?
        .ok_or_else(|| ApiError::not_found("attempt"))?;
    let space = label(&w.labels, "name", "fastmock.space", "value")
        .or_else(|| label(&w.launch_attributes, "key", "fastmock.space", "value"))
        .filter(|v| !v.is_empty() && v.len() <= 128);
    let trace = w.trace_id.as_deref().and_then(trace_id_of);
    if trace.is_none() && w.started_at.is_none() {
        return Err(ApiError::bad_request(
            "the attempt has neither a trace id nor a start time to correlate by",
        ));
    }
    let token = match &fm.token_secret_ref {
        Some(r) => Some(
            s.project_secret(project.id, r)
                .await
                .map_err(|e| ApiError::conflict(e.to_string()))?,
        ),
        None => None,
    };
    let start = w.started_at.map(|t| t - ChronoDuration::seconds(2));
    let stop = w
        .stopped_at
        .or(w.started_at)
        .map(|t| t + ChronoDuration::seconds(2));
    let base = fm.url.trim_end_matches('/');
    let mut by_trace = Vec::new();
    let mut by_window = Vec::new();
    let mut truncated = false;
    for page in 0..MAX_PAGES {
        let mut q: Vec<(String, String)> = vec![
            ("limit".into(), PAGE.to_string()),
            ("offset".into(), (page * PAGE).to_string()),
        ];
        if let Some(t) = start {
            q.push(("since".into(), t.to_rfc3339()));
        }
        if let Some(sp) = &space {
            q.push(("space".into(), sp.clone()));
        }
        let mut rb = s
            .egress
            .request(Method::GET, &format!("{base}/__admin/requests"))
            .map_err(|e| unavailable(e.to_string()))?
            .query(&q);
        if let Some(t) = &token {
            rb = rb.bearer_auth(t.expose().trim());
        }
        let r = s
            .egress
            .send(rb, 32 * 1024 * 1024)
            .await
            .map_err(|e| unavailable(format!("FastMock: {e}")))?;
        if r.status != 200 {
            return Err(unavailable(format!(
                "FastMock answered HTTP {} to the journal query",
                r.status
            )));
        }
        let p: Page = r
            .json()
            .map_err(|_| unavailable("FastMock answered an unexpected journal format"))?;
        let n = p.requests.len();
        for e in p.requests {
            let in_window = match (start, stop) {
                (Some(a), Some(b)) => e.at >= a && e.at <= b,
                _ => false,
            };
            let same_trace = match (&trace, e.trace_id.as_deref().and_then(trace_id_of)) {
                (Some(t), Some(et)) => *t == et,
                _ => false,
            };
            if same_trace && by_trace.len() < MAX_ITEMS {
                by_trace.push(item(e));
            } else if in_window && by_window.len() < MAX_ITEMS {
                by_window.push(item(e));
            }
        }
        if n < PAGE {
            break;
        }
        if page + 1 == MAX_PAGES {
            truncated = true;
        }
    }
    let (source, mut confidence, mut items) = if !by_trace.is_empty() {
        ("traceId", 0.95, by_trace)
    } else if space.is_some() {
        ("timeWindow", 0.6, by_window)
    } else {
        ("timeWindow", 0.3, by_window)
    };
    if truncated {
        confidence /= 2.0;
    }
    items.sort_by_key(|i| i.at);
    Ok(MockInteractions {
        source: source.into(),
        confidence,
        items,
        attempt: w.no,
        space,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_ids() {
        assert_eq!(
            trace_id_of("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01").as_deref(),
            Some("0af7651916cd43dd8448eb211c80319c")
        );
        assert_eq!(
            trace_id_of("0AF7651916CD43DD8448EB211C80319C").as_deref(),
            Some("0af7651916cd43dd8448eb211c80319c")
        );
        assert!(trace_id_of("00-00000000000000000000000000000000-b7ad6b7169203331-01").is_none());
        assert!(trace_id_of("nope").is_none());
    }
}
