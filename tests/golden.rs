//! The output of a recorded capture, line by line, against `stvtf replay` of it as recorded in
//! `bots_cp_badlands.jsonl.gz`. Any change to the output shows up here: regenerate the file with
//! `stvtf replay tests/fixtures/bots_cp_badlands.cap | gzip -9` once the change is intended.

use serde_json::Value;
use std::io::{BufRead, BufReader};
use stvtf::Options;
use stvtf::capture;
use stvtf::decode::Tables;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

#[test]
fn replay_matches_the_recorded_output() {
    let bytes = std::fs::read(format!("{FIXTURES}/bots_cp_badlands.cap")).unwrap();
    let tables = Tables::from_demo_bytes(stvtf::builtin(stvtf::CURRENT_BUILD).unwrap().tables).unwrap();
    let mut out = Vec::new();
    // Through text, as the CLI writes it: serde_json parses floats to within an ulp, not exactly.
    let mut emit = |v: Value| out.push(serde_json::from_str::<Value>(&v.to_string()).unwrap());
    capture::replay(&capture::read(&bytes).unwrap(), &tables, &Options::default(), &mut emit).unwrap();

    let recorded = std::fs::File::open(format!("{FIXTURES}/bots_cp_badlands.jsonl.gz")).unwrap();
    let expected: Vec<Value> = BufReader::new(flate2::read::GzDecoder::new(recorded))
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect();
    for (n, (got, want)) in out.iter().zip(&expected).enumerate() {
        assert_eq!(got, want, "line {}", n + 1);
    }
    assert_eq!(out.len(), expected.len(), "number of lines");
}
