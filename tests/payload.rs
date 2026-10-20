//! Payload: carts, rollback and the first capture, from a real highlander match.
//!
//! `pl_upward.dem` is the first 3.4 minutes of an STV demo of a 9v9 on `pl_upward_f12`
//! (serveme.tf, 2026-10-18, from demos.tf), cut with `cargo run --example cut_demo -- IN.dem OUT.dem 13600`:
//! setup, BLU pushing the cart, a rollback, and the first point captured.

use serde_json::Value;
use std::sync::LazyLock;

const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pl_upward.dem");

static EVENTS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    let bytes = std::fs::read(DEMO).unwrap();
    let mut out = Vec::new();
    stvtf::decode::demo_file(&bytes, 20.0, &mut |v| out.push(v)).unwrap();
    out
});

fn snapshots() -> impl Iterator<Item = &'static Value> {
    EVENTS.iter().filter(|e| e["type"] == "snapshot")
}

fn cart(snapshot: &Value) -> Option<&Value> {
    snapshot["objectives"].as_array()?.iter().find(|o| o["kind"] == "cart")
}

fn pos(v: &Value) -> [f64; 3] {
    [v[0].as_f64().unwrap(), v[1].as_f64().unwrap(), v[2].as_f64().unwrap()]
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

#[test]
fn one_blu_cart_in_every_running_snapshot() {
    let running: Vec<_> = snapshots().filter(|s| s["round"] == "running").collect();
    assert!(running.len() > 1000);
    for s in running {
        let carts = s["objectives"].as_array().unwrap().iter().filter(|o| o["kind"] == "cart").count();
        assert_eq!(carts, 1, "tick {}", s["tick"]);
        let c = cart(s).unwrap();
        assert_eq!(c["team"], "blue");
        let progress = c["progress"].as_f64().unwrap();
        assert!((0.0..=1.0).contains(&progress), "{c}");
        let speed = c["speed"].as_i64().unwrap();
        assert!((-1..=3).contains(&speed), "{c}");
    }
}

#[test]
fn cart_advances_along_the_track() {
    let carts: Vec<_> = snapshots().filter_map(cart).collect();
    let start = pos(&carts[0]["pos"]);
    let furthest = carts.iter().map(|c| c["progress"].as_f64().unwrap()).fold(0.0, f64::max);
    assert!(furthest > 0.2, "furthest progress {furthest}");
    let moved = carts.iter().map(|c| dist(pos(&c["pos"]), start)).fold(0.0, f64::max);
    assert!(moved > 2000.0, "cart moved at most {moved} units");
    // Pushing it means players on it: the speed level follows the number of cappers.
    assert!(carts.iter().any(|c| c["cappers"].as_i64().unwrap() >= 3 && c["speed"] == 3));
}

#[test]
fn cart_rolls_back_with_nobody_on_it() {
    let rolling: Vec<_> =
        snapshots().filter_map(cart).filter(|c| c["speed"].as_i64().unwrap() < 0 && c["cappers"] == 0).collect();
    assert!(rolling.len() > 20, "{} rollback snapshots", rolling.len());
    let first = pos(&rolling[0]["pos"]);
    assert!(rolling.iter().any(|c| dist(pos(&c["pos"]), first) > 20.0), "the cart moves while rolling back");
}

#[test]
fn first_point_is_captured_by_the_cart() {
    let caps: Vec<_> = EVENTS.iter().filter(|e| e["type"] == "point_captured").collect();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0]["point"], 0);
    assert_eq!(caps[0]["team"], "blue");
    assert!(!caps[0]["cappers"].as_array().unwrap().is_empty());
    let tick = caps[0]["tick"].as_u64().unwrap();
    let before = snapshots().filter(|s| s["tick"].as_u64().unwrap() <= tick).last().unwrap();
    let point = before["objectives"].as_array().unwrap().iter().find(|o| o["index"] == 0).unwrap();
    let gap = dist(pos(&cart(before).unwrap()["pos"]), pos(&point["pos"]));
    assert!(gap < 100.0, "cart {gap} units from the point it captures");
    let after = snapshots().last().unwrap();
    let point = after["objectives"].as_array().unwrap().iter().find(|o| o["index"] == 0).unwrap();
    assert_eq!(point["owner"], "blue");
}

#[test]
fn setup_timer_runs_out_before_the_push() {
    let tick_of = |t: &str| EVENTS.iter().find(|e| e["type"] == t).unwrap()["tick"].as_u64().unwrap();
    // The demo starts in the previous round; setup runs from round_active to setup_finished.
    let (active, setup_end) = (tick_of("round_active"), tick_of("setup_finished"));
    let during: Vec<_> = snapshots()
        .filter(|s| s["round"] == "running" && (active..setup_end).contains(&s["tick"].as_u64().unwrap()))
        .collect();
    assert!(during.len() > 1000, "{} setup snapshots", during.len());
    for s in during {
        assert!(s["timers"].as_array().unwrap().iter().any(|t| t["setup"] == true), "tick {}", s["tick"]);
        assert_eq!(cart(s).unwrap()["progress"], 0.0, "the cart cannot move during setup");
    }
}
