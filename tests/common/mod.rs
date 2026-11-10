//! Shared by the integration tests.

#![allow(dead_code, reason = "each test uses part of this")]

use std::collections::HashSet;
use stvtf::decode::Tables;
use stvtf::output::{PlayerId, Position};
use stvtf::{Line, Options, capture};

pub const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

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

pub fn dist(a: Position, b: Position) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}
