//! Record values of the FastProof topics (JSON, key = launch id; `31-streaming.md`).

use fastproof_model::{ParsedResult, ReportResult};
use fastproof_store::logs::{LogLevel, LogLine, LogStream};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Version of the record values.
pub const VERSION: u32 = 1;

/// `ingest.results`: a [`ReportResult`] from the gateway or a trusted producer, or (from the
/// upload workers) an already parsed result with its idempotency key.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultRecord {
    pub v: u32,
    pub project: String,
    pub launch_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ReportResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parsed: Option<Box<ParsedResult>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_id: Option<Uuid>,
}

/// `ingest.logs`: one line.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRecord {
    pub v: u32,
    pub project: String,
    pub launch_id: Uuid,
    pub result_id: String,
    pub attempt: u32,
    pub seq: u64,
    pub ts: i64,
    pub level: LogLevel,
    pub stream: LogStream,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
}

impl LogRecord {
    pub fn new(project: &str, launch_id: Uuid, l: LogLine) -> Self {
        Self {
            v: VERSION,
            project: project.to_string(),
            launch_id,
            result_id: l.result_id,
            attempt: l.attempt,
            seq: l.seq,
            ts: l.ts,
            level: l.level,
            stream: l.stream,
            message: l.message,
            step: l.step,
        }
    }

    pub fn into_line(self) -> LogLine {
        LogLine {
            result_id: self.result_id,
            attempt: self.attempt,
            seq: self.seq,
            ts: self.ts,
            level: self.level,
            stream: self.stream,
            message: self.message,
            step: self.step,
        }
    }
}

/// `ingest.uploads`: an archive already in blob storage.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadRecord {
    pub v: u32,
    pub project: String,
    pub launch_id: Uuid,
    pub upload_id: Uuid,
    pub blob_key: String,
    pub format: String,
    #[serde(default)]
    pub file_name: Option<String>,
}

/// `events`: an outbox event relayed from PostgreSQL.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRecord {
    pub v: u32,
    /// Outbox id (`events.id`).
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub project: Option<String>,
    pub payload: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<fastproof_store::events::OutboxEvent> for EventRecord {
    fn from(e: fastproof_store::events::OutboxEvent) -> Self {
        Self {
            v: VERSION,
            id: e.id,
            kind: e.kind,
            project: e.project,
            payload: e.payload,
            created_at: e.created_at,
        }
    }
}

impl EventRecord {
    /// The Kafka key: the launch of the event, else its project.
    pub fn key(&self) -> Vec<u8> {
        match self.payload.get("launchId").and_then(|v| v.as_str()) {
            Some(l) => l.as_bytes().to_vec(),
            None => self.project.clone().unwrap_or_default().into_bytes(),
        }
    }
}
