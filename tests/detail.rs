//! Heals, blast jumps and pickups, from the same 9v9 payload demo as `tests/payload.rs`.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pl_upward.dem");

static EVENTS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    let bytes = std::fs::read(DEMO).unwrap();
    let mut out = Vec::new();
    stvtf::decode::demo_file(&bytes, 0.0, &mut |v| out.push(v)).unwrap();
    out
});

fn of_type(ty: &str) -> Vec<&'static Value> {
    EVENTS.iter().filter(|e| e["type"] == ty).collect()
}

#[test]
fn heals_name_patient_and_amount() {
    let heals = of_type("heal");
    assert!(heals.len() > 100, "{} heals", heals.len());
    assert!(heals.iter().all(|h| h["patient"].is_u64() && h["amount"].as_i64().unwrap() > 0));
    assert!(heals.iter().any(|h| h["source"] == "medigun"));
    assert!(heals.iter().any(|h| h["source"] == "crossbow"));
}

#[test]
fn every_blast_jump_starts_before_it_lands() {
    let jumps = of_type("blast_jump");
    assert!(!jumps.is_empty());
    assert!(jumps.iter().any(|j| j["phase"] == "land"));
    let mut last: HashMap<(u64, String), String> = HashMap::new();
    for j in jumps {
        let key = (j["player"].as_u64().unwrap(), j["kind"].as_str().unwrap().to_string());
        let phase = j["phase"].as_str().unwrap().to_string();
        // A jump can restart before it lands (died, or jumped again), but a landing needs a start.
        if phase == "land" {
            assert_eq!(last.get(&key).map(String::as_str), Some("start"), "{j}");
        }
        last.insert(key, phase);
    }
}

#[test]
fn pickups_are_health_or_ammo() {
    let p = of_type("pickup");
    assert!(p.iter().all(|e| e["player"].is_u64()), "every pickup names its player");
    assert!(p.iter().filter(|e| e["item"] == "ammo").count() > 20);
    assert!(p.iter().filter(|e| e["item"] == "health").count() > 10);
}

#[test]
fn impacts_and_marks_have_positions() {
    let impacts = of_type("impact");
    let marks = of_type("mark");
    assert!(impacts.iter().filter(|e| e["kind"] == "blood").count() > 200);
    assert!(impacts.iter().any(|e| e["kind"] == "sparks"));
    assert!(impacts.iter().any(|e| e["kind"] == "ricochet"));
    assert!(marks.len() > 100, "{} marks", marks.len());
    let world = of_type("world")[0];
    let (min, max) = (&world["min"], &world["max"]);
    for e in impacts.iter().chain(&marks) {
        for i in 0..3 {
            let v = e["pos"][i].as_f64().unwrap_or_else(|| panic!("{e}"));
            assert!(v.is_finite() && v >= min[i].as_f64().unwrap() && v <= max[i].as_f64().unwrap(), "{e}");
        }
    }
}
