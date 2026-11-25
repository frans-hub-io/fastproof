//! FastProof Analysis: the pure logic that the live analysis path and the backtest share
//! (scoring of triage suggestions, the error lifecycle, flaky-cause classifiers, stack frames
//! and trigram similarity). No database and no async runtime: everything here is a function of
//! its inputs, so the same events give the same answers on the server and offline.

pub mod backtest;
pub mod fisher;
pub mod frames;
pub mod hints;
pub mod lifecycle;
pub mod model;
pub mod score;
pub mod synth;
pub mod synth_flaky;
pub mod trigram;

pub use frames::{MEMORY_FRAMES, frames, jaccard};
pub use model::*;
pub use trigram::similarity;

/// Result of the fallible operations of this crate (the backtest readers of event files).
pub type Result<T> = anyhow::Result<T>;
