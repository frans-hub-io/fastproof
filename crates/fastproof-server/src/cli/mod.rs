//! Command line: `serve`, `migrate`, `upload`, `watch`, `launch start|finish`, `testplan`, `rerun`,
//! `cases export|import`, `export`, `bench [tm]`, `version`.

pub mod archive;
pub mod bench;
pub mod bench_tm;
pub mod cases;
pub mod ci;
pub mod client;
pub mod direct;
pub mod export;
pub mod gate;
pub mod merge;
pub mod watch;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};
use fastproof_store::launches::LaunchStart;
use fastproof_store::{Attribute, Launch, LaunchEnvironment};
use franshub_observe::{LogFormat, TelemetryConfig};

use self::client::Client;

#[derive(Parser, Debug)]
#[command(
    name = "fastproof",
    version = crate::VERSION,
    about = "FastProof test reporting server and CLI"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run the server (configured by FASTPROOF_* environment variables).
    Serve,
    /// Apply database migrations and exit (needs only the database settings).
    Migrate,
    /// Upload a report file, an archive or a results directory into a launch.
    Upload(UploadArgs),
    /// Upload new result files from a directory while tests run.
    Watch(WatchArgs),
    /// Start or finish a launch.
    Launch {
        #[command(subcommand)]
        command: LaunchCommand,
    },
    /// Check the quality gate of a launch: exit 0 when it passed, 1 when it failed, 2 on
    /// errors or when it is still pending.
    Gate(gate::GateArgs),
    /// Print an Allure `testplan.json` of chosen tests (e.g. the failed ones of a launch), of a
    /// managed plan (`--plan`) or of a launch started from one (`--launch`), so Allure adapters
    /// run only those (`ALLURE_TESTPLAN_PATH=testplan.json`).
    Testplan(TestplanArgs),
    /// Run the failed (or chosen) tests of a launch again: make a test plan and trigger the
    /// CI configured in the project settings (`rerun`). Prints the external id the new launch
    /// uses.
    Rerun(RerunArgs),
    /// Export test cases (`export --format ndjson|csv [--out file]`) or import them
    /// (`import <file> --format <f> [--apply]`, a dry run by default).
    Cases {
        #[command(subcommand)]
        command: cases::CasesCommand,
    },
    /// Export a launch as a self-contained HTML report, an XLSX workbook or a PDF (printed by a
    /// local Chromium).
    Export(export::ExportArgs),
    /// Generate launches, results and log lines and measure throughput and end-to-end lag;
    /// `bench tm` loads test management.
    Bench(bench::BenchCli),
    /// Print the version.
    Version,
}

#[derive(Args, Debug)]
pub struct RerunArgs {
    #[command(flatten)]
    pub conn: Conn,
    /// The launch to rerun: its id or its external id.
    #[arg(long)]
    pub launch: String,
    /// Statuses taken from the launch, comma-separated.
    #[arg(long, default_value = "failed,broken", conflicts_with = "result")]
    pub status: String,
    /// A result of the launch to run again, repeatable (instead of `--status`).
    #[arg(long = "result")]
    pub result: Vec<uuid::Uuid>,
}

#[derive(Args, Debug)]
pub struct TestplanArgs {
    #[command(flatten)]
    pub conn: Conn,
    /// Launch id whose tests are taken.
    #[arg(long)]
    pub from_launch: Option<uuid::Uuid>,
    /// Statuses taken from the launch, comma-separated.
    #[arg(long, default_value = "failed,broken")]
    pub status: String,
    /// Test case id to add, repeatable.
    #[arg(long = "test-case")]
    pub test_cases: Vec<uuid::Uuid>,
    /// Name of the stored plan.
    #[arg(long)]
    pub name: Option<String>,
    /// A managed plan: print its testplan.json (its automated cases, computed now).
    #[arg(long, conflicts_with_all = ["from_launch", "test_cases", "launch", "name"])]
    pub plan: Option<uuid::Uuid>,
    /// External id of a launch started from a managed plan (as in `FASTPROOF_LAUNCH`): print
    /// the testplan.json of that plan.
    #[arg(long, conflicts_with_all = ["from_launch", "test_cases", "name"])]
    pub launch: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum LaunchCommand {
    /// Start a launch (or join the one with the same external id) and print its id.
    Start(LaunchArgs),
    /// Finish a launch.
    Finish(FinishArgs),
    /// Merge launches (CI shards, re-runs) into a new launch or into one of them; a test run
    /// in several becomes one result with more attempts.
    Merge(merge::MergeArgs),
    /// Undo a merge within its undo window.
    UndoMerge(merge::UndoMergeArgs),
}

/// Server connection.
#[derive(Args, Debug, Clone, Default)]
pub struct Conn {
    /// Server URL.
    #[arg(long, env = "FASTPROOF_URL")]
    pub url: String,
    /// File holding the API token (otherwise FASTPROOF_TOKEN).
    #[arg(long, env = "FASTPROOF_TOKEN_FILE")]
    pub token_file: Option<PathBuf>,
    /// Project key.
    #[arg(long, short, env = "FASTPROOF_PROJECT")]
    pub project: String,
}

impl Conn {
    pub fn client(&self) -> anyhow::Result<Client> {
        let token = match &self.token_file {
            Some(f) => {
                std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?
            }
            None => std::env::var("FASTPROOF_TOKEN").unwrap_or_default(),
        };
        Client::new(&self.url, &token)
    }
}

/// Which launch, and how to create it.
#[derive(Args, Debug, Clone)]
pub struct LaunchArgs {
    #[command(flatten)]
    pub conn: Conn,
    /// External id: every command with the same id joins the same launch (default: from CI).
    #[arg(long, env = "FASTPROOF_LAUNCH")]
    pub launch: Option<String>,
    /// Launch name (default: from CI, else the external id).
    #[arg(long, env = "FASTPROOF_LAUNCH_NAME")]
    pub name: Option<String>,
    /// Environment key (part of test identity, e.g. `staging` or `firefox`).
    #[arg(long, env = "FASTPROOF_ENVIRONMENT")]
    pub environment: Option<String>,
    /// Branch (default: from CI).
    #[arg(long)]
    pub branch: Option<String>,
    /// Attribute `key=value`, repeatable.
    #[arg(long = "attr", value_name = "KEY=VALUE")]
    pub attrs: Vec<String>,
    /// Mark the launch as a debug run (hidden from lists by default).
    #[arg(long)]
    pub debug: bool,
    /// Ignore the CI environment: no external id, name, CI or VCS metadata derived from the
    /// variables of GitLab CI, GitHub Actions or Jenkins.
    #[arg(long, env = "FASTPROOF_NO_CI", value_parser = clap::builder::BoolishValueParser::new())]
    pub no_ci: bool,
    /// Add the results to this existing launch (id or number) instead of starting one: a
    /// re-run joins the original launch, and a test run again becomes a new attempt of its
    /// result. A launch merged into another resolves to that one.
    #[arg(long, env = "FASTPROOF_RERUN_OF", conflicts_with = "launch")]
    pub rerun_of: Option<String>,
}

impl LaunchArgs {
    /// CI metadata from the environment, unless `--no-ci`.
    pub fn ci(&self) -> Option<ci::CiMeta> {
        if self.no_ci {
            return None;
        }
        ci::detect(&|k| std::env::var(k).ok())
    }

    /// The launch start request, with CI metadata.
    pub fn start(&self) -> anyhow::Result<LaunchStart> {
        let ci = self.ci();
        let external_id = self
            .launch
            .clone()
            .or_else(|| ci.as_ref().and_then(|c| c.external_id.clone()));
        let name = self
            .name
            .clone()
            .or_else(|| ci.as_ref().and_then(|c| c.name.clone()))
            .or_else(|| external_id.clone())
            .unwrap_or_else(|| {
                format!(
                    "{} {}",
                    self.conn.project,
                    chrono::Utc::now().format("%Y-%m-%d %H:%M")
                )
            });
        let mut attributes = Vec::new();
        for a in &self.attrs {
            let Some((k, v)) = a.split_once('=') else {
                bail!("--attr {a:?}: expected key=value");
            };
            attributes.push(Attribute {
                key: k.to_string(),
                value: v.to_string(),
            });
        }
        let mut vcs = ci.as_ref().map(|c| c.vcs.clone());
        if let Some(b) = &self.branch {
            vcs.get_or_insert_with(Default::default).branch = Some(b.clone());
        }
        Ok(LaunchStart {
            external_id,
            name,
            attributes: (!attributes.is_empty()).then_some(attributes),
            ci: ci.map(|c| c.ci),
            vcs,
            environment: self.environment.as_ref().map(|k| LaunchEnvironment {
                key: Some(k.clone()),
                values: Default::default(),
            }),
            debug: self.debug.then_some(true),
        })
    }

    pub async fn resolve(&self, client: &Client) -> anyhow::Result<Launch> {
        if let Some(r) = &self.rerun_of {
            return merge::find_launch(client, &self.conn.project, r).await;
        }
        client
            .start_launch(&self.conn.project, &self.start()?)
            .await
    }
}

#[derive(Args, Debug)]
pub struct FinishArgs {
    #[command(flatten)]
    pub launch: LaunchArgs,
    /// Launch id (instead of the external id).
    #[arg(long)]
    pub id: Option<uuid::Uuid>,
    /// `finished` or `interrupted`.
    #[arg(long, default_value = "finished")]
    pub status: String,
    /// How long to wait for queued and processing uploads of the launch before finishing, in
    /// seconds (the summary then counts them).
    #[arg(long, default_value_t = 600)]
    pub timeout: u64,
    /// Finish at once, without waiting for queued uploads.
    #[arg(long)]
    pub no_wait: bool,
}

#[derive(Args, Debug)]
pub struct UploadArgs {
    /// A report file, an archive (zip, tar, tar.gz) or a directory.
    pub path: PathBuf,
    #[command(flatten)]
    pub launch: LaunchArgs,
    /// `auto`, `allure`, `junit`, `ctrf` or `playwright`.
    #[arg(long, default_value = "auto")]
    pub format: String,
    /// Wait until the server has processed the upload and print a summary.
    #[arg(long)]
    pub wait: bool,
    /// How long `--wait` waits, in seconds.
    #[arg(long, default_value_t = 600)]
    pub timeout: u64,
    /// Finish the launch after the upload (implies --wait). This is the default for a launch
    /// without an external id (no --launch, FASTPROOF_LAUNCH or CI); launches with one are
    /// shared by several commands and stay open unless this is given.
    #[arg(long, conflicts_with = "no_finish")]
    pub finish: bool,
    /// Keep the launch open even when this command created it for itself.
    #[arg(long)]
    pub no_finish: bool,
    /// Attachment files of a directory go straight to the server's S3 store when it offers
    /// that (`auto`), or always inside the archive (`off`).
    #[arg(
        long,
        value_enum,
        env = "FASTPROOF_DIRECT_UPLOAD_MODE",
        default_value = "auto"
    )]
    pub direct_uploads: direct::Mode,
    /// Smallest file sent directly, in bytes.
    #[arg(
        long,
        env = "FASTPROOF_DIRECT_UPLOAD_MIN_BYTES",
        default_value_t = 1_048_576
    )]
    pub direct_min_bytes: u64,
}

#[derive(Args, Debug)]
pub struct WatchArgs {
    /// The results directory.
    pub dir: PathBuf,
    #[command(flatten)]
    pub launch: LaunchArgs,
    /// `auto`, `allure`, `junit`, `ctrf` or `playwright`.
    #[arg(long, default_value = "auto")]
    pub format: String,
    /// Poll interval in milliseconds.
    #[arg(long, default_value_t = 1000)]
    pub interval_ms: u64,
    /// Finish the launch when stopped (SIGINT/SIGTERM).
    #[arg(long)]
    pub finish: bool,
    /// Stop after this many seconds without new files (0: run until a signal).
    #[arg(long, default_value_t = 0)]
    pub idle_exit: u64,
}

fn log_format() -> LogFormat {
    std::env::var("FASTPROOF_LOG_FORMAT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(LogFormat::Json)
}

/// Runs the command; returns the process exit code.
pub async fn run(cli: Cli) -> anyhow::Result<i32> {
    fastproof_blob::install_crypto_provider();
    match cli.command {
        Command::Version => {
            println!("fastproof {}", crate::VERSION);
            Ok(0)
        }
        Command::Serve => {
            let cfg = crate::Config::from_env()?;
            let mut tc = TelemetryConfig::new("fastproof", cfg.log_format);
            tc.build_info = Some(("fastproof_build_info".into(), crate::VERSION.into()));
            tc.histogram_buckets = crate::integrations::quality::histogram_buckets();
            let telemetry = franshub_observe::init(tc)?;
            tracing::info!(config = ?cfg, "starting");
            let running = crate::start(cfg, Some(telemetry.prometheus.clone())).await?;
            franshub_http::shutdown_signal().await;
            tracing::info!("shutting down");
            running.shutdown().await;
            telemetry.shutdown();
            Ok(0)
        }
        Command::Migrate => {
            let telemetry =
                franshub_observe::init(TelemetryConfig::new("fastproof", log_format()))?;
            let env = |k: &str| std::env::var(k).ok();
            let db = crate::db::DbSettings::from_lookup(&env)?;
            let pool = db.connect_waiting(&env).await?;
            crate::db::migrate(&pool).await?;
            tracing::info!("migrations are up to date");
            telemetry.shutdown();
            Ok(0)
        }
        Command::Launch {
            command: LaunchCommand::Start(a),
        } => {
            let client = a.conn.client()?;
            let l = a.resolve(&client).await?;
            println!("{}", l.id);
            eprintln!("launch #{} {} ({})", l.number, l.name, l.status);
            Ok(0)
        }
        Command::Launch {
            command: LaunchCommand::Finish(f),
        } => {
            let client = f.launch.conn.client()?;
            if !matches!(f.status.as_str(), "finished" | "interrupted") {
                bail!("--status must be finished or interrupted");
            }
            let id = match f.id {
                Some(id) => id,
                None => {
                    if f.launch.launch.is_none()
                        && f.launch.rerun_of.is_none()
                        && f.launch.ci().is_none()
                    {
                        bail!("give --id, --launch (FASTPROOF_LAUNCH), --rerun-of or run in CI");
                    }
                    f.launch.resolve(&client).await?.id
                }
            };
            let project = &f.launch.conn.project;
            let mut pending = 0;
            if !f.no_wait {
                pending = client
                    .wait_launch_uploads(project, id, Duration::from_secs(f.timeout))
                    .await?;
            }
            let l = client.finish_launch(project, id, &f.status).await?;
            print_launch(&l);
            if pending > 0 {
                eprintln!(
                    "{pending} uploads were still queued or processing after {}s; the counts change when they are done",
                    f.timeout
                );
            } else if f.no_wait {
                let open = pending_uploads(&client.uploads(project, id).await?);
                if open > 0 {
                    eprintln!(
                        "{open} uploads are still queued or processing; the counts change when they are done"
                    );
                }
            }
            Ok(0)
        }
        Command::Launch {
            command: LaunchCommand::Merge(m),
        } => merge::merge(m).await,
        Command::Launch {
            command: LaunchCommand::UndoMerge(m),
        } => merge::undo(m).await,
        Command::Upload(u) => upload(u).await,
        Command::Gate(g) => Ok(gate::run(g).await),
        Command::Testplan(t) => testplan(t).await,
        Command::Rerun(r) => rerun(r).await,
        Command::Cases { command } => cases::run(command).await,
        Command::Export(e) => export::run(e).await,
        Command::Watch(w) => watch::run(w).await,
        Command::Bench(bench::BenchCli {
            command: Some(bench::BenchCommand::Tm(t)),
            ..
        }) => {
            let r = bench_tm::run(t).await?;
            bench_tm::print(&r)?;
            Ok(if r.errors > 0 { 1 } else { 0 })
        }
        Command::Bench(b) => {
            let mut args = b.args;
            args.conn = b.conn.conn()?;
            let r = bench::run(args).await?;
            println!(
                "launches {}, results {}, log lines {}: sent in {:.1}s ({:.0} results/s, {:.0} log lines/s); queryable {:.2}s after the last request; total {:.1}s",
                r.launches,
                r.results_sent,
                r.log_lines_sent,
                r.send_seconds,
                r.results_per_second,
                r.log_lines_per_second,
                r.lag_seconds,
                r.total_seconds
            );
            println!("{}", serde_json::to_string(&r)?);
            Ok(0)
        }
    }
}

/// Uploads not processed yet.
pub fn pending_uploads(uploads: &[fastproof_store::Upload]) -> usize {
    uploads
        .iter()
        .filter(|u| u.status == "queued" || u.status == "processing")
        .count()
}

pub fn print_launch(l: &Launch) {
    let c = &l.counts;
    println!(
        "launch #{} {} [{}]: {} results, {} passed, {} failed, {} broken, {} skipped, {} flaky",
        l.number, l.name, l.status, c.total, c.passed, c.failed, c.broken, c.skipped, c.flaky
    );
}

async fn upload(u: UploadArgs) -> anyhow::Result<i32> {
    let client = u.launch.conn.client()?;
    let project = u.launch.conn.project.clone();
    let meta = tokio::fs::metadata(&u.path)
        .await
        .with_context(|| format!("{} not found", u.path.display()))?;
    let start = u.launch.start()?;
    // A launch without an external id belongs to this command alone, so it is finished here;
    // one with an external id is shared (CI jobs, shards, `launch start`) and stays open.
    let shared = start.external_id.is_some() || u.launch.rerun_of.is_some();
    let finish = u.finish || (!shared && !u.no_finish);
    let launch = u.launch.resolve(&client).await?;
    eprintln!("launch #{} {} ({})", launch.number, launch.name, launch.id);
    let (body, file_name) = if meta.is_dir() && u.direct_uploads == direct::Mode::Auto {
        let dir = u.path.clone();
        let files =
            tokio::task::spawn_blocking(move || archive::files_under(&dir, archive::MAX_FILES))
                .await??;
        let min = u.direct_min_bytes;
        let list = files.clone();
        let cands = tokio::task::spawn_blocking(move || direct::candidates(&list, min)).await?;
        let d = direct::upload(&client, &project, launch.id, cands).await?;
        for w in &d.warnings {
            eprintln!("  warning: {w}");
        }
        if !d.stubs.is_empty() {
            eprintln!(
                "{} attachments ({} bytes) uploaded directly to the store{}",
                d.stubs.len(),
                d.bytes,
                if d.kept > 0 {
                    format!(", {} sent in the archive", d.kept)
                } else {
                    String::new()
                }
            );
        }
        let stubs = d
            .stubs
            .into_iter()
            .map(|(rel, id)| (rel, crate::direct_uploads::stub(id)))
            .collect();
        (archive::stream_files(files, stubs), None)
    } else if meta.is_dir() {
        (archive::stream_dir(u.path.clone()), None)
    } else {
        let f = tokio::fs::File::open(&u.path).await?;
        let name = u.path.file_name().map(|n| n.to_string_lossy().to_string());
        (
            reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(f)),
            name,
        )
    };
    let up = client
        .upload(&project, launch.id, &u.format, file_name.as_deref(), body)
        .await?;
    eprintln!("upload {} queued ({} bytes)", up.id, up.size_bytes);
    if !(u.wait || finish) {
        println!("{}", up.id);
        eprintln!(
            "launch #{} stays open; finish it with `fastproof launch finish` (or pass --finish)",
            launch.number
        );
        return Ok(0);
    }
    let done = client
        .wait_uploads(
            &project,
            launch.id,
            &[up.id],
            Duration::from_secs(u.timeout),
        )
        .await?;
    let mut code = 0;
    for d in &done {
        match &d.error {
            Some(e) => {
                code = 2;
                eprintln!(
                    "upload {} failed: {} {}{}",
                    d.id,
                    e.code,
                    e.message,
                    e.limit
                        .as_ref()
                        .map(|l| format!(" (limit {l})"))
                        .unwrap_or_default()
                );
            }
            None => eprintln!(
                "upload {} {}: {} results, {} attachments, {} ignored files",
                d.id, d.format, d.counts.results, d.counts.attachments, d.counts.ignored
            ),
        }
        for w in d.warnings.iter().take(20) {
            eprintln!("  warning: {w}");
        }
    }
    let l = if finish {
        client
            .finish_launch(&project, launch.id, "finished")
            .await?
    } else {
        client.launch(&project, launch.id).await?
    };
    print_launch(&l);
    if !finish {
        eprintln!(
            "launch #{} stays open; finish it with `fastproof launch finish` (or pass --finish)",
            launch.number
        );
    }
    Ok(code)
}

async fn rerun(r: RerunArgs) -> anyhow::Result<i32> {
    let client = r.conn.client()?;
    let id = match r.launch.parse::<uuid::Uuid>() {
        Ok(id) => id,
        Err(_) => {
            client
                .launch_by_external(&r.conn.project, &r.launch)
                .await?
                .ok_or_else(|| anyhow::anyhow!("no launch with external id {:?}", r.launch))?
                .id
        }
    };
    let body = if r.result.is_empty() {
        let statuses: Vec<String> = r
            .status
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        serde_json::json!({"mode": "failed", "statuses": statuses})
    } else {
        serde_json::json!({"mode": "selected", "resultIds": r.result})
    };
    let out = client.rerun(&r.conn.project, id, &body).await?;
    println!("{}", out["externalId"].as_str().unwrap_or_default());
    eprintln!(
        "rerun {}: {} tests sent to the CI ({})",
        out["id"].as_str().unwrap_or_default(),
        out["testCount"],
        out["provider"].as_str().unwrap_or_default()
    );
    Ok(0)
}

async fn testplan(t: TestplanArgs) -> anyhow::Result<i32> {
    if let Some(plan) = t.plan {
        let client = t.conn.client()?;
        let json = client.testplan_json(&t.conn.project, plan).await?;
        println!("{}", serde_json::to_string_pretty(&json)?);
        eprintln!(
            "test plan {plan}: {} tests",
            json["tests"].as_array().map(|a| a.len()).unwrap_or(0)
        );
        return Ok(0);
    }
    if let Some(ext) = &t.launch {
        let client = t.conn.client()?;
        let launch = client
            .launch_by_external(&t.conn.project, ext)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no launch with external id {ext:?}"))?;
        let json = client
            .launch_testplan_json(&t.conn.project, launch.id)
            .await?;
        println!("{}", serde_json::to_string_pretty(&json)?);
        eprintln!(
            "launch #{}: {} tests",
            launch.number,
            json["tests"].as_array().map(|a| a.len()).unwrap_or(0)
        );
        return Ok(0);
    }
    if t.from_launch.is_none() && t.test_cases.is_empty() {
        bail!("give --plan, --launch, --from-launch or --test-case");
    }
    let client = t.conn.client()?;
    let statuses: Vec<String> = t
        .status
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let name = t.name.clone().unwrap_or_else(|| match t.from_launch {
        Some(l) => format!("{} of launch {l}", statuses.join(",")),
        None => "selected tests".into(),
    });
    let plan = client
        .create_test_plan(
            &t.conn.project,
            &serde_json::json!({"name": name, "fromLaunchId": t.from_launch, "statuses": statuses,
                "testCaseIds": (!t.test_cases.is_empty()).then_some(&t.test_cases)}),
        )
        .await?;
    let json = client.testplan_json(&t.conn.project, plan).await?;
    println!("{}", serde_json::to_string_pretty(&json)?);
    eprintln!(
        "test plan {plan}: {} tests",
        json["tests"].as_array().map(|a| a.len()).unwrap_or(0)
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn no_ci_ignores_the_ci_environment() {
        let cli = Cli::try_parse_from([
            "fastproof",
            "launch",
            "start",
            "--url",
            "http://localhost:8080",
            "-p",
            "web",
            "--no-ci",
        ])
        .unwrap();
        let Command::Launch {
            command: LaunchCommand::Start(a),
        } = cli.command
        else {
            panic!("launch start expected");
        };
        assert!(a.no_ci);
        assert!(a.ci().is_none());
        let s = a.start().unwrap();
        assert_eq!(s.external_id, None);
        assert!(s.ci.is_none() && s.vcs.is_none());
        assert!(s.name.starts_with("web "), "{}", s.name);
    }

    #[test]
    fn bench_tm_parses_without_the_bench_connection() {
        let cli = Cli::try_parse_from([
            "fastproof",
            "bench",
            "tm",
            "--url",
            "http://localhost:8080",
            "--cases",
            "100",
            "--keep",
        ])
        .unwrap();
        let Command::Bench(bench::BenchCli {
            command: Some(bench::BenchCommand::Tm(t)),
            ..
        }) = cli.command
        else {
            panic!("bench tm expected");
        };
        assert_eq!((t.cases, t.keep, t.project), (100, true, None));
        let cli = Cli::try_parse_from([
            "fastproof",
            "bench",
            "--url",
            "http://localhost:8080",
            "-p",
            "web",
            "--launches",
            "3",
        ])
        .unwrap();
        let Command::Bench(b) = cli.command else {
            panic!("bench expected");
        };
        assert!(b.command.is_none());
        assert_eq!(b.args.launches, 3);
        assert_eq!(b.conn.conn().unwrap().project, "web");
        assert!(bench::BenchConn::default().conn().is_err());
    }
}
