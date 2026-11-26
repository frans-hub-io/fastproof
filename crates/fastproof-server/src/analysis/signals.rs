//! Signals of the analysis role (contract §7.7): the `signals` step resolves the launch's
//! pending pushed signals and, for projects with FastMock settings, reads the FastMock journal
//! of the launch's failed attempts (phase 1, through the existing correlation with its egress,
//! pages and limits) and writes `mock` signals; the job `analysis.signals.resolve` matches
//! pending pushed signals to attempts that arrived later and deletes the ones that never match.
//!
//! Mock signal kinds, per attempt and from the requests FastMock received during it:
//! - `unmatched`: no stub matched a request to a route a stub answered elsewhere in the launch
//!   (a near miss: wrong body, header or query);
//! - `unexpected_calls`: no stub matched a request to a route no stub answered in the launch;
//! - `stub_error`: a stub answered 4xx or 5xx;
//! - `slow`: a stub took more than 2 s, or more than the 95th percentile of that stub in the
//!   launch (with at least [`P95_MIN_CALLS`] calls);
//! - `injected_fault`: FastMock reports a fault it injected (once its journal records them).
//!
//! `confidence` is `exact` when the requests were found by the attempt's trace id and `window`
//! when only its time window matched.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::Utc;
use fastproof_store::analysis::signals::NewSignal;
use fastproof_store::analysis::types::{LaunchCtx, SignalConfidence, StepOutcome};
use futures::StreamExt;
use serde_json::{Map, Value, json};

use super::pipeline::LaunchJob;
use crate::integrations::fastmock::{MockInteraction, MockInteractions, interactions};
use crate::integrations::settings::IntegrationSettings;
use crate::state::AppState;

/// Whether this build takes signals (`AnalysisFeatures.signals`).
pub const AVAILABLE: bool = true;

/// FastMock journal reads at once per launch.
pub const FASTMOCK_CONCURRENCY: usize = 4;
/// Wall time of the FastMock part of a launch's step.
pub const FASTMOCK_BUDGET: Duration = Duration::from_secs(30);
/// Failed reads in a row (with none succeeding) after which the step stops calling FastMock.
pub const FASTMOCK_GIVE_UP: u32 = 4;
/// A stub call slower than this is `slow`.
pub const SLOW_MS: u64 = 2000;
/// Calls of a stub in the launch needed before its 95th percentile counts.
pub const P95_MIN_CALLS: usize = 20;
/// Wall time of one run of the resolution job.
pub const RESOLVE_BUDGET: Duration = Duration::from_secs(20);
/// Characters of a path kept in a signal's `detail` (the query string is never kept).
const DETAIL_PATH_CHARS: usize = 200;
/// Journal entries the step keeps over all attempts of a launch, at most (time windows of
/// parallel attempts overlap, so the same entries come back for many attempts); beyond it the
/// step stops reading and is `partial`.
pub const MAX_KEPT_ENTRIES: usize = 20_000;
/// Bytes of a request path kept for classification (the query string is dropped).
const KEPT_PATH_BYTES: usize = 512;
/// Bytes of a method, stub id or fault kept for classification.
const KEPT_FIELD_BYTES: usize = 64;

/// Step `signals`: resolves the launch's pending pushed signals, then the FastMock phase 1.
pub async fn on_launch(s: &AppState, job: &LaunchJob) -> anyhow::Result<StepOutcome> {
    let resolved = s
        .store
        .resolve_launch_signals(job.project.id, &job.launch)
        .await?;
    let mock = fastmock(s, job).await?;
    Ok(match mock {
        Mock::NotConfigured if resolved == 0 => {
            StepOutcome::skipped("no FastMock settings and no signals to resolve")
        }
        Mock::NotConfigured => StepOutcome::done(json!({"resolved": resolved})),
        Mock::Ran(r) => {
            let detail = json!({"resolved": resolved, "fastmock": r.detail()});
            if r.read == 0 && r.failed > 0 {
                let mut o = StepOutcome::failed(&format!(
                    "FastMock: {}",
                    r.first_error.as_deref().unwrap_or("unavailable")
                ));
                o.detail.insert("resolved".into(), json!(resolved));
                o.detail.insert("fastmock".into(), r.detail());
                o
            } else if r.partial() {
                StepOutcome::partial(detail)
            } else {
                StepOutcome::done(detail)
            }
        }
    })
}

/// Job `analysis.signals.resolve` (30 s).
pub async fn resolve(s: &AppState) -> anyhow::Result<()> {
    let t = Instant::now();
    let n = s.store.resolve_signals(RESOLVE_BUDGET).await?;
    metrics::histogram!("fastproof_analysis_job_seconds", "job" => "analysis.signals.resolve")
        .record(t.elapsed().as_secs_f64());
    if n > 0 {
        tracing::debug!(signals = n, "pending signals matched to attempts");
    }
    Ok(())
}

enum Mock {
    NotConfigured,
    Ran(MockRun),
}

/// What the FastMock part of the step did.
#[derive(Debug, Default)]
struct MockRun {
    /// Failed attempts considered (at most `fastmock_attempts`).
    attempts: usize,
    /// More failed attempts than the limit.
    capped: bool,
    read: u32,
    failed: u32,
    first_error: Option<String>,
    /// The time budget ran out, FastMock kept failing, or the kept journal entries reached
    /// [`MAX_KEPT_ENTRIES`] before every attempt was read.
    stopped: bool,
    written: u64,
}

impl MockRun {
    fn partial(&self) -> bool {
        self.capped || self.stopped || self.failed > 0
    }

    fn detail(&self) -> Value {
        let mut d = json!({"attempts": self.attempts, "read": self.read, "failed": self.failed,
            "signals": self.written, "truncated": self.capped || self.stopped});
        if let Some(e) = &self.first_error {
            d["error"] = json!(e);
        }
        d
    }
}

async fn fastmock(s: &AppState, job: &LaunchJob) -> anyhow::Result<Mock> {
    let limit = job.limits.fastmock_attempts;
    if limit == 0 {
        return Ok(Mock::NotConfigured);
    }
    let doc = s.store.project_settings_json(job.project.id).await?;
    let configured = IntegrationSettings::parse(&doc)
        .map(|st| st.fastmock.is_some())
        .unwrap_or(false);
    if !configured {
        return Ok(Mock::NotConfigured);
    }
    let mut attempts = s.store.failed_attempts(&job.launch, limit + 1).await?;
    let mut run = MockRun::default();
    if attempts.len() > limit as usize {
        attempts.truncate(limit as usize);
        run.capped = true;
    }
    run.attempts = attempts.len();
    if attempts.is_empty() {
        return Ok(Mock::Ran(run));
    }
    let deadline = tokio::time::Instant::now() + FASTMOCK_BUDGET;
    let project = &job.project;
    let mut reads =
        futures::stream::iter(attempts.into_iter().map(|(rid, no)| async move {
            (rid, no, interactions(s, project, rid, Some(no)).await)
        }))
        .buffer_unordered(FASTMOCK_CONCURRENCY);
    let mut got: Vec<(uuid::Uuid, i32, MockInteractions)> = Vec::new();
    let mut kept = 0usize;
    loop {
        match tokio::time::timeout_at(deadline, reads.next()).await {
            Ok(Some((rid, no, Ok(mut m)))) => {
                run.read += 1;
                compact(&mut m);
                kept += m.items.len();
                got.push((rid, no, m));
                if kept >= MAX_KEPT_ENTRIES && ((run.read + run.failed) as usize) < run.attempts {
                    run.stopped = true;
                    break;
                }
            }
            Ok(Some((_, _, Err(e)))) => {
                run.failed += 1;
                if run.first_error.is_none() {
                    tracing::warn!(launch = %job.launch.id, error = %e.body().message,
                        "reading the FastMock journal failed");
                    run.first_error = Some(format!(
                        "the journal could not be read (HTTP {}, {}); the server log has the cause",
                        e.status().as_u16(),
                        e.body().code
                    ));
                }
                if run.read == 0 && run.failed >= FASTMOCK_GIVE_UP {
                    run.stopped = true;
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => {
                run.stopped = true;
                break;
            }
        }
    }
    drop(reads);
    if run.capped || run.stopped {
        metrics::counter!("fastproof_analysis_budget_exhausted_total", "kind" => "fastmock")
            .increment(1);
    }
    let signals = classify(&got);
    if !signals.is_empty() {
        let ctx = LaunchCtx {
            project_id: job.project.id,
            project_key: &job.project.key,
            launch: &job.launch,
            settings: &job.settings,
            limits: &job.limits,
            now: Utc::now(),
        };
        run.written = s.store.write_mock_signals(&ctx, &signals).await?;
    }
    Ok(Mock::Ran(run))
}

fn cut(s: &str, max: usize) -> String {
    let mut i = s.len().min(max);
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    s[..i].to_string()
}

/// Keeps only what [`classify`] reads of each entry, bounded: the path without its query,
/// method, status, duration, stub, fault, time.
fn compact(m: &mut MockInteractions) {
    for i in &mut m.items {
        let path = i.url.split(['?', '#']).next().unwrap_or_default();
        i.url = cut(path, KEPT_PATH_BYTES);
        i.method = cut(&i.method, KEPT_FIELD_BYTES);
        i.matched_stub = i.matched_stub.as_deref().map(|v| cut(v, KEPT_FIELD_BYTES));
        i.fault = i.fault.as_deref().map(|v| cut(v, KEPT_FIELD_BYTES));
        i.trace_id = None;
        i.space = None;
    }
    m.items.shrink_to_fit();
}

/// A path with its query removed and id-like segments (numbers, UUIDs, long hex) replaced by
/// `{}`, so `/users/1` and `/users/2` are one route.
pub fn route_of(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let segs: Vec<&str> = path
        .split('/')
        .map(|seg| {
            let id_like = !seg.is_empty()
                && (seg.bytes().all(|b| b.is_ascii_digit())
                    || uuid::Uuid::parse_str(seg).is_ok()
                    || (seg.len() >= 16 && seg.bytes().all(|b| b.is_ascii_hexdigit())));
            if id_like { "{}" } else { seg }
        })
        .collect();
    segs.join("/")
}

fn short_path(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    path.chars().take(DETAIL_PATH_CHARS).collect()
}

fn unmatched(i: &MockInteraction) -> bool {
    i.near_miss == Some(true)
}

/// The key of one journal entry, so entries seen by several attempts (time windows overlap)
/// count once for the launch-wide statistics.
fn entry_key(i: &MockInteraction) -> (i64, String, String, u16, u64) {
    (
        i.at.timestamp_micros(),
        i.method.clone(),
        i.url.clone(),
        i.status,
        i.duration_ms,
    )
}

fn p95(mut v: Vec<u64>) -> Option<u64> {
    if v.len() < P95_MIN_CALLS {
        return None;
    }
    v.sort_unstable();
    let idx = ((v.len() as f64) * 0.95).ceil() as usize;
    v.get(idx.saturating_sub(1)).copied()
}

/// The `mock` signals of the attempts read from FastMock (see the module docs for the kinds).
pub fn classify(got: &[(uuid::Uuid, i32, MockInteractions)]) -> Vec<NewSignal> {
    let mut seen: HashSet<(i64, String, String, u16, u64)> = HashSet::new();
    let mut answered: HashSet<(String, String)> = HashSet::new();
    let mut per_stub: HashMap<&str, Vec<u64>> = HashMap::new();
    for (_, _, m) in got {
        for i in &m.items {
            if !seen.insert(entry_key(i)) {
                continue;
            }
            if !unmatched(i) {
                answered.insert((i.method.to_ascii_uppercase(), route_of(&i.url)));
            }
            if let Some(stub) = i.matched_stub.as_deref() {
                per_stub.entry(stub).or_default().push(i.duration_ms);
            }
        }
    }
    let p95s: HashMap<&str, u64> = per_stub
        .into_iter()
        .filter_map(|(k, v)| p95(v).map(|p| (k, p)))
        .collect();
    let mut out = Vec::new();
    for (rid, no, m) in got {
        let confidence = if m.source == "traceId" {
            SignalConfidence::Exact
        } else {
            SignalConfidence::Window
        };
        let mut kinds: BTreeMap<&'static str, Vec<&MockInteraction>> = BTreeMap::new();
        for i in &m.items {
            if unmatched(i) {
                let route = (i.method.to_ascii_uppercase(), route_of(&i.url));
                let kind = if answered.contains(&route) {
                    "unmatched"
                } else {
                    "unexpected_calls"
                };
                kinds.entry(kind).or_default().push(i);
                continue;
            }
            if i.status >= 400 {
                kinds.entry("stub_error").or_default().push(i);
            }
            let over_p95 = i
                .matched_stub
                .as_deref()
                .and_then(|st| p95s.get(st))
                .is_some_and(|p| i.duration_ms > *p);
            if i.duration_ms > SLOW_MS || over_p95 {
                kinds.entry("slow").or_default().push(i);
            }
            if i.fault.as_deref().is_some_and(|f| !f.is_empty()) {
                kinds.entry("injected_fault").or_default().push(i);
            }
        }
        for (kind, items) in kinds {
            let first = if kind == "slow" {
                items.iter().max_by_key(|i| i.duration_ms).copied()
            } else {
                items.first().copied()
            };
            let Some(first) = first else {
                continue;
            };
            let mut d = Map::new();
            d.insert("count".into(), json!(items.len()));
            d.insert("method".into(), json!(first.method));
            d.insert("path".into(), json!(short_path(&first.url)));
            d.insert("status".into(), json!(first.status));
            d.insert("durationMs".into(), json!(first.duration_ms));
            if let Some(st) = &first.matched_stub {
                d.insert(
                    "stub".into(),
                    json!(st.chars().take(64).collect::<String>()),
                );
            }
            if let Some(f) = &first.fault {
                d.insert(
                    "fault".into(),
                    json!(f.chars().take(64).collect::<String>()),
                );
            }
            if let Some(sp) = &m.space {
                d.insert("space".into(), json!(sp));
            }
            d.insert("correlation".into(), json!(m.source));
            out.push(NewSignal {
                result_id: *rid,
                attempt: *no,
                kind: kind.to_string(),
                at: first.at,
                confidence,
                detail: d,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration as ChronoDuration};

    fn call(
        at: DateTime<Utc>,
        method: &str,
        url: &str,
        status: u16,
        stub: Option<&str>,
        ms: u64,
    ) -> MockInteraction {
        MockInteraction {
            at,
            method: method.into(),
            url: url.into(),
            status,
            matched_stub: stub.map(str::to_string),
            near_miss: stub.is_none().then_some(true),
            fault: None,
            duration_ms: ms,
            trace_id: None,
            space: None,
        }
    }

    fn seen(source: &str, items: Vec<MockInteraction>) -> MockInteractions {
        MockInteractions {
            source: source.into(),
            confidence: 0.95,
            items,
            attempt: 1,
            space: None,
            truncated: false,
        }
    }

    #[test]
    fn routes_ignore_ids_and_queries() {
        assert_eq!(route_of("/users/42?x=1"), "/users/{}");
        assert_eq!(
            route_of("/orders/0af76519-16cd-43dd-8448-eb211c80319c/items"),
            "/orders/{}/items"
        );
        assert_eq!(route_of("/a/0af7651916cd43dd"), "/a/{}");
        assert_eq!(route_of("/health"), "/health");
    }

    #[test]
    fn requests_become_signals_by_kind() {
        let t = Utc::now();
        let a = uuid::Uuid::now_v7();
        let b = uuid::Uuid::now_v7();
        let traced = seen(
            "traceId",
            vec![
                call(t, "GET", "/users/1", 200, Some("s-users"), 5),
                call(t, "POST", "/pay?token=secret", 502, Some("s-pay"), 12),
                call(t, "GET", "/users/7?verbose=1", 404, None, 1),
                call(t, "GET", "/nowhere", 404, None, 1),
                call(t, "GET", "/report", 200, Some("s-report"), 2500),
            ],
        );
        let window = seen("timeWindow", vec![call(t, "GET", "/users/2", 404, None, 1)]);
        let out = classify(&[(a, 1, traced), (b, 2, window)]);
        let kinds = |r: uuid::Uuid| -> Vec<(String, SignalConfidence)> {
            out.iter()
                .filter(|s| s.result_id == r)
                .map(|s| (s.kind.clone(), s.confidence))
                .collect()
        };
        assert_eq!(
            kinds(a),
            vec![
                ("slow".into(), SignalConfidence::Exact),
                ("stub_error".into(), SignalConfidence::Exact),
                ("unexpected_calls".into(), SignalConfidence::Exact),
                ("unmatched".into(), SignalConfidence::Exact),
            ]
        );
        assert_eq!(
            kinds(b),
            vec![("unmatched".into(), SignalConfidence::Window)]
        );
        let err = out.iter().find(|s| s.kind == "stub_error").unwrap();
        assert_eq!(err.detail["path"], "/pay", "the query string is never kept");
        assert_eq!(err.detail["status"], 502);
        assert_eq!(err.detail["stub"], "s-pay");
        assert_eq!(err.attempt, 1);
        let slow = out.iter().find(|s| s.kind == "slow").unwrap();
        assert_eq!(slow.detail["durationMs"], 2500);
    }

    #[test]
    fn slow_against_the_stubs_own_95th_percentile() {
        let t = Utc::now();
        let r = uuid::Uuid::now_v7();
        let mut items: Vec<MockInteraction> = (0..30)
            .map(|i| {
                call(
                    t + ChronoDuration::milliseconds(i),
                    "GET",
                    "/fast",
                    200,
                    Some("s-fast"),
                    10 + (i as u64 % 5),
                )
            })
            .collect();
        let few = seen("traceId", items.clone());
        assert!(classify(&[(r, 1, few)]).is_empty());
        items.push(call(t, "GET", "/fast", 200, Some("s-fast"), 400));
        let out = classify(&[(r, 1, seen("traceId", items))]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "slow");
        assert_eq!(out[0].detail["durationMs"], 400);
        let under = (0..10)
            .map(|i| call(t, "GET", "/x", 200, Some("s-x"), 10 + i))
            .chain(std::iter::once(call(t, "GET", "/x", 200, Some("s-x"), 900)))
            .collect();
        assert!(
            classify(&[(r, 1, seen("traceId", under))]).is_empty(),
            "fewer than {P95_MIN_CALLS} calls: only the 2 s rule"
        );
    }

    #[test]
    fn kept_entries_hold_only_what_the_classification_reads() {
        let mut c = call(
            Utc::now(),
            &"P".repeat(1000),
            &format!("/a/{}?token=secret", "é".repeat(5000)),
            200,
            Some(&"s".repeat(1000)),
            3,
        );
        c.fault = Some("f".repeat(1000));
        c.trace_id = Some("t".repeat(1000));
        c.space = Some("x".repeat(1000));
        let mut m = seen("traceId", vec![c]);
        compact(&mut m);
        let i = &m.items[0];
        assert!(i.url.len() <= KEPT_PATH_BYTES && i.url.starts_with("/a/é"));
        assert!(!i.url.contains("token"));
        assert!(i.method.len() <= KEPT_FIELD_BYTES);
        assert!(i.matched_stub.as_ref().unwrap().len() <= KEPT_FIELD_BYTES);
        assert!(i.fault.as_ref().unwrap().len() <= KEPT_FIELD_BYTES);
        assert!(i.trace_id.is_none() && i.space.is_none());
    }

    #[test]
    fn injected_faults_are_read_when_present() {
        let t = Utc::now();
        let mut c = call(t, "GET", "/pay", 503, Some("s-pay"), 3);
        c.fault = Some("http503".into());
        let out = classify(&[(uuid::Uuid::now_v7(), 1, seen("traceId", vec![c]))]);
        let kinds: Vec<&str> = out.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, vec!["injected_fault", "stub_error"]);
    }
}
