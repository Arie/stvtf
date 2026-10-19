//! The intelligence and the bball ball: where it is, who carries it, and its events.
//!
//! `ctf_ballin_sky.dem` is a 2v2 bball match (4.4 minutes, 2026-10-17, from demos.tf): three
//! ball spawns of which one is enabled at a time, pickups, drops, defences and captures.

use serde_json::Value;
use std::sync::LazyLock;

const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ctf_ballin_sky.dem");

static EVENTS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    let bytes = std::fs::read(DEMO).unwrap();
    let mut out = Vec::new();
    stvtf::decode::demo_file(&bytes, 20.0, &mut |v| out.push(v)).unwrap();
    out
});

fn flags(snapshot: &Value) -> Vec<&Value> {
    snapshot["objectives"].as_array().unwrap().iter().filter(|o| o["kind"] == "flag").collect()
}

#[test]
fn one_neutral_ball_at_a_time() {
    let snapshots: Vec<_> = EVENTS.iter().filter(|e| e["type"] == "snapshot" && e["round"] == "running").collect();
    assert!(snapshots.len() > 1000);
    for s in snapshots {
        let f = flags(s);
        assert!(f.len() <= 1, "{f:?}");
        if let Some(f) = f.first() {
            assert_eq!(f["team"], "none");
        }
    }
}

#[test]
fn a_carried_ball_is_where_its_carrier_is() {
    let mut carried = 0;
    for s in EVENTS.iter().filter(|e| e["type"] == "snapshot") {
        for f in flags(s).into_iter().filter(|f| f["status"] == "carried") {
            let carrier = s["players"].as_array().unwrap().iter().find(|p| p["id"] == f["carrier"]).expect("a carrier");
            assert_eq!(f["pos"], carrier["pos"]);
            carried += 1;
        }
    }
    assert!(carried > 1000, "{carried}");
}

#[test]
fn a_dropped_ball_counts_down_to_going_home() {
    let dropped: Vec<f64> = EVENTS
        .iter()
        .filter(|e| e["type"] == "snapshot")
        .flat_map(flags)
        .filter(|f| f["status"] == "dropped")
        .map(|f| f["returns_in"].as_f64().unwrap())
        .collect();
    assert!(!dropped.is_empty());
    // The map's return time is 60 s; the reset time is set a tick ahead of the snapshot's clock.
    assert!(dropped.iter().all(|&t| t > 0.0 && t <= 60.2), "{dropped:?}");
}

#[test]
fn flag_events_name_the_players() {
    let events: Vec<_> = EVENTS.iter().filter(|e| e["type"] == "flag").collect();
    let count = |kind: &str| events.iter().filter(|e| e["event"] == kind).count();
    assert!(count("pickup") > 20 && count("captured") > 10 && count("dropped") > 10 && count("defended") > 10);
    for e in &events {
        assert!(e["player"].is_u64(), "{e}");
    }
    // A defence stops a carrier: it names them.
    assert!(events.iter().filter(|e| e["event"] == "defended").all(|e| e["carrier"].is_u64()));
}
