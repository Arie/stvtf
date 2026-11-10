//! Shared by the integration tests.

#![allow(dead_code, reason = "each test uses part of this")]

use std::collections::HashSet;
use std::io::Read;
use std::process::Command;
use std::time::{Duration, Instant};
use stvtf::decode::Tables;
use stvtf::output::{PlayerId, Position};
use stvtf::{Line, Options, capture};

pub const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
/// A second, in ticks: TF2 runs 66 a second, near enough.
pub const SECOND: u32 = 66;
/// A tick, in seconds.
pub const TICK: f64 = 0.015;
/// A bound on what should take moments.
pub const LIMIT: Duration = Duration::from_secs(30);

/// What starts a connectionless packet.
pub const OOB: [u8; 4] = [0xff; 4];
pub const A2S_INFO: &[u8] = b"\xff\xff\xff\xffTSource Engine Query\0";
pub const AUTH_HASHEDCDKEY: u32 = 2;

/// The payloads of one kind of line: `of!(lines, Kill)` iterates over every `Kill` in `lines`.
#[allow(unused_macros, reason = "each test uses part of this")]
macro_rules! of {
    ($lines:expr, $variant:ident) => {
        $lines.iter().filter_map(|l| match l {
            stvtf::Line::$variant(x) => Some(x),
            _ => None,
        })
    };
}
#[allow(unused_imports, reason = "each test uses part of this")]
pub(crate) use of;

pub fn fixture(name: &str) -> String {
    format!("{FIXTURES}/{name}")
}

/// The stvtf program, without the passwords and debug settings of whoever runs the tests.
pub fn stvtf() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_stvtf"));
    c.env_remove("STVTF_PASSWORD").env_remove("STVTF_RELAY_PASSWORD").env_remove("STVTF_DEBUG");
    c
}

/// A gzipped fixture's text.
pub fn gunzipped(name: &str) -> String {
    let mut text = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(fixture(name)).unwrap()).read_to_string(&mut text).unwrap();
    text
}

/// The current build's built-in tables.
pub fn tables() -> Tables {
    Tables::from_demo_bytes(stvtf::builtin(stvtf::CURRENT_BUILD).unwrap().tables).unwrap()
}

/// A demo file's output.
pub fn demo(name: &str, snapshot_hz: f32) -> Vec<Line> {
    let bytes = std::fs::read(fixture(name)).unwrap();
    let mut out = Vec::new();
    stvtf::decode::demo_file(&bytes, snapshot_hz, &mut |l| out.push(l)).unwrap();
    out
}

/// A capture's output.
pub fn replay(name: &str, opt: &Options) -> Vec<Line> {
    let bytes = std::fs::read(fixture(name)).unwrap();
    let mut out = Vec::new();
    capture::replay(&capture::read(&bytes).unwrap(), &tables(), opt, &mut |l| out.push(l)).unwrap();
    out
}

/// The players of every snapshot in `lines`.
pub fn players_seen(lines: &[Line]) -> HashSet<PlayerId> {
    of!(lines, Snapshot).flat_map(|s| s.players.iter().map(|p| p.id)).collect()
}

pub fn has_players(l: &Line) -> bool {
    matches!(l, Line::Snapshot(s) if !s.players.is_empty())
}

/// Waits until `done` says so, failing after `LIMIT`.
#[track_caller]
pub fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + LIMIT;
    while !done() {
        assert!(Instant::now() < until, "{what} did not happen");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A `SourceTV`'s answer to a challenge request with `nonce`.
pub fn challenge(nonce: &[u8]) -> Vec<u8> {
    const MAGIC_VERSION: u32 = 0x5A4F_4933;
    let mut v = OOB.to_vec();
    v.push(b'A');
    v.extend_from_slice(&MAGIC_VERSION.to_le_bytes());
    v.extend_from_slice(&7u32.to_le_bytes());
    v.extend_from_slice(nonce);
    v.extend_from_slice(&AUTH_HASHEDCDKEY.to_le_bytes());
    v
}

pub fn dist(a: Position, b: Position) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}
