//! A headless SourceTV spectator for Team Fortress 2.
//!
//! [`watch::watch`] connects to a SourceTV port and passes every event and snapshot
//! (the JSON described in the README) to a callback, reconnecting as needed.
//! [`capture`] replays recorded traffic and [`decode::demo_file`] runs a `.dem`
//! through the same extractor.

#![warn(missing_docs)]

pub mod capture;
pub mod decode;
mod net;
mod session;
pub mod watch;

pub use session::{Fatal, Options};

/// The TF2 server build whose tables and CRC are compiled in.
pub const CURRENT_BUILD: &str = "11138802";

/// Compiled-in data for one TF2 server build.
#[derive(Debug, Clone, Copy)]
pub struct Build {
    /// `g_SendTableCRC` of the server's engine.
    pub crc: u32,
    /// A demo of that build cut off after its data tables.
    pub tables: &'static [u8],
}

/// Compiled-in data for known TF2 server builds (A2S_INFO `version`).
pub fn builtin(build: &str) -> Option<Build> {
    match build {
        CURRENT_BUILD => Some(Build { crc: 0x5FCD_4733, tables: include_bytes!("../assets/tables_11138802.dem") }),
        _ => None,
    }
}
