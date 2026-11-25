//! `fastproof bench`: N launches × M results × L log lines with realistic failures (about 10 %
//! failing with a handful of recurring errors, retries that turn flaky, steps, small
//! attachments), sent through the HTTP API or straight to Kafka, then waits until every
//! launch is queryable and prints throughput and end-to-end lag.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use fastproof_store::launches::LaunchStart;
use fastproof_store::logs::{LogLevel, LogLine, LogStream};
use futures::StreamExt;
use reqwest::Method;
use serde_json::{Value, json};
use uuid::Uuid;

use super::Conn;
use super::client::Client;

/// `fastproof bench [tm]`: the ingest bench, or a scenario named by a subcommand.
#[derive(Args, Debug, Clone)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct BenchCli {
    #[command(subcommand)]
    pub command: Option<BenchCommand>,
    #[command(flatten)]
    pub conn: BenchConn,
    #[command(flatten)]
    pub args: BenchArgs,
}

/// Scenarios of `fastproof bench`.
#[derive(Subcommand, Debug, Clone)]
pub enum BenchCommand {
    /// Load test management (cases, versions, code sync, reads, a plan launch run by virtual
    /// testers, export and a TestRail CSV import) and print p50/p95/max per operation against
    /// the lab targets. Exits 1 when requests failed (not on missed targets).
    Tm(super::bench_tm::TmBenchArgs),
    /// Load FastProof Analysis (synthetic launches, triage decisions, a measured launch) and
    /// print the suggestion step time and the read and accept latencies against the targets.
    /// Exits 1 when requests failed (not on missed targets).
    Analysis(super::bench_analysis::AnalysisBenchArgs),
}

/// Server connection of the ingest bench (required there, checked at run time so that
/// `bench tm` does not need it).
#[derive(Args, Debug, Clone, Default)]
pub struct BenchConn {
    /// Server URL.
    #[arg(long, env = "FASTPROOF_URL")]
    pub url: Option<String>,
    /// File holding the API token (otherwise FASTPROOF_TOKEN).
    #[arg(long, env = "FASTPROOF_TOKEN_FILE")]
    pub token_file: Option<std::path::PathBuf>,
    /// Project key.
    #[arg(long, short, env = "FASTPROOF_PROJECT")]
    pub project: Option<String>,
}

impl BenchConn {
    /// The connection, when URL and project are given.
    pub fn conn(self) -> anyhow::Result<Conn> {
        let (Some(url), Some(project)) = (self.url, self.project) else {
            bail!("fastproof bench needs --url (FASTPROOF_URL) and --project (FASTPROOF_PROJECT)");
        };
        Ok(Conn {
            url,
            token_file: self.token_file,
            project,
        })
    }
}

/// Options of `fastproof bench`.
#[derive(Args, Debug, Clone)]
pub struct BenchArgs {
    /// Set from `BenchConn` before `run`.
    #[arg(skip)]
    pub conn: Conn,
    /// Launches.
    #[arg(long, default_value_t = 10)]
    pub launches: usize,
    /// Results (tests) per launch.
    #[arg(long, default_value_t = 1000)]
    pub results: usize,
    /// Log lines per result.
    #[arg(long, default_value_t = 20)]
    pub log_lines: usize,
    /// Launches sent at the same time.
    #[arg(long, default_value_t = 4)]
    pub parallel: usize,
    /// Percent of results with a small attachment.
    #[arg(long, default_value_t = 2)]
    pub attachments_percent: u32,
    /// Create the project if it does not exist (needs projects:write).
    #[arg(long)]
    pub create_project: bool,
    /// Produce results and logs straight to Kafka (FASTPROOF_KAFKA_* settings; the server must
    /// run with FASTPROOF_KAFKA_TRUSTED_PRODUCERS=true).
    #[arg(long)]
    pub kafka: bool,
    /// Seconds to wait until the data is queryable.
    #[arg(long, default_value_t = 900)]
    pub wait_timeout: u64,
}

/// What a run measured.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchReport {
    pub launches: usize,
    pub results_sent: usize,
    pub log_lines_sent: usize,
    pub send_seconds: f64,
    pub results_per_second: f64,
    pub log_lines_per_second: f64,
    /// From the last request to every launch being queryable.
    pub lag_seconds: f64,
    pub total_seconds: f64,
}

const ERRORS: [&str; 5] = [
    "AssertionError: expected status 200 but was {n}",
    "TimeoutError: locator.click: Timeout {n}ms exceeded waiting for #checkout",
    "java.net.ConnectException: Connection refused (db-{n}.example.com:5432)",
    "NullPointerException at com.example.cart.CartService.total(CartService.java:{n})",
    "ElementNotInteractableException: element <button id=pay-{n}> is not clickable",
];

const OWNERS: [&str; 4] = ["qa", "alice", "bob", "carol"];
const SEVERITIES: [&str; 5] = ["blocker", "critical", "normal", "minor", "trivial"];

fn rnd(seed: u64) -> u64 {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 31;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^ (x >> 29)
}

struct Plan {
    results: Vec<Value>,
    logs: Vec<LogLine>,
}

fn plan(launch: usize, m: usize, l: usize, attachments: &[(usize, String)], now: i64) -> Plan {
    let mut results = Vec::with_capacity(m + m / 10);
    let mut logs = Vec::with_capacity(m * l);
    for j in 0..m {
        let r = rnd((launch * 1_000_003 + j) as u64);
        let failing = r.is_multiple_of(10);
        let flaky = failing && (r >> 8).is_multiple_of(3);
        let pattern = ERRORS[((r >> 16) % ERRORS.len() as u64) as usize]
            .replace("{n}", &((r >> 24) % 900 + 100).to_string());
        let full = format!("com.example.suite{}.Feature{}Test#case{j}", j % 20, j % 7);
        let steps = json!([
            {"name": "open the page", "status": "passed", "start": now, "stop": now + 20},
            {"name": "fill the form", "status": "passed", "start": now + 20, "stop": now + 60,
             "steps": [{"name": "type the e-mail", "status": "passed"}]},
            {"name": "submit", "status": if failing && !flaky { "failed" } else { "passed" }, "start": now + 60, "stop": now + 90}
        ]);
        let attempts = if flaky { 2 } else { 1 };
        for a in 0..attempts {
            let last = a + 1 == attempts;
            let fails = failing && !(flaky && last);
            let id = format!("t{j}-{a}");
            let mut res = json!({"id": id, "name": format!("case {j}"), "fullName": full,
                "status": if fails { "failed" } else { "passed" }, "start": now + a as i64 * 100,
                "stop": now + a as i64 * 100 + 90, "retry": a, "steps": steps,
                "labels": [{"name": "suite", "value": format!("suite{}", j % 20)}, {"name": "owner", "value": OWNERS[j % OWNERS.len()]},
                    {"name": "severity", "value": SEVERITIES[(r >> 40) as usize % SEVERITIES.len()]},
                    {"name": "epic", "value": format!("Epic {}", j % 4)}, {"name": "feature", "value": format!("Feature {}", j % 7)},
                    {"name": "story", "value": format!("Story {}", j % 30)}, {"name": "tag", "value": if j % 3 == 0 { "smoke" } else { "regression" }}]});
            if fails {
                res["error"] = json!({"message": pattern, "trace": format!("{pattern}\n\tat com.example.Test.case{j}(Test.java:42)")});
            }
            if last && let Some((_, att)) = attachments.iter().find(|(k, _)| *k == j) {
                res["attachments"] = json!([{"id": att, "name": "log.txt"}]);
            }
            results.push(res);
            for s in 0..l {
                let level = if fails && s + 1 == l {
                    LogLevel::Error
                } else if s % 10 == 9 {
                    LogLevel::Debug
                } else {
                    LogLevel::Info
                };
                logs.push(LogLine {
                    result_id: id.clone(),
                    attempt: a as u32,
                    seq: s as u64,
                    ts: now + s as i64,
                    level,
                    stream: if s % 5 == 0 {
                        LogStream::Step
                    } else {
                        LogStream::Log
                    },
                    message: if level == LogLevel::Error {
                        pattern.clone()
                    } else {
                        format!(
                            "step {s}: request GET /api/items/{} answered 200 in {} ms",
                            (r >> 4) % 1000,
                            s * 3 + 1
                        )
                    },
                    step: None,
                });
            }
        }
    }
    Plan { results, logs }
}

fn ndjson<T: serde::Serialize>(items: &[T]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    for i in items {
        serde_json::to_writer(&mut out, i)?;
        out.push(b'\n');
    }
    Ok(out)
}

async fn post(client: &Client, path: &str, body: Vec<u8>) -> anyhow::Result<Value> {
    let mut wait = Duration::from_millis(200);
    for attempt in 0..6 {
        let rb = client
            .request(Method::POST, path)?
            .header("content-type", "application/x-ndjson")
            .body(body.clone());
        match client.send::<Value>(rb).await {
            Ok(v) => return Ok(v),
            Err(e) if attempt < 5 => {
                tracing::debug!(error = %e, "bench request failed; retrying");
                tokio::time::sleep(wait).await;
                wait *= 2;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Runs the bench.
pub async fn run(a: BenchArgs) -> anyhow::Result<BenchReport> {
    if a.launches == 0 || a.results == 0 || a.parallel == 0 {
        bail!("--launches, --results and --parallel must be positive");
    }
    let client = a.conn.client()?;
    let project = a.conn.project.clone();
    if a.create_project {
        let rb = client.request(Method::GET, &format!("api/v1/projects/{project}"))?;
        if client.send::<Value>(rb).await.is_err() {
            let rb = client
                .request(Method::POST, "api/v1/projects")?
                .json(&json!({"key": project, "name": project}));
            client
                .send::<Value>(rb)
                .await
                .context("creating the project")?;
        }
    }
    let kafka = if a.kafka {
        let env = |k: &str| std::env::var(k).ok();
        let cfg = fastproof_stream::KafkaConfig::from_vars(&fastproof_stream::Vars(&env))?
            .context("--kafka needs FASTPROOF_KAFKA_BROKERS")?;
        Some(Arc::new(
            fastproof_stream::Kafka::connect(&cfg, None).await?,
        ))
    } else {
        None
    };
    let run_id = Uuid::now_v7().simple().to_string()[..12].to_string();
    let now = chrono::Utc::now().timestamp_millis();
    let started = Instant::now();
    let jobs = (0..a.launches).map(|i| {
        let client = client.clone();
        let project = project.clone();
        let kafka = kafka.clone();
        let run_id = run_id.clone();
        let a = a.clone();
        async move {
            let launch = client
                .start_launch(
                    &project,
                    &LaunchStart {
                        external_id: Some(format!("bench-{run_id}-{i}")),
                        name: format!("bench {run_id} #{i}"),
                        attributes: None,
                        ci: None,
                        vcs: None,
                        environment: None,
                        debug: None,
                    },
                )
                .await?;
            let mut atts = Vec::new();
            for j in 0..a.results {
                if (rnd((i * 7919 + j) as u64) % 100) < a.attachments_percent as u64 {
                    let rb = client
                        .request(
                            Method::POST,
                            &format!(
                                "api/v1/projects/{project}/launches/{}/attachments",
                                launch.id
                            ),
                        )?
                        .header("content-type", "text/plain")
                        .header("x-fastproof-attachment-name", "log.txt")
                        .body(format!("attachment of case {j} in launch {i}\n").repeat(20));
                    let v: Value = client.send(rb).await?;
                    if let Some(id) = v["id"].as_str() {
                        atts.push((j, id.to_string()));
                    }
                }
            }
            let p = plan(i, a.results, a.log_lines, &atts, now);
            let base = format!("api/v1/projects/{project}/launches/{}", launch.id);
            let n_results = p.results.len();
            let n_logs = p.logs.len();
            match &kafka {
                None => {
                    for c in p.results.chunks(500) {
                        post(&client, &format!("{base}/results"), ndjson(c)?).await?;
                    }
                    for c in p.logs.chunks(5000) {
                        post(&client, &format!("{base}/logs"), ndjson(c)?).await?;
                    }
                }
                Some(k) => {
                    use fastproof_stream::kafka::{Outgoing, TOPIC_LOGS, TOPIC_RESULTS};
                    use fastproof_stream::messages::{
                        LogRecord, ResultRecord, VERSION, log_key, result_key,
                    };
                    let mut recs = Vec::with_capacity(n_results);
                    for r in p.results {
                        // The test name stands in for the series key (one per test here).
                        let key = result_key(launch.id, r["fullName"].as_str().unwrap_or_default());
                        let rec = ResultRecord {
                            v: VERSION,
                            project: project.clone(),
                            launch_id: launch.id,
                            result: Some(serde_json::from_value(r)?),
                            parsed: None,
                            client_key: None,
                            upload_id: None,
                        };
                        recs.push(Outgoing {
                            key,
                            value: serde_json::to_vec(&rec)?,
                        });
                    }
                    k.produce(&k.topic(TOPIC_RESULTS), recs).await?;
                    let mut recs = Vec::with_capacity(n_logs);
                    for l in p.logs {
                        recs.push(Outgoing {
                            key: log_key(launch.id, &l.result_id),
                            value: serde_json::to_vec(&LogRecord::new(&project, launch.id, l))?,
                        });
                    }
                    for c in recs.chunks(20_000) {
                        k.produce(&k.topic(TOPIC_LOGS), c.to_vec()).await?;
                    }
                }
            }
            client
                .finish_launch(&project, launch.id, "finished")
                .await?;
            Ok::<_, anyhow::Error>((launch.id, n_results, n_logs))
        }
    });
    let sent: Vec<anyhow::Result<(Uuid, usize, usize)>> = futures::stream::iter(jobs)
        .buffer_unordered(a.parallel)
        .collect()
        .await;
    let mut launches = Vec::new();
    let (mut results_sent, mut logs_sent) = (0, 0);
    for r in sent {
        let (id, n, l) = r?;
        launches.push(id);
        results_sent += n;
        logs_sent += l;
    }
    let send_seconds = started.elapsed().as_secs_f64();
    let sent_at = Instant::now();
    let deadline = sent_at + Duration::from_secs(a.wait_timeout);
    for id in &launches {
        loop {
            let l = client.launch(&project, *id).await?;
            let logs_ok = if a.log_lines == 0 {
                true
            } else {
                let rb = client.request(
                    Method::GET,
                    &format!(
                        "api/v1/projects/{project}/launches/{id}/logs?resultId=t0-0&limit=5000"
                    ),
                )?;
                let v: Value = client.send(rb).await?;
                v["items"].as_array().map_or(0, |a| a.len()) >= a.log_lines
            };
            if l.counts.total as usize >= a.results && logs_ok {
                break;
            }
            if Instant::now() > deadline {
                bail!(
                    "launch {id} is not complete after {}s ({} of {} results)",
                    a.wait_timeout,
                    l.counts.total,
                    a.results
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let lag = sent_at.elapsed().as_secs_f64();
    Ok(BenchReport {
        launches: launches.len(),
        results_sent,
        log_lines_sent: logs_sent,
        send_seconds,
        results_per_second: results_sent as f64 / send_seconds.max(1e-9),
        log_lines_per_second: logs_sent as f64 / send_seconds.max(1e-9),
        lag_seconds: lag,
        total_seconds: started.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_is_realistic() {
        let p = plan(0, 1000, 3, &[(5, "a".into())], 0);
        let failed = p.results.iter().filter(|r| r["status"] == "failed").count();
        let retries = p.results.len() - 1000;
        assert!((50..=150).contains(&failed), "{failed}");
        assert!(retries > 0 && retries < failed);
        assert_eq!(p.logs.len(), p.results.len() * 3);
        let distinct: std::collections::HashSet<&str> = p
            .results
            .iter()
            .filter_map(|r| r["error"]["message"].as_str())
            .map(|m| m.split(':').next().unwrap_or(m))
            .collect();
        assert!(distinct.len() <= ERRORS.len());
        assert!(p.results.iter().any(|r| r["attachments"].is_array()));
    }
}
