//! `fastproof bench analysis`: loads FastProof Analysis through the public HTTP API only (a
//! server with `FASTPROOF_ANALYSIS=on`), so it runs from a laptop or a pod against any
//! installation. It uploads synthetic launches whose failures form `--clusters` distinct
//! errors, triages their clusters as a person would (the decisions the memory learns), uploads
//! one more launch of `--results` results, and measures the targets of the analysis contract:
//! the `suggestions` step of that launch (< 5 s), `GET …/suggestions` (< 100 ms p95) and
//! accepting suggestions.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::Args;
use fastproof_analysis::synth::{SynthError, distinct_errors};
use reqwest::Method;
use serde::Serialize;
use serde_json::{Value, json};

use super::client::Client;

/// Options of `fastproof bench analysis`.
#[derive(Args, Debug, Clone)]
pub struct AnalysisBenchArgs {
    /// Server URL.
    #[arg(long, env = "FASTPROOF_URL")]
    pub url: String,
    /// File holding the API token (otherwise FASTPROOF_TOKEN); an admin token (it creates a
    /// project).
    #[arg(long, env = "FASTPROOF_TOKEN_FILE")]
    pub token_file: Option<PathBuf>,
    /// Key of the project to create (default a random `bench-an-…`); it must not exist.
    #[arg(long)]
    pub project: Option<String>,
    /// Launches triaged before the measured one (the memory).
    #[arg(long, default_value_t = 3)]
    pub history: u32,
    /// Distinct errors (clusters) per launch.
    #[arg(long, default_value_t = 300)]
    pub clusters: usize,
    /// Results of the measured launch (failed ones are five per cluster, at most all).
    #[arg(long, default_value_t = 5000)]
    pub results: usize,
    /// Suggestions accepted on the measured launch (accepts share the per-caller write rate
    /// of 30 a minute).
    #[arg(long, default_value_t = 20)]
    pub accepts: usize,
    /// Seed of the synthetic errors.
    #[arg(long, default_value_t = 7)]
    pub seed: u64,
    /// Seconds after finishing the measured launch before the bench queues it itself (a
    /// recompute) when the outbox consumer has not yet.
    #[arg(long, default_value_t = 15)]
    pub queue_wait: u64,
    /// How long to wait for the server's analysis, in seconds.
    #[arg(long, default_value_t = 300)]
    pub timeout: u64,
    /// `s`: suggestions on PostgreSQL; `m`: also the analytics endpoints and the portfolio
    /// (with ClickHouse, contract §14: each < 1 s, the portfolio < 3 s).
    #[arg(long, default_value = "s", value_parser = ["s", "m"])]
    pub profile: String,
    /// Profile m: an existing, filled project whose analytics are measured (default: the
    /// bench's own project).
    #[arg(long)]
    pub analytics_project: Option<String>,
    /// Profile m: days the analytics windows cover.
    #[arg(long, default_value_t = 90)]
    pub analytics_days: u32,
    /// Profile m: requests per analytics endpoint.
    #[arg(long, default_value_t = 10)]
    pub analytics_repeat: usize,
    /// Profile m: measure only the analytics of `--analytics-project` (no upload, no
    /// suggestions).
    #[arg(long)]
    pub analytics_only: bool,
}

/// The analytics endpoints profile m measures (path under the project, query).
pub const ANALYTICS_ENDPOINTS: [(&str, &str); 9] = [
    ("failures", "groupBy=fingerprint"),
    ("failures", "groupBy=defectGroup"),
    ("failures", "groupBy=suite"),
    ("durations", ""),
    ("retries", ""),
    ("flaky-trend", ""),
    ("mttr", ""),
    ("slow-steps", ""),
    ("timeline", ""),
];

/// What profile m measured.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsBench {
    pub project: String,
    pub days: u32,
    /// `clickhouse` or `postgres` (as the answers said).
    pub backend: String,
    pub endpoints: BTreeMap<String, OpStats>,
    pub portfolio: OpStats,
    /// Every endpoint p95 < 1000 ms and the portfolio p95 < 3000 ms, on ClickHouse.
    pub targets_met: bool,
}

/// Latencies of one operation.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpStats {
    pub count: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

fn stats(mut v: Vec<f64>) -> OpStats {
    if v.is_empty() {
        return OpStats::default();
    }
    v.sort_by(f64::total_cmp);
    let at = |q: f64| v[((v.len() as f64 - 1.0) * q).round() as usize];
    OpStats {
        count: v.len(),
        p50_ms: at(0.5),
        p95_ms: at(0.95),
        max_ms: *v.last().unwrap_or(&0.0),
    }
}

/// What the bench measured.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisBenchReport {
    pub project: String,
    pub clusters: usize,
    pub results: usize,
    pub decisions: usize,
    /// Seconds from finishing the measured launch until its analysis was done (includes the
    /// server's settle time and queue).
    pub seconds_to_suggestions: f64,
    /// The outbox consumer had not queued the launch after `--queue-wait`; the bench queued
    /// it with a recompute.
    pub recomputed: bool,
    /// The `suggestions` step of the measured launch as the server timed it.
    pub suggestions_step_ms: u64,
    pub suggested_clusters: usize,
    pub bands: BTreeMap<String, usize>,
    pub get_suggestions: OpStats,
    pub accept: OpStats,
    pub triage: OpStats,
    /// `suggestions` step < 5000 ms and `GET …/suggestions` p95 < 100 ms.
    pub targets_met: bool,
    pub errors: usize,
    /// Profile m.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analytics: Option<AnalyticsBench>,
}

struct Ctx {
    c: Client,
    key: String,
    errors: usize,
}

impl Ctx {
    async fn call(&mut self, m: Method, path: &str, body: Option<Value>) -> Option<(f64, Value)> {
        let rb = match self.c.request(m, path) {
            Ok(r) => r,
            Err(_) => {
                self.errors += 1;
                return None;
            }
        };
        let rb = match body {
            Some(b) => rb.json(&b),
            None => rb,
        };
        let t = Instant::now();
        match self.c.send::<Value>(rb).await {
            Ok(v) => Some((t.elapsed().as_secs_f64() * 1000.0, v)),
            Err(e) => {
                self.errors += 1;
                eprintln!("request failed: {e:#}");
                None
            }
        }
    }

    fn p(&self, rest: &str) -> String {
        format!("api/v1/projects/{}/{rest}", self.key)
    }
}

fn result_of(i: usize, e: Option<&SynthError>) -> Value {
    let name = format!("test_{i:05}");
    match e {
        Some(e) => {
            json!({"id": name, "name": name, "fullName": format!("com.example.bench.Suite#{name}"),
            "status": "failed", "error": {"message": e.message, "trace": e.trace}})
        }
        None => {
            json!({"id": name, "name": name, "fullName": format!("com.example.bench.Suite#{name}"),
            "status": "passed"})
        }
    }
}

async fn upload_launch(
    x: &mut Ctx,
    errors: &[SynthError],
    results: usize,
    n: u32,
) -> anyhow::Result<String> {
    let (_, l) = x
        .call(
            Method::POST,
            &x.p("launches"),
            Some(json!({"name": format!("bench {n}"), "vcs": {"branch": "main"}})),
        )
        .await
        .context("creating a launch")?;
    let lid = l["id"].as_str().context("no launch id")?.to_string();
    let failed = (errors.len() * 5).min(results);
    let items: Vec<Value> = (0..results)
        .map(|i| result_of(i, (i < failed).then(|| &errors[i % errors.len()])))
        .collect();
    for chunk in items.chunks(500) {
        x.call(
            Method::POST,
            &x.p(&format!("launches/{lid}/results")),
            Some(Value::Array(chunk.to_vec())),
        )
        .await
        .context("uploading results")?;
    }
    Ok(lid)
}

async fn wait_ingested(
    x: &mut Ctx,
    lid: &str,
    want: usize,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some((_, l)) = x
            .call(Method::GET, &x.p(&format!("launches/{lid}")), None)
            .await
            && l["counts"]["total"].as_u64().unwrap_or(0) as usize >= want
        {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("the results of launch {lid} were not stored in time");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

async fn finish(x: &mut Ctx, lid: &str) -> anyhow::Result<()> {
    x.call(
        Method::POST,
        &x.p(&format!("launches/{lid}/finish")),
        Some(json!({})),
    )
    .await
    .context("finishing a launch")?;
    Ok(())
}

/// Waits for the analysis of a launch; when the outbox consumer has not queued it after
/// `queue_wait`, queues it with a recompute (and says so).
async fn wait_analysis(
    x: &mut Ctx,
    lid: &str,
    timeout: Duration,
    queue_wait: Duration,
) -> anyhow::Result<(Value, bool)> {
    let started = Instant::now();
    let deadline = started + timeout;
    let mut recomputed = false;
    loop {
        if let Some((_, a)) = x
            .call(Method::GET, &x.p(&format!("launches/{lid}/analysis")), None)
            .await
        {
            if matches!(a["state"].as_str(), Some("done" | "failed")) {
                return Ok((a, recomputed));
            }
            if a["state"] == "none" && !recomputed && started.elapsed() > queue_wait {
                recomputed = x
                    .call(
                        Method::POST,
                        &x.p(&format!("launches/{lid}/suggestions/recompute")),
                        Some(json!({})),
                    )
                    .await
                    .is_some();
            }
        }
        if Instant::now() > deadline {
            bail!(
                "the analysis of launch {lid} did not finish in time (is FASTPROOF_ANALYSIS on?)"
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Profile m: times every analytics endpoint of `project` over `days` days, and the portfolio.
async fn measure_analytics(
    x: &mut Ctx,
    project: &str,
    days: u32,
    repeat: usize,
) -> anyhow::Result<AnalyticsBench> {
    let to = chrono::Utc::now();
    let from = to - chrono::Duration::days(i64::from(days.clamp(1, 365)));
    let window = format!(
        "from={}&to={}",
        from.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        to.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );
    let base = format!("api/v1/projects/{project}");
    let (_, top) = x
        .call(
            Method::GET,
            &format!("{base}/analytics/failures?{window}&limit=1"),
            None,
        )
        .await
        .context("reading the analytics of the project")?;
    let backend = top["window"]["backend"].as_str().unwrap_or("?").to_string();
    let fingerprint = top["items"][0]["key"].as_str().map(str::to_string);
    let mut endpoints = BTreeMap::new();
    for (ep, q) in ANALYTICS_ENDPOINTS {
        let path = if ep == "timeline" {
            let Some(f) = &fingerprint else {
                continue;
            };
            format!("{base}/errors/{f}/timeline?{window}")
        } else {
            format!("{base}/analytics/{ep}?{window}&{q}")
        };
        let mut v = Vec::new();
        for _ in 0..repeat.max(1) {
            if let Some((ms, _)) = x.call(Method::GET, &path, None).await {
                v.push(ms);
            }
        }
        let name = if q.is_empty() {
            ep.to_string()
        } else {
            format!("{ep}?{q}")
        };
        endpoints.insert(name, stats(v));
    }
    let mut v = Vec::new();
    for _ in 0..repeat.max(1) {
        if let Some((ms, _)) = x
            .call(
                Method::GET,
                &format!("api/v1/analytics/portfolio?{window}"),
                None,
            )
            .await
        {
            v.push(ms);
        }
    }
    let portfolio = stats(v);
    let targets_met = backend == "clickhouse"
        && portfolio.count > 0
        && portfolio.p95_ms < 3000.0
        && endpoints.values().all(|s| s.count > 0 && s.p95_ms < 1000.0);
    Ok(AnalyticsBench {
        project: project.to_string(),
        days,
        backend,
        endpoints,
        portfolio,
        targets_met,
    })
}

pub async fn run(a: AnalysisBenchArgs) -> anyhow::Result<AnalysisBenchReport> {
    if a.analytics_only {
        let Some(project) = a.analytics_project.clone() else {
            bail!("--analytics-only needs --analytics-project");
        };
        let token = match &a.token_file {
            Some(f) => {
                std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?
            }
            None => std::env::var("FASTPROOF_TOKEN").unwrap_or_default(),
        };
        let mut x = Ctx {
            c: Client::new(&a.url, token.trim())?,
            key: project.clone(),
            errors: 0,
        };
        let m = measure_analytics(&mut x, &project, a.analytics_days, a.analytics_repeat).await?;
        return Ok(AnalysisBenchReport {
            project,
            errors: x.errors,
            analytics: Some(m),
            ..AnalysisBenchReport::default()
        });
    }
    if a.clusters == 0 || a.clusters > 5000 || a.results == 0 || a.results > 50_000 {
        bail!("--clusters must be 1..=5000 and --results 1..=50000");
    }
    let token = match &a.token_file {
        Some(f) => {
            std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?
        }
        None => std::env::var("FASTPROOF_TOKEN").unwrap_or_default(),
    };
    let c = Client::new(&a.url, &token)?;
    let key = a.project.clone().unwrap_or_else(|| {
        format!(
            "bench-an-{}",
            &uuid::Uuid::now_v7().simple().to_string()[24..]
        )
    });
    let mut x = Ctx {
        c,
        key: key.clone(),
        errors: 0,
    };
    x.call(
        Method::POST,
        "api/v1/projects",
        Some(json!({"key": key, "name": "Analysis bench"})),
    )
    .await
    .context("creating the project")?;
    let (_, types) = x
        .call(Method::GET, &x.p("defect-types"), None)
        .await
        .context("reading defect types")?;
    let type_of: BTreeMap<String, String> = types["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["builtIn"] == true)
        .filter_map(|t| {
            Some((
                t["group"].as_str()?.to_string(),
                t["id"].as_str()?.to_string(),
            ))
        })
        .collect();
    let errors = distinct_errors(a.seed, a.clusters);
    let timeout = Duration::from_secs(a.timeout.max(10));
    let mut triage = Vec::new();
    let failed = (errors.len() * 5).min(a.results);
    for h in 0..a.history {
        let lid = upload_launch(&mut x, &errors, a.results, h + 1).await?;
        wait_ingested(&mut x, &lid, a.results, timeout).await?;
        finish(&mut x, &lid).await?;
        for e in errors.iter().take(failed.min(errors.len())) {
            let Some(t) = type_of.get(&e.group) else {
                continue;
            };
            if let Some((ms, _)) = x
                .call(
                    Method::POST,
                    &x.p("triage"),
                    Some(json!({"launchId": lid, "fingerprint": e.fingerprint, "defectTypeId": t})),
                )
                .await
            {
                triage.push(ms);
            }
        }
    }
    let decisions = triage.len();
    // The memory folds people's decisions every 10 s; wait until the last one is in.
    if let Some(last) = errors.get(failed.min(errors.len()).saturating_sub(1)) {
        let deadline = Instant::now() + timeout;
        loop {
            let path = x.p(&format!("analysis/memory?fingerprint={}", last.fingerprint));
            if let Some((_, m)) = x.call(Method::GET, &path, None).await
                && m["items"][0]["confirmed"].as_u64().unwrap_or(0) >= a.history as u64
            {
                break;
            }
            if Instant::now() > deadline {
                bail!("the triage memory did not learn the decisions in time");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    let lid = upload_launch(&mut x, &errors, a.results, a.history + 1).await?;
    wait_ingested(&mut x, &lid, a.results, timeout).await?;
    let t0 = Instant::now();
    finish(&mut x, &lid).await?;
    let (status, recomputed) =
        wait_analysis(&mut x, &lid, timeout, Duration::from_secs(a.queue_wait)).await?;
    let seconds_to_suggestions = t0.elapsed().as_secs_f64();
    let step_ms = status["steps"]["suggestions"]["millis"]
        .as_u64()
        .unwrap_or(0);
    let mut gets = Vec::new();
    let mut view = Value::Null;
    for _ in 0..20 {
        if let Some((ms, v)) = x
            .call(
                Method::GET,
                &x.p(&format!("launches/{lid}/suggestions")),
                None,
            )
            .await
        {
            gets.push(ms);
            view = v;
        }
    }
    let mut bands: BTreeMap<String, usize> = BTreeMap::new();
    let mut open: Vec<String> = Vec::new();
    let mut suggested = 0;
    for cl in view["clusters"].as_array().into_iter().flatten() {
        if let Some(s) = cl["suggestions"].as_array().and_then(|s| s.first()) {
            suggested += 1;
            *bands
                .entry(s["band"].as_str().unwrap_or("?").to_string())
                .or_default() += 1;
            if s["state"] == "open"
                && let Some(id) = s["id"].as_str()
            {
                open.push(id.to_string());
            }
        }
    }
    let mut accepts = Vec::new();
    for id in open.iter().take(a.accepts) {
        if let Some((ms, _)) = x
            .call(
                Method::POST,
                &x.p(&format!("suggestions/{id}/accept")),
                Some(json!({})),
            )
            .await
        {
            accepts.push(ms);
        }
    }
    let get_suggestions = stats(gets);
    let targets_met = step_ms > 0 && step_ms < 5000 && get_suggestions.p95_ms < 100.0;
    let analytics = if a.profile == "m" {
        let project = a.analytics_project.clone().unwrap_or_else(|| key.clone());
        Some(measure_analytics(&mut x, &project, a.analytics_days, a.analytics_repeat).await?)
    } else {
        None
    };
    Ok(AnalysisBenchReport {
        project: key,
        clusters: a.clusters,
        results: a.results,
        decisions,
        seconds_to_suggestions,
        recomputed,
        suggestions_step_ms: step_ms,
        suggested_clusters: suggested,
        bands,
        get_suggestions,
        accept: stats(accepts),
        triage: stats(triage),
        targets_met,
        errors: x.errors,
        analytics,
    })
}

pub fn print(r: &AnalysisBenchReport) -> anyhow::Result<()> {
    if let Some(m) = &r.analytics {
        println!(
            "analytics of {} over {} days on {}: portfolio p95 {:.1} ms; targets {}",
            m.project,
            m.days,
            m.backend,
            m.portfolio.p95_ms,
            if m.targets_met { "met" } else { "missed" }
        );
        for (name, s) in &m.endpoints {
            println!("  {name}: p50 {:.1} ms, p95 {:.1} ms", s.p50_ms, s.p95_ms);
        }
        if r.clusters == 0 {
            println!("{}", serde_json::to_string(r)?);
            return Ok(());
        }
    }
    println!(
        "{} clusters, {} results, {} decisions learned; suggestions step {} ms ({:.1} s after finish, settle \
         included); {} clusters suggested {:?}",
        r.clusters,
        r.results,
        r.decisions,
        r.suggestions_step_ms,
        r.seconds_to_suggestions,
        r.suggested_clusters,
        r.bands
    );
    println!(
        "GET suggestions p50 {:.1} ms, p95 {:.1} ms; accept p50 {:.1} ms; targets {}",
        r.get_suggestions.p50_ms,
        r.get_suggestions.p95_ms,
        r.accept.p50_ms,
        if r.targets_met { "met" } else { "missed" }
    );
    println!("{}", serde_json::to_string(r)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_are_percentiles() {
        let s = stats((1..=100).map(|v| v as f64).collect());
        assert_eq!(s.count, 100);
        assert_eq!(s.p50_ms, 51.0);
        assert_eq!(s.p95_ms, 95.0);
        assert_eq!(s.max_ms, 100.0);
        assert_eq!(stats(vec![]).count, 0);
        let errors = distinct_errors(7, 10);
        let r = result_of(3, Some(&errors[0]));
        assert_eq!(r["status"], "failed");
        assert_eq!(result_of(4, None)["status"], "passed");
    }
}
