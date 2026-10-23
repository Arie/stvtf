//! Integration tests over real `SourceTV` traffic recorded from a serveme.tf test
//! reservation (TF2 build 11138802, `cp_badlands`, bots) with `connect --record`.
//! `bots_map_change.cap` is one connection that followed a `changelevel`: during the
//! `tv_delay` window the server restarted signon in place four times, then streamed on.
//! `bots_engineers.cap` has engineer bots building (sentries, teleporters) under attack.

mod common;

use common::{dist, of};
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use stvtf::capture;
use stvtf::decode::{self, Tables};
use stvtf::output::{BuildingKind, Class, Condition, Objective, PlayerId, Position, Round, Score, Status, Team, Timer};
use stvtf::{Line, Options};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn tables() -> Tables {
    Tables::from_demo_bytes(stvtf::builtin(stvtf::CURRENT_BUILD).unwrap().tables).unwrap()
}

/// Decodes a fixture once and shares the result between tests.
fn replay(name: &str) -> &'static [Line] {
    static CACHE: LazyLock<Mutex<HashMap<String, &'static [Line]>>> = LazyLock::new(Mutex::default);
    let mut cache = CACHE.lock().unwrap();
    cache
        .entry(name.to_string())
        .or_insert_with(|| replay_with(name, &Options { snapshot_hz: 10.0, ..Options::default() }).leak())
}

fn replay_with(name: &str, opt: &Options) -> Vec<Line> {
    let bytes = std::fs::read(format!("{FIXTURES}/{name}")).unwrap();
    let records = capture::read(&bytes).unwrap();
    let mut out = Vec::new();
    capture::replay(&records, &tables(), opt, &mut |l| out.push(l)).unwrap();
    out
}

fn statuses(events: &[Line]) -> Vec<&Status> {
    of!(events, Status).collect()
}

fn reached_full(statuses: &[&Status]) -> bool {
    statuses.iter().any(|s| matches!(s, Status::Full { .. }))
}

fn decode_errors(events: &[Line]) -> u64 {
    match of!(events, Status).last() {
        Some(Status::Disconnected { decode_errors, .. }) => *decode_errors,
        other => panic!("ends with {other:?}"),
    }
}

/// Where a player is, for one who has a position.
fn at(pos: Option<Position>) -> Position {
    pos.expect("a position")
}

#[test]
fn current_build_tables_are_built_in() {
    let t = tables();
    assert!(t.class_names().count() > 300);
    assert!(t.class_names().any(|c| c == "CTFPlayer"));
    let full = Tables::from_demo(format!("{FIXTURES}/tables_11138802.dem").as_ref()).unwrap();
    assert_eq!(t.class_names().count(), full.class_names().count());
    assert_eq!(t.send_table_count(), full.send_table_count());
}

#[test]
fn session_reaches_full_without_decode_errors() {
    let ev = replay("bots_cp_badlands.cap");
    assert!(reached_full(&statuses(ev)), "{:?}", statuses(ev));
    assert_eq!(decode_errors(ev), 0);
    assert_eq!(of!(ev, Server).next().unwrap().map, "cp_badlands");
    assert_eq!(of!(ev, World).count(), 1);
}

#[test]
fn snapshots_track_all_bots_moving() {
    let ev = replay("bots_cp_badlands.cap");
    let snaps: Vec<_> = of!(ev, Snapshot).collect();
    assert!(snaps.len() > 200, "{} snapshots", snaps.len());
    let mut first: HashMap<PlayerId, Position> = HashMap::new();
    let mut moved: HashMap<PlayerId, f64> = HashMap::new();
    let mut last_tick = 0;
    for s in &snaps {
        assert!(s.tick >= last_tick, "ticks go forward");
        last_tick = s.tick;
        assert_eq!(s.players.len(), 8);
        for p in &s.players {
            assert!(matches!(p.team, Team::Red | Team::Blue));
            assert_ne!(p.class, Class::Unknown);
            assert!(!p.name.is_empty(), "a player without a name");
            let Some(here) = p.pos else {
                assert!(!p.alive, "only players who died before we connected lack a position");
                continue;
            };
            let start = *first.entry(p.id).or_insert(here);
            let m = moved.entry(p.id).or_insert(0.0);
            *m = m.max(dist(start, here));
        }
    }
    assert!(moved.values().filter(|&&m| m > 300.0).count() >= 6, "{moved:?}");
}

#[test]
fn control_points_have_positions_and_owners() {
    let ev = replay("bots_cp_badlands.cap");
    let snap = of!(ev, Snapshot).last().unwrap();
    assert_eq!(snap.objectives.len(), 5, "cp_badlands has five points");
    for cp in &snap.objectives {
        assert!(
            matches!(
                cp,
                Objective::ControlPoint { pos: Some(_), owner: Team::Red | Team::Blue | Team::Unassigned, .. }
            ),
            "{cp:?}"
        );
    }
}

#[test]
fn shots_are_attributed_to_the_shooter() {
    let ev = replay("bots_cp_badlands.cap");
    let snaps: Vec<_> = of!(ev, Snapshot).collect();
    let shots: Vec<_> = of!(ev, Shot).collect();
    assert!(shots.len() > 50);
    let mut right = 0;
    for shot in &shots {
        let snap = snaps.iter().min_by_key(|s| s.tick.abs_diff(shot.tick)).unwrap();
        let nearest = snap
            .players
            .iter()
            .filter(|p| p.alive)
            .min_by(|a, b| dist(at(a.pos), shot.origin).total_cmp(&dist(at(b.pos), shot.origin)))
            .unwrap();
        right += usize::from(Some(nearest.id) == shot.player);
    }
    assert!(right * 100 >= shots.len() * 95, "{right}/{}", shots.len());
}

#[test]
fn events_reference_known_players() {
    let ev = replay("bots_cp_badlands.cap");
    let ids: HashSet<PlayerId> = of!(ev, Snapshot).flat_map(|s| s.players.iter().map(|p| p.id)).collect();
    let kills: Vec<_> = of!(ev, Kill).collect();
    assert!(!kills.is_empty(), "no kills");
    for k in &kills {
        assert!(ids.contains(&k.victim));
        assert!(k.victim_pos.is_some());
    }
    assert!(of!(ev, Damage).all(|d| ids.contains(&d.victim)));
    for p in of!(ev, PointCaptured) {
        assert!(p.cappers.iter().all(|c| c.is_some_and(|c| ids.contains(&c))), "{p:?}");
    }
    let projectiles: Vec<_> = of!(ev, Snapshot).flat_map(|s| &s.projectiles).collect();
    assert!(!projectiles.is_empty(), "no projectiles");
    assert!(projectiles.iter().all(|p| p.owner.is_some_and(|o| ids.contains(&o))));
}

#[test]
fn map_change_is_followed_in_place() {
    let ev = replay("bots_map_change.cap");
    let st = statuses(ev);
    let change = st.iter().position(|s| **s == Status::MapChange).expect("map change seen");
    assert!(reached_full(&st[..change]));
    assert!(reached_full(&st[change..]), "signon completes again: {st:?}");
    let changes = st.iter().filter(|s| ***s == Status::MapChange).count();
    assert_eq!(of!(ev, Server).count(), changes + 1, "every signon restart re-announces the server");
    let change_at = ev.iter().rposition(|e| *e == Line::Status(Status::MapChange)).unwrap();
    let after = of!(ev[change_at..], Snapshot).filter(|s| !s.players.is_empty()).count();
    assert!(after > 100, "{after} snapshots with players after the map change");
    assert_eq!(decode_errors(ev), 0);
}

#[test]
fn demo_files_use_the_same_extractor() {
    let bytes = std::fs::read(format!("{FIXTURES}/tables_11138802.dem")).unwrap();
    let mut out = Vec::new();
    decode::demo_file(&bytes, 10.0, &mut |l| out.push(l)).unwrap();
    assert_eq!(of!(out, Server).next().unwrap().map, "cp_badlands");
}

#[test]
fn explosions_land_near_players() {
    let ev = [replay("bots_cp_badlands.cap"), replay("bots_map_change.cap")].concat();
    let mut last = None;
    let mut dists = Vec::new();
    for e in &ev {
        match e {
            Line::Snapshot(s) => last = Some(s),
            Line::Explosion(x) => {
                let near = last.and_then(|s| {
                    s.players.iter().filter(|p| p.alive).map(|p| dist(x.origin, at(p.pos))).min_by(f64::total_cmp)
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
    let conds: HashSet<&str> =
        of!(ev, Snapshot).flat_map(|s| &s.players).flat_map(|p| &p.conds).map(|c| Condition::name(*c)).collect();
    assert!(!conds.iter().any(|c| c.starts_with("Halloween")), "{conds:?}");
    assert!(!conds.contains("Disguising") && !conds.contains("Disguised"), "no spies on the bot server: {conds:?}");
}

#[test]
fn snapshots_carry_score_round_and_timer() {
    let ev = replay("bots_cp_badlands.cap");
    let last = of!(ev, Snapshot).last().unwrap();
    assert_eq!(last.score, Score { red: 0, blue: 0 });
    assert_eq!(last.round, Round::Running);
    // No match was started on the bot server, so its 10 minute timer waits, paused.
    assert_eq!(last.timers, [Timer { team: Team::Unassigned, remaining: 600.0, paused: true, setup: false }]);
}

#[test]
fn dead_players_stay_where_they_died() {
    // A dead player's own position is their spectator camera, which follows teammates.
    let ev = [replay("bots_cp_badlands.cap"), replay("bots_map_change.cap")].concat();
    let mut died_at: HashMap<PlayerId, Position> = HashMap::new();
    let mut dead_snapshots = 0;
    for p in of!(ev, Snapshot).flat_map(|s| &s.players) {
        if p.alive {
            died_at.remove(&p.id);
            continue;
        }
        dead_snapshots += 1;
        let Some(pos) = p.pos else {
            continue; // died before the recording started
        };
        let at = *died_at.entry(p.id).or_insert(pos);
        assert_eq!(pos, at, "{} moved while dead", p.name);
    }
    assert!(dead_snapshots > 50, "{dead_snapshots}");
}

#[test]
fn sentries_aim_at_what_they_shoot() {
    // bots_engineers.cap: engineer bots building on red, soldiers and a medic attacking from blue.
    let ev = replay("bots_engineers.cap");
    let shots: Vec<_> = of!(ev, SentryShot).collect();
    assert!(shots.len() >= 10, "{} sentry shots", shots.len());
    let snaps: Vec<_> = of!(ev, Snapshot).collect();
    let (mut errors, mut attacking) = (Vec::new(), 0);
    for shot in &shots {
        assert!(shot.builder.is_some(), "{shot:?}");
        let target = shot.target.expect("a target");
        let snap = snaps.iter().min_by_key(|s| s.tick.abs_diff(shot.tick)).unwrap();
        let sentry = snap.buildings.iter().find(|b| b.id == shot.sentry).unwrap();
        attacking += usize::from(sentry.attacking == Some(true));
        let target = snap.players.iter().find(|p| p.id == target).unwrap();
        let (s, t) = (sentry.pos, at(target.pos));
        let towards = (t[1] - s[1]).atan2(t[0] - s[0]).to_degrees();
        let aim = sentry.aim.unwrap();
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
    for b in of!(ev, Snapshot).flat_map(|s| &s.buildings) {
        assert!(b.builder.is_some(), "{b:?}");
        seen.insert((b.kind, b.level, b.entrance));
    }
    for want in [
        (BuildingKind::Teleporter, 1, Some(true)),
        (BuildingKind::Teleporter, 1, Some(false)),
        (BuildingKind::Sentry, 1, None),
        (BuildingKind::Sentry, 2, None),
    ] {
        assert!(seen.contains(&want), "{want:?} not in {seen:?}");
    }
}

#[test]
fn spawn_events_carry_the_spawn_position() {
    // player_spawn arrives before the player's new position; the event must wait for it.
    let (mut near, mut total) = (0, 0);
    for fixture in ["bots_cp_badlands.cap", "bots_engineers.cap"] {
        let ev = replay(fixture);
        let snaps: Vec<_> = of!(ev, Snapshot).collect();
        for spawn in of!(ev, Spawn) {
            // The first snapshot showing the player alive after spawning.
            let Some(p) = snaps
                .iter()
                .filter(|s| s.tick >= spawn.tick)
                .take(3)
                .find_map(|s| s.players.iter().find(|p| p.id == spawn.player && p.alive))
            else {
                continue;
            };
            total += 1;
            near += usize::from(dist(at(spawn.pos), at(p.pos)) < 150.0);
        }
    }
    assert!(total > 10, "{total} spawns checked");
    assert!(near * 10 >= total * 9, "{near} of {total} spawns where the player appears");
}

#[test]
fn decoder_memory_budget_ends_the_session() {
    let opt = Options { snapshot_hz: 10.0, max_decoder_bytes: 1 << 20, ..Options::default() };
    let events = replay_with("bots_engineers.cap", &opt);
    let reason = of!(events, Status)
        .filter_map(|s| match s {
            Status::Disconnected { reason, .. } => Some(reason),
            _ => None,
        })
        .next_back()
        .unwrap();
    assert!(reason.contains("decoder holds over 1 MiB"), "{reason}");
}
