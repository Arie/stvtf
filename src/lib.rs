//! A headless `SourceTV` spectator for Team Fortress 2.
//!
//! [`watch::watch`] connects to a `SourceTV` port and passes every event and snapshot
//! (the JSON described in the README) to a callback, reconnecting as needed.
//! [`capture`] replays recorded traffic and [`decode::demo_file`] runs a `.dem`
//! through the same extractor.

#![warn(missing_docs)]

pub mod capture;
pub mod decode;
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

/// The newest TF2 server build compiled in. A build we have no data for gets its tables.
pub const CURRENT_BUILD: &str = BUILDS[0].0;

/// Known TF2 server builds (`A2S_INFO` `version`) and the demo of their data tables, newest first.
/// Most updates keep the tables, so builds share them. `cargo run --example add_build` adds to it.
const BUILDS: &[(&str, &[u8])] = &[
    // add_build: builds
    ("11185272", TABLES_11138802),
];

// add_build: tables
const TABLES_11138802: &[u8] = include_bytes!("../assets/tables_11138802.dem");

/// Compiled-in data for one TF2 server build.
#[derive(Debug, Clone, Copy)]
pub struct Build {
    /// A demo of that build cut off after its data tables, which give its `SendTable` CRC too.
    pub tables: &'static [u8],
}

/// Compiled-in data for known TF2 server builds (`A2S_INFO` `version`).
pub fn builtin(build: &str) -> Option<Build> {
    BUILDS.iter().find(|b| b.0 == build).map(|b| Build { tables: b.1 })
}
