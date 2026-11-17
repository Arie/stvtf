//! A headless `SourceTV` spectator for Team Fortress 2.
//!
//! [`watch::watch`] connects to a `SourceTV` port and passes every event and snapshot
//! (the JSON described in the README) to a callback, reconnecting when dropped.
//! [`capture`] replays recorded traffic and [`decode::demo_file`] runs a `.dem`
//! through the same extractor.

#![warn(missing_docs)]

pub mod builds;
pub mod capture;
pub mod decode;
pub mod demo;
#[cfg(fuzzing)]
pub mod fuzzing;
#[cfg(test)]
mod hostile;
mod net;
pub mod output;
#[cfg(test)]
mod recovery;
pub mod relay;
mod session;
pub mod watch;

pub use output::Line;
pub use session::DEFAULT_SNAPSHOT_HZ;
pub use session::{Fatal, Options, parse_snapshot_hz};

/// Seconds per server tick (TF2's 66.67 per second) until the server's own value arrives in
/// [`output::Server::tick_interval`].
pub const DEFAULT_TICK_INTERVAL: f32 = 0.015;

/// Locks `m`, whatever a thread that panicked with it left.
fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A file in `tests/fixtures`.
#[cfg(test)]
fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).join(name)
}
