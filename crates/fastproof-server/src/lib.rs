//! FastProof server and CLI: one binary with the API, ingest workers, the scheduler and the
//! embedded UI, plus client commands for CI (`upload`, `watch`, `launch`).

pub mod api;
pub mod app;
pub mod cli;
pub mod config;
pub mod db;
pub mod ingest;
pub mod integrations;
pub mod limits;
pub mod live;
pub mod markdown;
pub mod mcp;
pub mod mcp_tm;
pub mod media_prepare;
pub mod ops;
pub mod scheduler;
pub mod schema;
pub mod state;
pub mod stream;
pub mod testops;
pub mod tm_jobs;
pub mod trace;
pub mod ui;

pub use app::{Running, start, start_with_pool};
pub use config::Config;

/// Version of this build: `FASTPROOF_BUILD_VERSION` at compile time (the release tag without
/// the `v`, set by the image build), else the Cargo package version.
pub const VERSION: &str = match option_env!("FASTPROOF_BUILD_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => env!("CARGO_PKG_VERSION"),
};
