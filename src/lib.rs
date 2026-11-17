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
pub mod output;
pub mod relay;
pub mod watch;

mod error;
mod net;
mod session;
mod sync;

pub use self::error::{Error, ErrorKind, Result};
pub use self::output::Line;
pub use self::session::{DEFAULT_SNAPSHOT_HZ, DEFAULT_TICK_INTERVAL, Fatal, Options, parse_snapshot_hz};

#[cfg(fuzzing)]
pub mod fuzzing;

#[cfg(test)]
mod hostile;
#[cfg(test)]
mod recovery;
#[cfg(test)]
mod testing;
