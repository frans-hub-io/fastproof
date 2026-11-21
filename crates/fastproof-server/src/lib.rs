//! FastProof server and CLI: one binary with the API, ingest workers, the scheduler and the
//! embedded UI, plus client commands for CI (`upload`, `watch`, `launch`).

pub mod api;
pub mod app;
pub mod cli;
pub mod config;
pub mod db;
pub mod ingest;
pub mod live;
pub mod ops;
pub mod scheduler;
pub mod schema;
pub mod state;
pub mod stream;
pub mod trace;
pub mod ui;

pub use app::{Running, start, start_with_pool};
pub use config::Config;

/// Version of this build.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
