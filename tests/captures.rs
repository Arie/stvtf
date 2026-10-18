//! Integration tests over real SourceTV traffic recorded from a serveme.tf test
//! reservation (TF2 build 11138802, cp_badlands, bots) with `connect --record`.
//! `bots_map_change.cap` is one connection that followed a `changelevel`: during the
//! tv_delay window the server restarted signon in place four times, then streamed on.
//! `bots_engineers.cap` has engineer bots building (sentries, teleporters) under attack.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use stvmap::capture;
use stvmap::decode::{self, Tables};
use stvmap::session::Options;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn tables() -> Tables {
    Tables::from_demo_bytes(stvmap::builtin(stvmap::CURRENT_BUILD).unwrap().tables).unwrap()
}

fn replay(name: &str) -> Vec<Value> {
    replay_with(name, &Options { snapshot_hz: 10.0, ..Options::default() })
}

fn replay_with(name: &str, opt: &Options) -> Vec<Value> {
    let bytes = std::fs::read(format!("{FIXTURES}/{name}")).unwrap();
    let records = capture::read(&bytes).unwrap();
    let mut out = Vec::new();
    capture::replay(&records, &tables(), opt, &mut |v| out.push(v)).unwrap();
    out
}

fn of_type<'a>(events: &'a [Value], t: &str) -> Vec<&'a Value> {
    events.iter().filter(|e| e["type"] == t).collect()
}

fn pos(v: &Value) -> [f64; 3] {
    [v[0].as_f64().unwrap(), v[1].as_f64().unwrap(), v[2].as_f64().unwrap()]
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn states(events: &[Value]) -> Vec<String> {
    of_type(events, "status").iter().map(|e| e["state"].as_str().unwrap().to_string()).collect()
}

#[test]
fn current_build_tables_are_built_in() {
    let t = tables();
    assert!(t.datatables.server_classes.len() > 300);
    assert!(t.datatables.server_classes.iter().any(|c| c.name.as_str() == "CTFPlayer"));
    let full = Tables::from_demo(format!("{FIXTURES}/tables_11138802.dem").as_ref()).unwrap();
    assert_eq!(t.datatables.server_classes.len(), full.datatables.server_classes.len());
    assert_eq!(t.datatables.tables.len(), full.datatables.tables.len());
}

#[test]
fn session_reaches_full_without_decode_errors() {
    let ev = replay("bots_cp_badlands.cap");
    let st = states(&ev);
    assert!(st.contains(&"full".to_string()), "{st:?}");
    let last = of_type(&ev, "status").last().copied().unwrap().clone();
    assert_eq!(last["decode_errors"], 0);
    assert_eq!(of_type(&ev, "server")[0]["map"], "cp_badlands");
    assert_eq!(of_type(&ev, "world").len(), 1);
}

#[test]
fn snapshots_track_all_bots_moving() {
    let ev = replay("bots_cp_badlands.cap");
    let snaps = of_type(&ev, "snapshot");
    assert!(snaps.len() > 200, "{} snapshots", snaps.len());
    let mut first: HashMap<u64, [f64; 3]> = HashMap::new();
    let mut moved: HashMap<u64, f64> = HashMap::new();
    let mut last_tick = 0;
    for s in &snaps {
        let tick = s["tick"].as_u64().unwrap();
        assert!(tick >= last_tick, "ticks go forward");
        last_tick = tick;
        let players = s["players"].as_array().unwrap();
        assert_eq!(players.len(), 8);
        for p in players {
            assert!(["red", "blue"].contains(&p["team"].as_str().unwrap()));
            assert_ne!(p["class"], "unknown");
            assert!(!p["name"].as_str().unwrap().is_empty());
            let id = p["id"].as_u64().unwrap();
            if p["pos"].is_null() {
                assert_eq!(p["alive"], false, "only players who died before we connected lack a position");
                continue;
            }
            let here = pos(&p["pos"]);
            let start = *first.entry(id).or_insert(here);
            let m = moved.entry(id).or_insert(0.0);
            *m = m.max(dist(start, here));
        }
    }
    assert!(moved.values().filter(|&&m| m > 300.0).count() >= 6, "{moved:?}");
}

#[test]
fn control_points_have_positions_and_owners() {
    let ev = replay("bots_cp_badlands.cap");
    let snap = of_type(&ev, "snapshot").last().copied().unwrap().clone();
    let cps = snap["objectives"].as_array().unwrap();
    assert_eq!(cps.len(), 5, "cp_badlands has five points");
    for cp in cps {
        assert_eq!(cp["kind"], "control_point");
        assert!(cp["pos"].is_array());
        assert!(["red", "blue", "none"].contains(&cp["owner"].as_str().unwrap()));
    }
}

#[test]
fn shots_are_attributed_to_the_shooter() {
    let ev = replay("bots_cp_badlands.cap");
    let snaps = of_type(&ev, "snapshot");
    let shots = of_type(&ev, "shot");
    assert!(shots.len() > 50);
    let mut right = 0;
    for shot in &shots {
        let tick = shot["tick"].as_u64().unwrap();
        let snap = snaps.iter().min_by_key(|s| s["tick"].as_u64().unwrap().abs_diff(tick)).unwrap();
        let origin = pos(&shot["origin"]);
        let nearest = snap["players"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["alive"] == true)
            .min_by(|a, b| dist(pos(&a["pos"]), origin).total_cmp(&dist(pos(&b["pos"]), origin)))
            .unwrap();
        right += usize::from(nearest["id"] == shot["player"]);
    }
    assert!(right * 100 >= shots.len() * 95, "{right}/{}", shots.len());
}

#[test]
fn events_reference_known_players() {
    let ev = replay("bots_cp_badlands.cap");
    let ids: HashSet<u64> = of_type(&ev, "snapshot")
        .iter()
        .flat_map(|s| s["players"].as_array().unwrap().iter().map(|p| p["id"].as_u64().unwrap()))
        .collect();
    let kills = of_type(&ev, "kill");
    assert!(!kills.is_empty());
    for k in &kills {
        assert!(ids.contains(&k["victim"].as_u64().unwrap()));
        assert!(k["victim_pos"].is_array());
    }
    for d in of_type(&ev, "damage") {
        assert!(ids.contains(&d["victim"].as_u64().unwrap()));
    }
    for p in of_type(&ev, "point_captured") {
        for c in p["cappers"].as_array().unwrap() {
            assert!(ids.contains(&c.as_u64().unwrap()));
        }
    }
    let projectiles: Vec<&Value> =
        of_type(&ev, "snapshot").iter().flat_map(|s| s["projectiles"].as_array().unwrap()).collect();
    assert!(!projectiles.is_empty());
    assert!(projectiles.iter().all(|p| p["owner"].as_u64().is_some_and(|o| ids.contains(&o))));
}

#[test]
fn map_change_is_followed_in_place() {
    let ev = replay("bots_map_change.cap");
    let st = states(&ev);
    let change = st.iter().position(|s| s == "map_change").expect("map change seen");
    assert!(st[..change].contains(&"full".to_string()));
    assert!(st[change..].contains(&"full".to_string()), "signon completes again: {st:?}");
    let changes = st.iter().filter(|s| *s == "map_change").count();
    assert_eq!(of_type(&ev, "server").len(), changes + 1, "every signon restart re-announces the server");
    let change_at = ev.iter().rposition(|e| e["type"] == "status" && e["state"] == "map_change").unwrap();
    let after = ev[change_at..]
        .iter()
        .filter(|e| e["type"] == "snapshot" && !e["players"].as_array().unwrap().is_empty())
        .count();
    assert!(after > 100, "{after} snapshots with players after the map change");
    assert_eq!(of_type(&ev, "status").last().unwrap()["decode_errors"], 0);
}

#[test]
fn demo_files_use_the_same_extractor() {
    let bytes = std::fs::read(format!("{FIXTURES}/tables_11138802.dem")).unwrap();
    let mut out = Vec::new();
    decode::demo_file(&bytes, 10.0, &mut |v| out.push(v)).unwrap();
    assert_eq!(of_type(&out, "server")[0]["map"], "cp_badlands");
}

#[test]
fn explosions_land_near_players() {
    let mut ev = replay("bots_cp_badlands.cap");
    ev.extend(replay("bots_map_change.cap"));
    let mut last: Option<&Value> = None;
    let mut dists = Vec::new();
    for e in &ev {
        match e["type"].as_str() {
            Some("snapshot") => last = Some(e),
            Some("explosion") => {
                let o = pos(&e["origin"]);
                let near = last.and_then(|s| {
                    s["players"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|p| p["alive"] == true)
                        .map(|p| dist(o, pos(&p["pos"])))
                        .min_by(f64::total_cmp)
                });
                dists.extend(near);
            }
            _ => {}
        }
    }
    assert!(dists.len() > 20, "{} explosions", dists.len());
    dists.sort_by(f64::total_cmp);
    assert!(dists[dists.len() / 2] < 400.0, "median {} units from the nearest player", dists[dists.len() / 2]);
}

#[test]
fn conditions_are_plausible() {
    // tf-demo-parser's own decoding reported Halloween and spy conditions on every player.
    let ev = replay("bots_cp_badlands.cap");
    let conds: HashSet<String> = of_type(&ev, "snapshot")
        .iter()
        .flat_map(|s| s["players"].as_array().unwrap().iter())
        .flat_map(|p| p["conds"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()))
        .collect();
    assert!(!conds.iter().any(|c| c.starts_with("Halloween")), "{conds:?}");
    assert!(!conds.contains("Disguising") && !conds.contains("Disguised"), "no spies on the bot server: {conds:?}");
}

#[test]
fn snapshots_carry_score_round_and_timer() {
    let ev = replay("bots_cp_badlands.cap");
    let last = of_type(&ev, "snapshot").last().copied().unwrap().clone();
    assert_eq!(last["score"], serde_json::json!({"red": 0, "blue": 0}));
    assert_eq!(last["round"], "running");
    // No match was started on the bot server, so its 10 minute timer waits, paused.
    assert_eq!(
        last["timers"],
        serde_json::json!([{"team": "none", "remaining": 600.0, "paused": true, "setup": false}])
    );
}

#[test]
fn dead_players_stay_where_they_died() {
    // A dead player's own position is their spectator camera, which follows teammates.
    let mut ev = replay("bots_cp_badlands.cap");
    ev.extend(replay("bots_map_change.cap"));
    let mut died_at: HashMap<u64, [f64; 3]> = HashMap::new();
    let mut dead_snapshots = 0;
    for s in of_type(&ev, "snapshot") {
        for p in s["players"].as_array().unwrap() {
            let id = p["id"].as_u64().unwrap();
            if p["alive"] == true {
                died_at.remove(&id);
                continue;
            }
            dead_snapshots += 1;
            if p["pos"].is_null() {
                continue; // died before the recording started
            }
            let at = *died_at.entry(id).or_insert_with(|| pos(&p["pos"]));
            assert_eq!(pos(&p["pos"]), at, "{} moved while dead", p["name"]);
        }
    }
    assert!(dead_snapshots > 50, "{dead_snapshots}");
}

#[test]
fn sentries_aim_at_what_they_shoot() {
    // bots_engineers.cap: engineer bots building on red, soldiers and a medic attacking from blue.
    let ev = replay("bots_engineers.cap");
    let shots = of_type(&ev, "sentry_shot");
    assert!(shots.len() >= 10, "{} sentry shots", shots.len());
    let snaps = of_type(&ev, "snapshot");
    let (mut errors, mut attacking) = (Vec::new(), 0);
    for shot in &shots {
        assert!(shot["target"].is_u64() && shot["builder"].is_u64(), "{shot}");
        let tick = shot["tick"].as_u64().unwrap();
        let snap = snaps.iter().min_by_key(|s| s["tick"].as_u64().unwrap().abs_diff(tick)).unwrap();
        let sentry = snap["buildings"].as_array().unwrap().iter().find(|b| b["id"] == shot["sentry"]).unwrap();
        attacking += usize::from(sentry["attacking"] == true);
        let target = snap["players"].as_array().unwrap().iter().find(|p| p["id"] == shot["target"]).unwrap();
        let (s, t) = (pos(&sentry["pos"]), pos(&target["pos"]));
        let towards = (t[1] - s[1]).atan2(t[0] - s[0]).to_degrees();
        let aim = sentry["aim"].as_f64().unwrap();
        errors.push(((aim - towards + 540.0).rem_euclid(360.0) - 180.0).abs());
    }
    // The nearest snapshot can be just after the last shot of a burst.
    assert!(attacking * 10 >= shots.len() * 8, "{attacking} of {} shots while attacking", shots.len());
    errors.sort_by(f64::total_cmp);
    assert!(errors[errors.len() / 2] < 5.0, "median aim error {errors:?}");
}

#[test]
fn engineer_buildings_have_kinds_levels_and_teleporter_ends() {
    let ev = replay("bots_engineers.cap");
    let mut seen = HashSet::new();
    for s in of_type(&ev, "snapshot") {
        for b in s["buildings"].as_array().unwrap() {
            assert!(b["builder"].is_u64(), "{b}");
            seen.insert(format!(
                "{}{}{}",
                b["kind"].as_str().unwrap(),
                b["level"],
                b.get("entrance").map_or("", |e| if e == true { " in" } else { " out" })
            ));
        }
    }
    for want in ["teleporter1 in", "teleporter1 out", "sentry1", "sentry2"] {
        assert!(seen.contains(want), "{want} not in {seen:?}");
    }
}

#[test]
fn spawn_events_carry_the_spawn_position() {
    // player_spawn arrives before the player's new position; the event must wait for it.
    let (mut near, mut total) = (0, 0);
    for fixture in ["bots_cp_badlands.cap", "bots_engineers.cap"] {
        let ev = replay(fixture);
        let snaps = of_type(&ev, "snapshot");
        for spawn in of_type(&ev, "spawn") {
            let tick = spawn["tick"].as_u64().unwrap();
            // The first snapshot showing the player alive after spawning.
            let Some(p) = snaps.iter().filter(|s| s["tick"].as_u64().unwrap() >= tick).take(3).find_map(|s| {
                s["players"].as_array().unwrap().iter().find(|p| p["id"] == spawn["player"] && p["alive"] == true)
            }) else {
                continue;
            };
            total += 1;
            near += usize::from(dist(pos(&spawn["pos"]), pos(&p["pos"])) < 150.0);
        }
    }
    assert!(total > 10, "{total} spawns checked");
    assert!(near * 10 >= total * 9, "{near} of {total} spawns where the player appears");
}

#[test]
fn decoder_memory_budget_ends_the_session() {
    let opt = Options { snapshot_hz: 10.0, max_decoder_bytes: 1 << 20, ..Options::default() };
    let events = replay_with("bots_engineers.cap", &opt);
    let end = events.iter().rfind(|e| e["state"] == "disconnected").unwrap();
    assert!(end["reason"].as_str().unwrap().contains("decoder holds over 1 MiB"), "{end}");
}
