//! Heals, blast jumps and pickups, from the same 9v9 payload demo as `tests/payload.rs`.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pl_upward.dem");

static EVENTS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    let bytes = std::fs::read(DEMO).unwrap();
    let mut out = Vec::new();
    stvtf::decode::demo_file(&bytes, 20.0, &mut |v| out.push(v)).unwrap();
    out
});

const TURBINE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ctf_turbine.dem");

static CTF: LazyLock<Vec<Value>> = LazyLock::new(|| {
    let bytes = std::fs::read(TURBINE).unwrap();
    let mut out = Vec::new();
    stvtf::decode::demo_file(&bytes, 0.0, &mut |v| out.push(v)).unwrap();
    out
});

fn ctf_of_type(ty: &str) -> Vec<&'static Value> {
    CTF.iter().filter(|e| e["type"] == ty).collect()
}

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

#[test]
fn votes_pick_from_the_options_sent() {
    let options = ctf_of_type("vote_options");
    assert!(!options.is_empty());
    let n = options.iter().map(|o| o["options"].as_array().unwrap().len()).max().unwrap() as u64;
    let casts = ctf_of_type("vote_cast");
    assert!(!casts.is_empty());
    assert!(casts.iter().all(|c| c["option"].as_u64().unwrap() < n && c["team"].is_string()), "{casts:?}");
}

#[test]
fn medics_get_defended() {
    let d = ctf_of_type("medic_defended");
    assert!(!d.is_empty());
    assert!(d.iter().all(|e| e["player"].is_u64() && e["medic"].is_u64()));
}

#[test]
fn match_flow_lines() {
    let t = ctf_of_type("tournament");
    assert!(!t.is_empty());
    assert!(t.iter().any(|e| e["ready"].is_boolean()));
    let a = ctf_of_type("announcer");
    assert!(!a.is_empty());
    assert!(a.iter().all(|e| !e["sound"].as_str().unwrap().is_empty() && e["team"].is_string()), "{a:?}");
    assert!(ctf_of_type("server_text").iter().all(|e| e["text"].is_string()));
}

#[test]
fn broken_captures_name_the_point() {
    let b = of_type("capture_broken");
    assert!(!b.is_empty());
    assert!(b.iter().all(|e| e["point"].is_u64() && e["name"].is_string()));
}

#[test]
fn points_count_players_standing_on_them() {
    let on_point = |s: &&Value| {
        s["objectives"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["kind"] == "control_point" && o["on_point"]["blue"].as_u64().unwrap_or(0) > 0)
    };
    let snaps = of_type("snapshot");
    assert!(snaps.iter().filter(|s| s["round"] == "running").any(on_point));
    assert!(snaps.iter().all(|s| {
        s["objectives"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| o["kind"] == "control_point")
            .all(|o| o["on_point"]["red"].is_u64())
    }));
}

#[test]
fn killed_cappers_match_the_kill_at_that_tick() {
    let k = of_type("killed_capper");
    assert!(!k.is_empty());
    let kills = of_type("kill");
    for e in k {
        assert!(e["killer"].is_u64() && e["victim"].is_u64(), "{e}");
        assert!(
            kills.iter().any(|d| d["tick"] == e["tick"] && d["attacker"] == e["killer"] && d["victim"] == e["victim"]),
            "no kill line for {e}"
        );
    }
}

#[test]
fn soldiers_hold_weapons_with_clips() {
    let live: Vec<&Value> = of_type("snapshot").into_iter().filter(|s| s["round"] == "running").collect();
    let alive: Vec<&Value> =
        live.iter().flat_map(|s| s["players"].as_array().unwrap()).filter(|p| p["alive"] == true).collect();
    let armed = alive.iter().filter(|p| p["weapon"].is_object()).count();
    assert!(armed * 10 > alive.len() * 9, "{armed} of {} armed", alive.len());
    let has_clip = |class: &str, ok: &dyn Fn(&Value) -> bool| {
        alive.iter().any(|p| p["class"] == class && p["weapon"]["clip"].as_i64().is_some_and(|c| ok(&Value::from(c))))
    };
    assert!(has_clip("soldier", &|c| (0..=4).contains(&c.as_i64().unwrap())));
    // m_iClip1 is networked +1 (so -1 fits unsigned): full clips are 4 (Original) and 8 (stickies).
    let clips = |class: &str, item: i64| -> Vec<i64> {
        alive
            .iter()
            .filter(|p| p["class"] == class && p["weapon"]["item"] == item)
            .filter_map(|p| p["weapon"]["clip"].as_i64())
            .collect()
    };
    let rockets = clips("soldier", 513);
    assert!(!rockets.is_empty() && rockets.iter().all(|&c| c <= 4) && rockets.contains(&4), "{rockets:?}");
    let stickies = clips("demoman", 207);
    assert!(!stickies.is_empty() && stickies.iter().all(|&c| c <= 8) && stickies.contains(&8), "{stickies:?}");
    assert!(alive.iter().any(|p| p["class"] == "medic" && p["weapon"].is_object() && p["weapon"]["clip"].is_null()));
}

fn misread_sentries(events: &[Value]) -> usize {
    // Only a real sentry has `mini`; a dispenser or teleporter reported as a sentry has none.
    let snapshots = events.iter().filter(|e| e["type"] == "snapshot");
    snapshots
        .flat_map(|s| s["buildings"].as_array().unwrap().iter())
        .filter(|b| b["kind"] == "sentry" && b["mini"].is_null())
        .count()
}

fn kinds(events: &[Value]) -> std::collections::HashSet<String> {
    let snapshots = events.iter().filter(|e| e["type"] == "snapshot");
    snapshots
        .flat_map(|s| s["buildings"].as_array().unwrap().iter())
        .filter_map(|b| b["kind"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn buildings_are_recognised_by_what_they_are() {
    assert_eq!(misread_sentries(&EVENTS), 0);
    assert_eq!(misread_sentries(&CTF), 0);
    let seen: std::collections::HashSet<String> = kinds(&EVENTS).union(&kinds(&CTF)).cloned().collect();
    assert!(seen.contains("sentry") && seen.contains("dispenser"), "{seen:?}");
}

fn teamless_projectiles(events: &[Value]) -> usize {
    events
        .iter()
        .filter(|e| e["type"] == "snapshot")
        .flat_map(|s| s["projectiles"].as_array().into_iter().flatten())
        .filter(|p| p["team"] == "none")
        .count()
}

/// An explosion used to drop the nearest projectile within 150 units, often a live sticky beside
/// the blast: it vanished, and came back without team or owner when it next moved.
#[test]
fn projectiles_keep_their_team_through_nearby_explosions() {
    assert_eq!(teamless_projectiles(&EVENTS), 0);
    assert_eq!(teamless_projectiles(&CTF), 0);
}
