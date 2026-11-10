//! The output of a recorded capture, line by line, against `stvtf replay` of it as recorded in
//! `bots_cp_badlands.jsonl.gz`. Any change to the output shows up here: regenerate the file with
//! `stvtf replay tests/fixtures/bots_cp_badlands.cap | gzip -9` once the change is intended.

mod common;

use common::{fixture, tables};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use stvtf::capture;
use stvtf::output::Stamped;
use stvtf::{Line, Options};

#[test]
fn replay_matches_the_recorded_output() {
    let bytes = std::fs::read(fixture("bots_cp_badlands.cap")).unwrap();
    let mut out = Vec::new();
    // Through text, as the CLI writes it: serde_json parses floats to within an ulp, not exactly.
    let mut emit = |line: Line| out.push(reparse(&serde_json::to_string(&line).unwrap()));
    capture::replay(&capture::read(&bytes).unwrap(), &tables(), &Options::default(), &mut emit).unwrap();

    let recorded = std::fs::File::open(fixture("bots_cp_badlands.jsonl.gz")).unwrap();
    let expected: Vec<Value> =
        BufReader::new(flate2::read::GzDecoder::new(recorded)).lines().map(|l| reparse(&l.unwrap())).collect();
    for (n, (got, want)) in out.iter().zip(&expected).enumerate() {
        assert_eq!(got, want, "line {}", n + 1);
    }
    assert_eq!(out.len(), expected.len(), "number of lines");
}

fn reparse(json: &str) -> Value {
    serde_json::from_str(json).unwrap()
}

/// Saved output reads back into the same types and writes out the same.
#[test]
fn recorded_lines_read_back_into_lines() {
    let recorded = std::fs::File::open(fixture("bots_cp_badlands.jsonl.gz")).unwrap();
    for (n, l) in BufReader::new(flate2::read::GzDecoder::new(recorded)).lines().enumerate() {
        let l = l.unwrap();
        let stamped: Stamped = serde_json::from_str(&l).unwrap_or_else(|e| panic!("line {}: {e}", n + 1));
        assert_eq!(reparse(&serde_json::to_string(&stamped).unwrap()), reparse(&l), "line {}", n + 1);
    }
}
