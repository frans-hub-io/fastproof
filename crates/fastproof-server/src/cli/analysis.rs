//! `fastproof analysis backtest`: the precision of triage suggestions on a history of people's
//! decisions, from exactly one source: a project on a server (`--project`, the server runs it
//! and the CLI waits), an export of decision events (`--events`, offline, no server) or
//! synthetic data (`--synthetic s|m|drift`, offline). The offline paths run the same harness
//! as the server, so an export gives the server's report.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use chrono::Utc;
use clap::{Args, Subcommand};
use fastproof_analysis::backtest::{Backtest, BacktestOptions, read_events};
use fastproof_analysis::score::RankOptions;
use fastproof_analysis::synth::{SynthProfile, generate};
use fastproof_store::analysis::backtest::report_of;
use fastproof_store::analysis::types::{
    BacktestReport, BacktestSource, DecisionEvent, SuggestionBand,
};
use reqwest::Method;
use serde_json::{Value, json};
use uuid::Uuid;

use super::client::Client;

/// `fastproof analysis …`.
#[derive(Subcommand, Debug)]
pub enum AnalysisCommand {
    /// Measure triage suggestions on history: precision per band and coverage. Exactly one of
    /// `--project`, `--events` and `--synthetic`. Exits 0 unless `--min-precision` is given
    /// and not met (1).
    Backtest(BacktestArgs),
}

/// Options of `fastproof analysis backtest`.
#[derive(Args, Debug, Clone)]
pub struct BacktestArgs {
    /// A project on the server (`FASTPROOF_URL`, `FASTPROOF_TOKEN` with `settings:write`):
    /// the server replays its stored decisions and the CLI waits for the report.
    #[arg(long, conflicts_with_all = ["events", "synthetic"])]
    pub project: Option<String>,
    /// Days of history with `--project` (1 to 730, default 180).
    #[arg(long, requires = "project")]
    pub days: Option<u32>,
    /// An NDJSON export of decision events (`GET .../analysis/decision-events`), offline.
    #[arg(long, conflicts_with = "synthetic")]
    pub events: Option<PathBuf>,
    /// A synthetic profile, offline: `s`, `m` or `drift`.
    #[arg(long)]
    pub synthetic: Option<String>,
    /// Seed of the synthetic data (default the profile's).
    #[arg(long, requires = "synthetic")]
    pub seed: Option<u64>,
    /// Offline: the project's `analysis.suggest.minScore` (0.40 to 0.95).
    #[arg(long, default_value_t = 0.4)]
    pub min_score: f64,
    /// Offline: the project's `analysis.suggest.maxCandidates` (1 to 3).
    #[arg(long, default_value_t = 3)]
    pub max_candidates: usize,
    /// Offline: decisions replayed, at most (later ones mark the report truncated).
    #[arg(long, default_value_t = 200_000)]
    pub max_events: u64,
    /// Print the report as JSON.
    #[arg(long)]
    pub json: bool,
    /// Fail (exit 1) when a band is below its minimum, e.g. `same=0.97,high=0.85`.
    #[arg(long)]
    pub min_precision: Option<String>,
    /// Server URL (with `--project`).
    #[arg(long, env = "FASTPROOF_URL")]
    pub url: Option<String>,
    /// File holding the API token (otherwise FASTPROOF_TOKEN; with `--project`).
    #[arg(long, env = "FASTPROOF_TOKEN_FILE")]
    pub token_file: Option<PathBuf>,
    /// How long to wait for the server's report with `--project`, in seconds.
    #[arg(long, default_value_t = 600)]
    pub timeout: u64,
}

/// Bytes of one line of an events file, at most.
pub const MAX_EVENT_LINE: usize = 256 * 1024;

pub async fn run(c: AnalysisCommand) -> anyhow::Result<i32> {
    match c {
        AnalysisCommand::Backtest(a) => backtest(a).await,
    }
}

/// Parses `same=0.97,high=0.85`.
pub fn parse_min_precision(s: &str) -> anyhow::Result<Vec<(SuggestionBand, f64)>> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let Some((b, v)) = part.split_once('=') else {
            bail!("--min-precision: expected band=value, got {part:?}");
        };
        let band = SuggestionBand::parse(b.trim())
            .with_context(|| format!("--min-precision: unknown band {b:?} (same, high, low)"))?;
        let v: f64 = v
            .trim()
            .parse()
            .with_context(|| format!("--min-precision: {v:?} is not a number"))?;
        if !(0.0..=1.0).contains(&v) {
            bail!("--min-precision: {v} is not between 0 and 1");
        }
        out.push((band, v));
    }
    if out.is_empty() {
        bail!("--min-precision: no band given");
    }
    Ok(out)
}

/// The bands below their minimum (a band without suggestions fails a minimum).
pub fn missed(report: &BacktestReport, mins: &[(SuggestionBand, f64)]) -> Vec<String> {
    mins.iter()
        .filter_map(|(band, min)| {
            let got = report
                .bands
                .iter()
                .find(|b| b.band == *band)
                .and_then(|b| b.precision);
            match got {
                Some(p) if p + 1e-9 >= *min => None,
                Some(p) => Some(format!("{}: {p:.3} < {min:.3}", band.as_str())),
                None => Some(format!(
                    "{}: no suggestions (minimum {min:.3})",
                    band.as_str()
                )),
            }
        })
        .collect()
}

fn options(a: &BacktestArgs) -> anyhow::Result<BacktestOptions> {
    if !(0.4..=0.95).contains(&a.min_score) {
        bail!("--min-score must be 0.40..=0.95");
    }
    if !(1..=3).contains(&a.max_candidates) {
        bail!("--max-candidates must be 1..=3");
    }
    Ok(BacktestOptions {
        rank: RankOptions {
            min_score: a.min_score,
            max_candidates: a.max_candidates,
        },
        max_events: a.max_events.max(1),
    })
}

/// Replays events read from an export: sorted by time (stable, so equal times keep the
/// export's order, which is the server's `(decided_at, id)` order).
pub fn backtest_events(
    mut events: Vec<DecisionEvent>,
    opts: &BacktestOptions,
    source: BacktestSource,
) -> BacktestReport {
    events.sort_by_key(|e| e.at);
    let mut b = Backtest::new(opts);
    b.feed(&events);
    report_of(b.finish(), source, Uuid::nil(), "fastproof-cli", Utc::now())
}

/// Reads an events file (at most `max` events plus one, to mark truncation).
pub fn read_events_file(path: &std::path::Path, max: u64) -> anyhow::Result<Vec<DecisionEvent>> {
    let f = std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut out = Vec::new();
    read_events(std::io::BufReader::new(f), MAX_EVENT_LINE, |e| {
        if (out.len() as u64) <= max {
            out.push(e);
        }
        Ok(())
    })
    .with_context(|| format!("reading {}", path.display()))?;
    Ok(out)
}

async fn backtest(a: BacktestArgs) -> anyhow::Result<i32> {
    let mins = a
        .min_precision
        .as_deref()
        .map(parse_min_precision)
        .transpose()?;
    let sources = [
        a.project.is_some(),
        a.events.is_some(),
        a.synthetic.is_some(),
    ];
    if sources.iter().filter(|s| **s).count() != 1 {
        bail!("give exactly one of --project, --events and --synthetic");
    }
    let report = if let Some(p) = &a.project {
        remote(&a, p).await?
    } else if let Some(f) = &a.events {
        let opts = options(&a)?;
        let events = read_events_file(f, opts.max_events)?;
        backtest_events(events, &opts, BacktestSource::Events)
    } else {
        let opts = options(&a)?;
        let name = a.synthetic.as_deref().unwrap_or_default();
        let mut profile = SynthProfile::named(name)
            .with_context(|| format!("--synthetic: unknown profile {name:?} (s, m, drift)"))?;
        if let Some(seed) = a.seed {
            profile = profile.with_seed(seed);
        }
        let data = generate(&profile);
        backtest_events(data.decisions, &opts, BacktestSource::Synthetic)
    };
    if a.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }
    if let Some(m) = mins {
        let miss = missed(&report, &m);
        if !miss.is_empty() {
            eprintln!("precision below the minimum: {}", miss.join("; "));
            return Ok(1);
        }
    }
    Ok(0)
}

fn print_report(r: &BacktestReport) {
    let when = match (r.from, r.to) {
        (Some(f), Some(t)) => format!("{} to {}", f.format("%Y-%m-%d"), t.format("%Y-%m-%d")),
        _ => "no decisions".into(),
    };
    println!(
        "{} decisions ({when}), coverage {:.1}%, {} without a suggestion{}",
        r.decisions,
        r.coverage * 100.0,
        r.no_suggestion,
        if r.truncated { " (truncated)" } else { "" }
    );
    println!("band   suggested  correct  group  precision");
    for b in &r.bands {
        println!(
            "{:<6} {:>9} {:>8} {:>6}  {}",
            b.band.as_str(),
            b.suggested,
            b.correct,
            b.correct_group,
            match b.precision {
                Some(p) if b.shown => format!("{:.1}%", p * 100.0),
                Some(p) => format!("{:.1}% (fewer than 50 decisions)", p * 100.0),
                None => "-".into(),
            }
        );
    }
    for c in &r.confusions {
        println!(
            "  {} decided {} times where {} was suggested",
            c.expected, c.count, c.suggested
        );
    }
}

async fn remote(a: &BacktestArgs, project: &str) -> anyhow::Result<BacktestReport> {
    let token = match &a.token_file {
        Some(f) => {
            std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?
        }
        None => std::env::var("FASTPROOF_TOKEN").unwrap_or_default(),
    };
    let url = a
        .url
        .as_deref()
        .context("--project needs FASTPROOF_URL (or --url)")?;
    let c = Client::new(url, &token)?;
    let key = Client::enc(project);
    let mut body = json!({});
    if let Some(d) = a.days {
        body["days"] = json!(d);
    }
    let task: Value = c
        .send(
            c.request(
                Method::POST,
                &format!("api/v1/projects/{key}/analysis/backtest"),
            )?
            .json(&body),
        )
        .await?;
    let id = task["id"].as_str().context("the server answered no task")?;
    let deadline = Instant::now() + Duration::from_secs(a.timeout.max(1));
    loop {
        let t: Value = c
            .send(c.request(Method::GET, &format!("api/v1/projects/{key}/tasks/{id}"))?)
            .await?;
        match t["status"].as_str() {
            Some("done") => break,
            Some("failed") => bail!(
                "the backtest failed on the server: {}",
                t["error"].as_str().unwrap_or("unknown error")
            ),
            _ => {}
        }
        if Instant::now() >= deadline {
            bail!("the backtest did not finish within {} s", a.timeout);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    c.send(c.request(
        Method::GET,
        &format!("api/v1/projects/{key}/analysis/backtest/latest"),
    )?)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_precision_parses_and_checks() {
        let m = parse_min_precision("same=0.97, high=0.85").unwrap();
        assert_eq!(
            m,
            vec![(SuggestionBand::Same, 0.97), (SuggestionBand::High, 0.85)]
        );
        assert!(parse_min_precision("same").is_err());
        assert!(parse_min_precision("best=0.9").is_err());
        assert!(parse_min_precision("same=2").is_err());
        assert!(parse_min_precision("").is_err());
        let data = generate(&SynthProfile::named("s").unwrap());
        let r = backtest_events(
            data.decisions,
            &BacktestOptions::default(),
            BacktestSource::Synthetic,
        );
        assert!(missed(&r, &m).is_empty(), "{r:?}");
        let strict = parse_min_precision("same=1.0").unwrap();
        assert_eq!(missed(&r, &strict).len(), 1);
    }
}
