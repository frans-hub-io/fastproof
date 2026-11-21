//! Command line: `serve`, `migrate`, `upload`, `watch`, `launch start|finish`, `version`.

pub mod archive;
pub mod ci;
pub mod client;
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
    /// Print the version.
    Version,
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
    /// Finish the launch after the upload (implies --wait).
    #[arg(long)]
    pub finish: bool,
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
            let pool = db.connect(&env).await?;
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
            let l = client
                .finish_launch(&f.launch.conn.project, id, &f.status)
                .await?;
            print_launch(&l);
            Ok(0)
        }
        Command::Upload(u) => upload(u).await,
        Command::Watch(w) => watch::run(w).await,
    }
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
    let launch = u.launch.resolve(&client).await?;
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
    if !(u.wait || u.finish) {
        println!("{}", up.id);
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
    let l = if u.finish {
        client
            .finish_launch(&project, launch.id, "finished")
            .await?
    } else {
        client.launch(&project, launch.id).await?
    };
    print_launch(&l);
    Ok(code)
}
