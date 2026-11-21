//! Command line: `serve`, `migrate`, `upload`, `watch`, `launch start|finish`, `testplan`,
//! `version`.

pub mod archive;
pub mod bench;
pub mod ci;
pub mod client;
pub mod gate;
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
    version,
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
    /// Print an Allure `testplan.json` of chosen tests (e.g. the failed ones of a launch), so
    /// Allure adapters run only those (`ALLURE_TESTPLAN_PATH=testplan.json`).
    Testplan(TestplanArgs),
    /// Generate launches, results and log lines and measure throughput and end-to-end lag.
    Bench(bench::BenchArgs),
    /// Print the version.
    Version,
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
}

#[derive(Subcommand, Debug)]
pub enum LaunchCommand {
    /// Start a launch (or join the one with the same external id) and print its id.
    Start(LaunchArgs),
    /// Finish a launch.
    Finish(FinishArgs),
}

/// Server connection.
#[derive(Args, Debug, Clone)]
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
}

impl LaunchArgs {
    /// The launch start request, with CI metadata.
    pub fn start(&self) -> anyhow::Result<LaunchStart> {
        let ci = ci::detect(&|k| std::env::var(k).ok());
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
                    if f.launch.launch.is_none() && ci::detect(&|k| std::env::var(k).ok()).is_none()
                    {
                        bail!("give --id, --launch (FASTPROOF_LAUNCH) or run in CI");
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
        Command::Upload(u) => upload(u).await,
        Command::Gate(g) => Ok(gate::run(g).await),
        Command::Testplan(t) => testplan(t).await,
        Command::Watch(w) => watch::run(w).await,
        Command::Bench(b) => {
            let r = bench::run(b).await?;
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
    let shared = start.external_id.is_some();
    let finish = u.finish || (!shared && !u.no_finish);
    let launch = client.start_launch(&project, &start).await?;
    eprintln!("launch #{} {} ({})", launch.number, launch.name, launch.id);
    let (body, file_name) = if meta.is_dir() {
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

async fn testplan(t: TestplanArgs) -> anyhow::Result<i32> {
    if t.from_launch.is_none() && t.test_cases.is_empty() {
        bail!("give --from-launch or --test-case");
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
