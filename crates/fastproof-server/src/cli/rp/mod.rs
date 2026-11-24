//! `fastproof rp import`: copies the launches of an existing ReportPortal into FastProof. The
//! launches, items, logs and files are read from the ReportPortal REST API (never from its
//! database) and replayed through the `/rp` endpoint of FastProof with their original uuids
//! and times, so imported history and live reports of RP agents form the same series.

pub mod reader;
pub mod replay;
pub mod state;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use futures::StreamExt;

use self::reader::{RpConn, RpLaunch, RpReader};
use self::replay::{Attachments, Counts, Options, Writer};
use self::state::{Done, State};
use super::Conn;

#[derive(Subcommand, Debug)]
pub enum RpCommand {
    /// Copy launches, tests, logs and attachments from a ReportPortal into a FastProof
    /// project through its `/rp` endpoint. Safe to repeat: finished launches are skipped
    /// (state file) and replayed ones keep their uuids.
    Import(ImportArgs),
}

#[derive(Args, Debug)]
pub struct ImportArgs {
    /// FastProof server URL.
    #[arg(long, env = "FASTPROOF_URL", required_unless_present = "dry_run")]
    pub url: Option<String>,
    /// File holding the FastProof API token (otherwise FASTPROOF_TOKEN). It needs
    /// `launches:write` (and `results:triage` to carry defect triage over).
    #[arg(long, env = "FASTPROOF_TOKEN_FILE")]
    pub token_file: Option<PathBuf>,
    /// FastProof project key.
    #[arg(
        long,
        short,
        env = "FASTPROOF_PROJECT",
        required_unless_present = "dry_run"
    )]
    pub project: Option<String>,
    /// ReportPortal URL, e.g. https://rp.example.com
    #[arg(long)]
    pub rp_url: String,
    /// ReportPortal project (name, or key in 26.x).
    #[arg(long)]
    pub rp_project: String,
    /// File holding the ReportPortal API key (otherwise RP_API_KEY). Never pass it as an
    /// argument.
    #[arg(long)]
    pub rp_key_file: Option<PathBuf>,
    /// PEM file with the certificate authority of ReportPortal (added to the trusted ones).
    #[arg(long)]
    pub rp_ca_file: Option<PathBuf>,
    /// Allow a plain http ReportPortal URL (the API key is sent unencrypted).
    #[arg(long)]
    pub rp_allow_http: bool,
    /// Only launches that started within this time (`90d`, `12h`, `4w`), since this date
    /// (`2026-01-31` or RFC 3339), or `all`.
    #[arg(long, default_value = "90d")]
    pub since: String,
    /// Only launches with this exact name.
    #[arg(long)]
    pub launch_name: Option<String>,
    /// Also copy DEBUG launches.
    #[arg(long)]
    pub include_debug: bool,
    /// Log lines to copy: `none`, `error`, `warn` (that level and above) or `all`.
    #[arg(long, default_value = "warn")]
    pub logs: String,
    /// Attachments to copy: `none`, `failed` (of failed tests) or `all`.
    #[arg(long, default_value = "failed")]
    pub attachments: String,
    /// Largest attachment copied (`64MiB`); larger ones are skipped.
    #[arg(long, default_value = "64MiB")]
    pub max_attachment: String,
    /// Count launches, items, log bytes and attachments and write nothing to FastProof.
    #[arg(long)]
    pub dry_run: bool,
    /// The state file: replayed launches, to continue an interrupted import.
    #[arg(long, default_value = ".fastproof/rp-import.json")]
    pub state: PathBuf,
    /// ReportPortal requests per second, at most.
    #[arg(long, default_value_t = 10.0)]
    pub rate: f64,
    /// Launches replayed at the same time (1 to 4).
    #[arg(long, default_value_t = 2)]
    pub parallel: usize,
    /// Pause before the first retry of a failed request in milliseconds; it doubles.
    #[arg(long, default_value_t = 500, hide = true)]
    pub retry_delay_ms: u64,
}

pub async fn run(c: RpCommand) -> anyhow::Result<i32> {
    match c {
        RpCommand::Import(a) => import(a).await,
    }
}

/// `90d`, `12h`, `4w`, a date or `all`: the earliest start time taken.
pub fn parse_since(v: &str, now_ms: i64) -> anyhow::Result<Option<i64>> {
    let v = v.trim();
    if v.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    let unit = |u: char| match u {
        'h' => Some(3_600_000i64),
        'd' => Some(86_400_000),
        'w' => Some(7 * 86_400_000),
        _ => None,
    };
    if let Some(last) = v.chars().last()
        && let Some(ms) = unit(last)
        && let Ok(n) = v[..v.len() - 1].parse::<i64>()
        && (0..=36_500).contains(&n)
    {
        return Ok(Some(now_ms - n * ms));
    }
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(v) {
        return Ok(Some(t.timestamp_millis()));
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d") {
        return Ok(Some(
            d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis(),
        ));
    }
    bail!("--since: expected 90d, 12h, 4w, a date (2026-01-31), an RFC 3339 time or all")
}

/// `64MiB`, `512KiB`, `1GiB` or plain bytes.
pub fn parse_size(v: &str) -> anyhow::Result<u64> {
    let t = v.trim();
    let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    let n: u64 = t[..split]
        .parse()
        .with_context(|| format!("--max-attachment: bad size {v:?}"))?;
    let mult: u64 = match t[split..].trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        _ => bail!("--max-attachment: bad size {v:?} (use e.g. 64MiB)"),
    };
    n.checked_mul(mult)
        .filter(|b| *b > 0)
        .with_context(|| format!("--max-attachment: bad size {v:?}"))
}

fn read_secret(file: Option<&PathBuf>, var: &str) -> anyhow::Result<String> {
    Ok(match file {
        Some(f) => {
            std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?
        }
        None => std::env::var(var).unwrap_or_default(),
    })
}

fn fmt_bytes(n: u64) -> String {
    const U: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 3 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

async fn import(a: ImportArgs) -> anyhow::Result<i32> {
    if !(1..=4).contains(&a.parallel) {
        bail!("--parallel must be 1 to 4");
    }
    if !(0.1..=1000.0).contains(&a.rate) {
        bail!("--rate must be 0.1 to 1000 requests per second");
    }
    let now = fastproof_rp::time::now_ms();
    let since = parse_since(&a.since, now)?;
    let opts = Options {
        min_level: replay::min_level(&a.logs)?,
        attachments: match a.attachments.as_str() {
            "none" => Attachments::None,
            "failed" => Attachments::Failed,
            "all" => Attachments::All,
            _ => bail!("--attachments must be none, failed or all"),
        },
        max_attachment: parse_size(&a.max_attachment)?,
    };
    let retry_delay = Duration::from_millis(a.retry_delay_ms.max(1));
    let reader = RpReader::new(&RpConn {
        url: a.rp_url.clone(),
        project: a.rp_project.clone(),
        key: read_secret(a.rp_key_file.as_ref(), "RP_API_KEY")?,
        ca_file: a.rp_ca_file.clone(),
        allow_http: a.rp_allow_http,
        rate: a.rate,
        retry_delay,
    })?;
    let version = reader.version().await.context("ReportPortal /api/info")?;
    eprintln!("ReportPortal {version}, project {}", a.rp_project);
    let types = reader.defect_types().await;

    let (writer, state) = if a.dry_run {
        (None, None)
    } else {
        let conn = Conn {
            url: a.url.clone().unwrap_or_default(),
            token_file: a.token_file.clone(),
            project: a.project.clone().unwrap_or_default(),
        };
        let w = Writer::new(conn.client()?, &conn.project, retry_delay);
        let st = State::open(
            &a.state,
            a.rp_url.trim_end_matches('/'),
            &a.rp_project,
            &conn.project,
        )?;
        (Some(w), Some(tokio::sync::Mutex::new(st)))
    };

    let mut total = Counts::default();
    let (mut imported, mut already, mut running, mut failed) = (0u64, 0u64, 0u64, 0u64);
    for debug in [false, true] {
        if debug && !a.include_debug {
            break;
        }
        let mut page = 1;
        loop {
            let Some(batch) = reader
                .launches_page(page, debug, since, a.launch_name.as_deref())
                .await?
            else {
                break;
            };
            page += 1;
            let mut todo: Vec<RpLaunch> = Vec::new();
            for l in batch {
                if since.is_some_and(|s| l.start_ms < s)
                    || a.launch_name.as_deref().is_some_and(|n| n != l.name)
                {
                    continue;
                }
                let mut l = l;
                l.debug = l.debug || debug;
                if !l.finished() {
                    running += 1;
                    continue;
                }
                if let Some(st) = &state
                    && st.lock().await.is_done(&l.uuid)
                {
                    already += 1;
                    continue;
                }
                todo.push(l);
            }
            let mut results = futures::stream::iter(todo)
                .map(|l| {
                    let (reader, writer, types, opts) = (&reader, &writer, &types, &opts);
                    async move {
                        let r = match writer {
                            Some(w) => replay::replay_launch(reader, w, types, &l, opts).await,
                            None => replay::survey_launch(reader, &l, opts).await,
                        };
                        (l, r)
                    }
                })
                .buffer_unordered(a.parallel);
            while let Some((l, r)) = results.next().await {
                match r {
                    Ok(c) => {
                        let verb = if a.dry_run {
                            "would import"
                        } else {
                            "imported"
                        };
                        eprintln!(
                            "{verb} {} #{} ({}): {} items, {} log lines, {} files",
                            l.name, l.number, l.uuid, c.items, c.logs, c.files
                        );
                        for w in &c.warnings {
                            eprintln!("warning: {w}");
                        }
                        if let Some(st) = &state {
                            st.lock().await.mark_done(
                                &l.uuid,
                                Done {
                                    name: l.name.clone(),
                                    items: c.items,
                                    logs: c.logs,
                                    files: c.files,
                                    at: chrono::Utc::now().to_rfc3339(),
                                },
                            )?;
                        }
                        imported += 1;
                        total.add(&c);
                    }
                    Err(e) => {
                        failed += 1;
                        eprintln!("error: launch {} ({}): {e:#}", l.name, l.uuid);
                    }
                }
            }
        }
    }

    let verb = if a.dry_run { "to import" } else { "imported" };
    println!(
        "launches {verb}: {imported}, already done: {already}, still running in ReportPortal (skipped): {running}, failed: {failed}"
    );
    println!(
        "items: {}, log lines: {} ({}), files: {}{}, files not copied: {}",
        total.items,
        total.logs,
        fmt_bytes(total.log_bytes),
        total.files,
        if a.dry_run {
            " (sizes known only on download)".to_string()
        } else {
            format!(" ({})", fmt_bytes(total.file_bytes))
        },
        total.skipped_files
    );
    if failed > 0 {
        eprintln!("{failed} launches failed; run the command again to continue with them");
    }
    Ok(if failed > 0 { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_and_sizes() {
        let now = 10 * 86_400_000;
        assert_eq!(
            parse_since("90d", 100 * 86_400_000).unwrap(),
            Some(10 * 86_400_000)
        );
        assert_eq!(parse_since("12h", now).unwrap(), Some(now - 12 * 3_600_000));
        assert_eq!(parse_since("1w", now).unwrap(), Some(3 * 86_400_000));
        assert_eq!(parse_since("all", now).unwrap(), None);
        assert_eq!(parse_since("1970-01-02", now).unwrap(), Some(86_400_000));
        assert_eq!(
            parse_since("1970-01-01T00:00:01Z", now).unwrap(),
            Some(1000)
        );
        assert!(parse_since("yesterday", now).is_err());
        assert!(parse_since("-5d", now).is_err());
        assert_eq!(parse_size("64MiB").unwrap(), 64 << 20);
        assert_eq!(parse_size("512 kb").unwrap(), 512 << 10);
        assert_eq!(parse_size("100").unwrap(), 100);
        assert!(parse_size("0").is_err());
        assert!(parse_size("5 parsecs").is_err());
        assert!(parse_size("").is_err());
    }
}
