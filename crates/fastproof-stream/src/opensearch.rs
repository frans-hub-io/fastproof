//! OpenSearch over its REST API: index templates, the logs data stream
//! (`<prefix>logs-default`), monthly result and attempt indexes, an ISM policy for the raw
//! TTL, `_bulk` writes with natural `_id`s, and the search queries.

use std::time::Duration;

use anyhow::{Context, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fastproof_store::logs::{LogEntry, LogHits, LogLevel, LogLine, LogSearch};
use fastproof_store::stream::{SimilarResult, SimilarSource, normalize_message};
use serde_json::{Value, json};
use sha2::Digest;
use uuid::Uuid;

use crate::clickhouse::stream_str;
use crate::config::OpenSearchConfig;
use crate::http::{HttpBackend, error_text};
use crate::sink::{Deletions, SearchBackend, Sink, SinkBatch, flatten_steps, match_key};

/// Time limit of a search request inside OpenSearch.
const SEARCH_TIMEOUT: &str = "30s";

/// The OpenSearch backend.
#[derive(Clone, Debug)]
pub struct OpenSearch {
    http: HttpBackend,
    cfg: OpenSearchConfig,
}

fn month(t: DateTime<Utc>) -> String {
    t.format("%Y.%m").to_string()
}

fn log_id(launch: Uuid, result: &str, attempt: u32, seq: u64) -> String {
    let raw = format!("{launch}:{result}:{attempt}:{seq}");
    if raw.len() <= 200 {
        raw
    } else {
        hex::encode(sha2::Sha256::digest(raw.as_bytes()))
    }
}

impl OpenSearch {
    pub fn new(cfg: OpenSearchConfig) -> anyhow::Result<Self> {
        Ok(Self {
            http: HttpBackend::new(cfg.endpoint.clone(), "FASTPROOF_OPENSEARCH")?,
            cfg,
        })
    }

    fn p(&self) -> &str {
        &self.cfg.index_prefix
    }

    /// The logs data stream.
    pub fn logs_stream(&self) -> String {
        format!("{}logs-default", self.p())
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<(u16, Value)> {
        let mut rb = self.http.request(method, path)?;
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let r = rb.send().await.context("OpenSearch request failed")?;
        let status = r.status().as_u16();
        let text = r.text().await?;
        Ok((
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text.chars().take(500).collect())),
        ))
    }

    async fn ok(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<Value> {
        let (s, v) = self.call(method, path, body).await?;
        if !(200..300).contains(&s) {
            bail!(
                "OpenSearch {path}: HTTP {s}: {}",
                v.to_string().chars().take(500).collect::<String>()
            );
        }
        Ok(v)
    }

    async fn bulk(&self, lines: Vec<Value>) -> anyhow::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let mut body = String::new();
        for l in &lines {
            body.push_str(&serde_json::to_string(l)?);
            body.push('\n');
        }
        let r = self
            .http
            .request(reqwest::Method::POST, "/_bulk")?
            .header("content-type", "application/x-ndjson")
            .body(body)
            .send()
            .await
            .context("OpenSearch bulk request failed")?;
        if !r.status().is_success() {
            bail!("OpenSearch bulk: {}", error_text(r).await);
        }
        let v: Value = r.json().await?;
        if v["errors"].as_bool() == Some(true) {
            for item in v["items"].as_array().into_iter().flatten() {
                let op = item.as_object().and_then(|o| o.values().next());
                let Some(op) = op else { continue };
                let status = op["status"].as_u64().unwrap_or(0);
                if status == 409 || (200..300).contains(&status) {
                    continue;
                }
                bail!(
                    "OpenSearch bulk item failed: {}",
                    op["error"]
                        .to_string()
                        .chars()
                        .take(500)
                        .collect::<String>()
                );
            }
        }
        Ok(())
    }

    fn templates(&self) -> Vec<(String, Value)> {
        let p = self.p();
        let settings = json!({"number_of_shards": 1, "number_of_replicas": self.cfg.replicas});
        vec![
            (
                format!("{p}logs"),
                json!({"index_patterns": [format!("{p}logs-*")], "data_stream": {}, "priority": 200,
                    "template": {"settings": settings, "mappings": {"dynamic": "strict", "properties": {
                        "@timestamp": {"type": "date"}, "project": {"type": "keyword"},
                        "launch_id": {"type": "keyword"}, "launch_created_at": {"type": "date"},
                        "result_id": {"type": "keyword"}, "attempt": {"type": "integer"}, "seq": {"type": "long"},
                        "level": {"type": "keyword"}, "stream": {"type": "keyword"}, "message": {"type": "text"},
                        "step": {"type": "keyword", "ignore_above": 1024}, "chunk_id": {"type": "long"}}}}}),
            ),
            (
                format!("{p}results"),
                json!({"index_patterns": [format!("{p}results-*")], "priority": 200,
                    "template": {"settings": settings, "mappings": {"dynamic": "strict", "properties": {
                        "project": {"type": "keyword"}, "launch_id": {"type": "keyword"},
                        "launch_number": {"type": "long"}, "launch_created_at": {"type": "date"},
                        "series_id": {"type": "keyword"}, "test_case_id": {"type": "keyword"},
                        "name": {"type": "text", "fields": {"raw": {"type": "keyword", "ignore_above": 1024}}},
                        "full_name": {"type": "keyword", "ignore_above": 2048}, "status": {"type": "keyword"},
                        "flaky_in_run": {"type": "boolean"}, "attempt_count": {"type": "integer"},
                        "started_at": {"type": "date"}, "duration_ms": {"type": "long"},
                        "message": {"type": "text"}, "message_norm": {"type": "text"},
                        "match_key": {"type": "keyword"}, "fingerprint": {"type": "keyword"},
                        "branch": {"type": "keyword"}, "env_key": {"type": "keyword"}}}}}),
            ),
            (
                format!("{p}attempts"),
                json!({"index_patterns": [format!("{p}attempts-*")], "priority": 200,
                    "template": {"settings": settings, "mappings": {"dynamic": "strict", "properties": {
                        "project": {"type": "keyword"}, "launch_id": {"type": "keyword"},
                        "launch_created_at": {"type": "date"}, "result_id": {"type": "keyword"},
                        "series_id": {"type": "keyword"}, "client_key": {"type": "keyword", "ignore_above": 1024},
                        "seq": {"type": "long"}, "status": {"type": "keyword"}, "started_at": {"type": "date"},
                        "duration_ms": {"type": "long"}, "message": {"type": "text"}, "trace": {"type": "text"},
                        "steps": {"properties": {"idx": {"type": "integer"}, "parent": {"type": "integer"},
                            "depth": {"type": "integer"}, "name": {"type": "text"}, "status": {"type": "keyword"},
                            "duration_ms": {"type": "long"}, "message": {"type": "text"}}}}}}}),
            ),
        ]
    }

    async fn ensure_policy(&self) -> anyhow::Result<()> {
        let p = self.p();
        let id = format!("{p}raw-ttl");
        let policy = json!({"policy": {"description": "FastProof raw data TTL", "default_state": "hot",
            "states": [
                {"name": "hot", "actions": [], "transitions": [
                    {"state_name": "delete", "conditions": {"min_index_age": format!("{}d", self.cfg.raw_ttl_days)}}]},
                {"name": "delete", "actions": [{"delete": {}}], "transitions": []}
            ],
            "ism_template": [{"index_patterns": [format!("{p}results-*"), format!("{p}attempts-*"),
                format!(".ds-{p}logs-*")], "priority": 100}]}});
        let path = format!("/_plugins/_ism/policies/{id}");
        let (s, cur) = self.call(reqwest::Method::GET, &path, None).await?;
        match s {
            404 => {
                let (s, v) = self.call(reqwest::Method::PUT, &path, Some(policy)).await?;
                if !(200..300).contains(&s) && s != 409 {
                    bail!("creating the ISM policy: HTTP {s}: {v}");
                }
            }
            200 => {
                let age =
                    cur["policy"]["states"][0]["transitions"][0]["conditions"]["min_index_age"]
                        .as_str();
                if age != Some(format!("{}d", self.cfg.raw_ttl_days).as_str()) {
                    let q = format!(
                        "{path}?if_seq_no={}&if_primary_term={}",
                        cur["_seq_no"].as_i64().unwrap_or(0),
                        cur["_primary_term"].as_i64().unwrap_or(1)
                    );
                    self.ok(reqwest::Method::PUT, &q, Some(policy)).await?;
                }
            }
            _ => bail!("reading the ISM policy: HTTP {s}: {cur}"),
        }
        Ok(())
    }
}

#[async_trait]
impl Sink for OpenSearch {
    fn name(&self) -> &'static str {
        "opensearch"
    }

    async fn prepare(&self) -> anyhow::Result<()> {
        if let Err(e) = self.ensure_policy().await {
            tracing::warn!(error = %format!("{e:#}"), "the OpenSearch ISM policy could not be set: raw data is not deleted after FASTPROOF_OPENSEARCH_RAW_TTL_DAYS");
        }
        for (name, body) in self.templates() {
            self.ok(
                reqwest::Method::PUT,
                &format!("/_index_template/{name}"),
                Some(body),
            )
            .await
            .with_context(|| format!("creating index template {name}"))?;
        }
        let ds = self.logs_stream();
        let (s, v) = self
            .call(reqwest::Method::PUT, &format!("/_data_stream/{ds}"), None)
            .await?;
        if !(200..300).contains(&s) && !v.to_string().contains("already exists") {
            bail!("creating data stream {ds}: HTTP {s}: {v}");
        }
        Ok(())
    }

    async fn write(&self, b: &SinkBatch, _token: &str) -> anyhow::Result<()> {
        let p = self.p().to_string();
        let mut lines = Vec::new();
        for r in &b.results {
            let message = r.message.clone().unwrap_or_default();
            lines.push(
                json!({"index": {"_index": format!("{p}results-{}", month(r.launch_created_at)),
                "_id": r.id.to_string(), "version": r.updated_at.timestamp_micros().max(0),
                "version_type": "external_gte"}}),
            );
            lines.push(json!({"project": r.project, "launch_id": r.launch_id, "launch_number": r.launch_number,
                "launch_created_at": r.launch_created_at, "series_id": r.series_id, "test_case_id": r.test_case_id,
                "name": r.name, "full_name": r.full_name, "status": r.status, "flaky_in_run": r.flaky_in_run,
                "attempt_count": r.attempt_count, "started_at": r.started_at, "duration_ms": r.duration_ms,
                "message_norm": normalize_message(&message), "match_key": match_key(r.fingerprint.as_deref(), &message),
                "message": message, "fingerprint": r.fingerprint, "branch": r.branch, "env_key": r.env_key}));
        }
        let version = Utc::now().timestamp_micros().max(0);
        for a in &b.attempts {
            let steps: Vec<Value> = flatten_steps(&a.steps)
                .into_iter()
                .take(2000)
                .map(|s| {
                    json!({"idx": s.idx, "parent": s.parent, "depth": s.depth, "name": s.name,
                    "status": s.status, "duration_ms": s.duration_ms, "message": s.message})
                })
                .collect();
            lines.push(
                json!({"index": {"_index": format!("{p}attempts-{}", month(a.launch_created_at)),
                "_id": a.id.to_string(), "version": version, "version_type": "external_gte"}}),
            );
            lines.push(json!({"project": a.project, "launch_id": a.launch_id, "launch_created_at": a.launch_created_at,
                "result_id": a.result_id, "series_id": a.series_id, "client_key": a.client_key, "seq": a.seq,
                "status": a.status, "started_at": a.started_at, "duration_ms": a.duration_ms, "message": a.message,
                "trace": a.trace, "steps": steps}));
        }
        let ds = self.logs_stream();
        for c in &b.logs {
            for l in &c.lines {
                let ts = DateTime::from_timestamp_millis(l.ts.clamp(0, 4_102_444_800_000))
                    .unwrap_or_default();
                lines.push(json!({"create": {"_index": ds, "_id": log_id(c.launch_id, &l.result_id, l.attempt, l.seq)}}));
                lines.push(json!({"@timestamp": ts, "project": c.project_key, "launch_id": c.launch_id,
                    "launch_created_at": c.launch_created_at, "result_id": l.result_id, "attempt": l.attempt,
                    "seq": l.seq, "level": l.level.as_str(), "stream": stream_str(l.stream), "message": l.message,
                    "step": l.step, "chunk_id": c.id}));
            }
        }
        for part in lines.chunks(4000) {
            self.bulk(part.to_vec()).await?;
        }
        Ok(())
    }

    async fn delete(&self, d: &Deletions) -> anyhow::Result<()> {
        let p = self.p();
        let path = format!(
            "/{p}results-*,{p}attempts-*,{p}logs-*/_delete_by_query?ignore_unavailable=true&allow_no_indices=true&conflicts=proceed&refresh=true"
        );
        let mut by_project: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
        for (project, l) in &d.launches {
            by_project
                .entry(project.as_str())
                .or_default()
                .push(l.to_string());
        }
        for (project, ids) in by_project {
            for part in ids.chunks(1000) {
                let body = json!({"query": {"bool": {"filter": [
                    {"term": {"project": project}}, {"terms": {"launch_id": part}}]}}});
                self.ok(reqwest::Method::POST, &path, Some(body))
                    .await
                    .context("deleting launches")?;
            }
        }
        for (project, at) in &d.projects {
            let body = json!({"query": {"bool": {"filter": [
                {"term": {"project": project}}, {"range": {"launch_created_at": {"lte": at}}}]}}});
            self.ok(reqwest::Method::POST, &path, Some(body))
                .await
                .context("deleting a project")?;
        }
        Ok(())
    }
}

fn trigram_score(a: &str, b: &str) -> f64 {
    let g = |s: &str| -> std::collections::HashSet<[char; 3]> {
        let c: Vec<char> = format!("  {s} ").chars().collect();
        c.windows(3).map(|w| [w[0], w[1], w[2]]).collect()
    };
    let (x, y) = (g(a), g(b));
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    2.0 * x.intersection(&y).count() as f64 / (x.len() + y.len()) as f64
}

#[async_trait]
impl SearchBackend for OpenSearch {
    async fn search_logs(&self, project_key: &str, s: &LogSearch) -> anyhow::Result<LogHits> {
        let mut filter = vec![json!({"term": {"project": project_key}})];
        if let Some(l) = s.launch_id {
            filter.push(json!({"term": {"launch_id": l.to_string()}}));
        }
        if s.from.is_some() || s.to.is_some() {
            filter.push(json!({"range": {"@timestamp": {"gte": s.from, "lte": s.to}}}));
        }
        if let Some(min) = s.level {
            let levels: Vec<&str> = [
                LogLevel::Trace,
                LogLevel::Debug,
                LogLevel::Info,
                LogLevel::Warn,
                LogLevel::Error,
            ]
            .into_iter()
            .filter(|x| *x >= min)
            .map(LogLevel::as_str)
            .collect();
            filter.push(json!({"terms": {"level": levels}}));
        }
        let body = json!({"size": s.limit.clamp(1, 1000), "track_total_hits": false, "timeout": SEARCH_TIMEOUT,
            "sort": [{"@timestamp": "desc"}, {"seq": "desc"}],
            "query": {"bool": {"filter": filter, "must": [{"match_phrase": {"message": s.q}}]}}});
        let v = self
            .ok(
                reqwest::Method::POST,
                &format!("/{}logs-*/_search?ignore_unavailable=true", self.p()),
                Some(body),
            )
            .await?;
        let mut items = Vec::new();
        for h in v["hits"]["hits"].as_array().into_iter().flatten() {
            let d = &h["_source"];
            let Some(launch_id) = d["launch_id"].as_str().and_then(|x| x.parse().ok()) else {
                continue;
            };
            let ts = d["@timestamp"]
                .as_str()
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.timestamp_millis())
                .unwrap_or_default();
            let (Some(level), Ok(stream)) = (
                d["level"].as_str().and_then(LogLevel::parse),
                serde_json::from_value(d["stream"].clone()),
            ) else {
                continue;
            };
            items.push(LogEntry {
                launch_id,
                line: LogLine {
                    result_id: d["result_id"].as_str().unwrap_or_default().to_string(),
                    attempt: d["attempt"].as_u64().unwrap_or(0) as u32,
                    seq: d["seq"].as_u64().unwrap_or(0),
                    ts,
                    level,
                    stream,
                    message: d["message"].as_str().unwrap_or_default().to_string(),
                    step: d["step"].as_str().map(str::to_string),
                },
            });
        }
        Ok(LogHits {
            items,
            partial: false,
        })
    }

    async fn similar(
        &self,
        project_key: &str,
        src: &SimilarSource,
        limit: i64,
    ) -> anyhow::Result<Vec<SimilarResult>> {
        let Some(message) = src.message.as_deref().filter(|m| !m.trim().is_empty()) else {
            return Ok(Vec::new());
        };
        let key = match_key(src.fingerprint.as_deref(), message);
        let norm = normalize_message(message);
        let body = json!({"size": (limit.clamp(1, 100) * 3), "track_total_hits": false, "timeout": SEARCH_TIMEOUT,
            "_source": ["launch_id", "launch_number", "name", "message", "message_norm", "match_key"],
            "query": {"bool": {
                "filter": [{"term": {"project": project_key}}, {"terms": {"status": ["failed", "broken"]}}],
                "must_not": [{"ids": {"values": [src.id.to_string()]}}],
                "should": [{"term": {"match_key": {"value": key, "boost": 10.0}}},
                    {"more_like_this": {"fields": ["message_norm"], "like": norm, "min_term_freq": 1,
                        "min_doc_freq": 1, "minimum_should_match": "30%"}}],
                "minimum_should_match": 1}}});
        let v = self
            .ok(
                reqwest::Method::POST,
                &format!("/{}results-*/_search?ignore_unavailable=true", self.p()),
                Some(body),
            )
            .await?;
        let mut out = Vec::new();
        for h in v["hits"]["hits"].as_array().into_iter().flatten() {
            let d = &h["_source"];
            let (Some(id), Some(launch_id)) = (
                h["_id"].as_str().and_then(|x| x.parse().ok()),
                d["launch_id"].as_str().and_then(|x| x.parse().ok()),
            ) else {
                continue;
            };
            let score = if d["match_key"].as_str() == Some(key.as_str()) {
                1.0
            } else {
                trigram_score(&norm, d["message_norm"].as_str().unwrap_or_default())
            };
            if score < 0.3 {
                continue;
            }
            out.push(SimilarResult {
                result_id: id,
                launch_id,
                launch_number: d["launch_number"].as_i64().unwrap_or_default(),
                name: d["name"].as_str().unwrap_or_default().to_string(),
                message: d["message"].as_str().map(|m| m.chars().take(500).collect()),
                score: (score * 1000.0).round() / 1000.0,
                defect: None,
            });
        }
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out.truncate(limit.clamp(1, 100) as usize);
        Ok(out)
    }

    async fn ping(&self) -> anyhow::Result<()> {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.ok(reqwest::Method::GET, "/", None),
        )
        .await
        .context("OpenSearch did not answer")??;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_scores() {
        let l = Uuid::nil();
        assert_eq!(log_id(l, "r1", 0, 5), format!("{l}:r1:0:5"));
        assert_eq!(log_id(l, &"x".repeat(300), 0, 5).len(), 64);
        assert!(trigram_score("connection refused", "connection refused") > 0.99);
        assert!(trigram_score("connection refused", "assertion failed") < 0.3);
        assert_eq!(
            month(DateTime::from_timestamp_millis(0).unwrap()),
            "1970.01"
        );
    }
}
