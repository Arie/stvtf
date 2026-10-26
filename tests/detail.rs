//! Heals, blast jumps and pickups, from the same 9v9 payload demo as `tests/payload.rs`.

mod common;

use common::of;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use stvtf::Line;
use stvtf::output::{
    BuildingKind, Class, HealSource, ImpactKind, JumpKind, JumpPhase, Objective, PickupItem, PlayerId, Round, Team,
};

static EVENTS: LazyLock<Vec<Line>> = LazyLock::new(|| common::demo("pl_upward.dem", 20.0));

static CTF: LazyLock<Vec<Line>> = LazyLock::new(|| common::demo("ctf_turbine.dem", 0.0));

#[test]
fn heals_name_patient_and_amount() {
    let heals: Vec<_> = of!(EVENTS, Heal).collect();
    assert!(heals.len() > 100, "{} heals", heals.len());
    assert!(heals.iter().all(|h| h.amount > 0));
    assert!(heals.iter().any(|h| h.source == HealSource::Medigun));
    assert!(heals.iter().any(|h| h.source == HealSource::Crossbow));
}

#[test]
fn a_heal_of_oneself_is_no_medigun_or_dispenser() {
    // A medic regenerates and an engineer's weapons can heal their wielder: the event names
    // the player as their own healer.
    let heals: Vec<_> = of!(EVENTS, Heal).collect();
    assert!(heals.iter().all(|h| h.healer != Some(h.patient)), "a heal names the patient as healer");
    assert!(heals.iter().any(|h| h.healer.is_none() && h.source == HealSource::Other));
}

#[test]
fn every_blast_jump_starts_before_it_lands() {
    let jumps: Vec<_> = of!(EVENTS, BlastJump).collect();
    assert!(!jumps.is_empty(), "no blast jumps");
    assert!(jumps.iter().any(|j| j.phase == JumpPhase::Land));
    let mut last: HashMap<(PlayerId, JumpKind), JumpPhase> = HashMap::new();
    for j in jumps {
        // A jump can restart before it lands (died, or jumped again), but a landing needs a start.
        if j.phase == JumpPhase::Land {
            assert_eq!(last.get(&(j.player, j.kind)), Some(&JumpPhase::Start), "{j:?}");
        }
        last.insert((j.player, j.kind), j.phase);
    }
}

#[test]
fn pickups_are_health_or_ammo() {
    let p: Vec<_> = of!(EVENTS, Pickup).collect();
    assert!(p.iter().filter(|e| e.item == PickupItem::Ammo).count() > 20);
    assert!(p.iter().filter(|e| e.item == PickupItem::Health).count() > 10);
}

#[test]
fn impacts_and_marks_have_positions() {
    let impacts: Vec<_> = of!(EVENTS, Impact).collect();
    let marks: Vec<_> = of!(EVENTS, Mark).collect();
    assert!(impacts.iter().filter(|e| e.kind == ImpactKind::Blood).count() > 200);
    assert!(impacts.iter().any(|e| e.kind == ImpactKind::Sparks));
    assert!(impacts.iter().any(|e| e.kind == ImpactKind::Ricochet));
    assert!(marks.len() > 100, "{} marks", marks.len());
    let world = of!(EVENTS, World).next().unwrap();
    for pos in impacts.iter().map(|e| e.pos).chain(marks.iter().map(|e| e.pos)) {
        for i in 0..3 {
            assert!(pos[i].is_finite() && pos[i] >= world.min[i] && pos[i] <= world.max[i], "{pos:?}");
        }
    }
}

#[test]
fn votes_pick_from_the_options_sent() {
    let n = of!(CTF, VoteOptions).map(|o| o.options.len()).max().expect("no vote options");
    let casts: Vec<_> = of!(CTF, VoteCast).collect();
    assert!(!casts.is_empty(), "no votes cast");
    assert!(casts.iter().all(|c| usize::from(c.option) < n), "{casts:?}");
}

#[test]
fn medics_get_defended() {
    assert!(of!(CTF, MedicDefended).next().is_some(), "no medic_defended");
}

#[test]
fn match_flow_lines() {
    let t: Vec<_> = of!(CTF, Tournament).collect();
    assert!(!t.is_empty(), "no tournament lines");
    assert!(t.iter().any(|e| e.ready.is_some()));
    let a: Vec<_> = of!(CTF, Announcer).collect();
    assert!(!a.is_empty(), "no announcer lines");
    assert!(a.iter().all(|e| !e.sound.is_empty()), "{a:?}");
}

#[test]
fn broken_captures_name_the_point() {
    let b: Vec<_> = of!(EVENTS, CaptureBroken).collect();
    assert!(!b.is_empty(), "no capture_broken");
    assert!(b.iter().all(|e| !e.name.is_empty()));
}

#[test]
fn points_count_players_standing_on_them() {
    let blue_on_point = |o: &Objective| matches!(o, Objective::ControlPoint { on_point, .. } if on_point.blue > 0);
    assert!(
        of!(EVENTS, Snapshot).filter(|s| s.round == Round::Running).any(|s| s.objectives.iter().any(blue_on_point))
    );
}

#[test]
fn kills_without_an_attacker_have_no_attacker_position() {
    let suicides: Vec<_> = of!(EVENTS, Kill).filter(|k| k.attacker.is_none()).collect();
    assert!(!suicides.is_empty(), "no kill without an attacker");
    for k in suicides {
        assert_eq!(k.attacker_pos, None, "{k:?}");
    }
}

#[test]
fn killed_cappers_match_the_kill_at_that_tick() {
    let k: Vec<_> = of!(EVENTS, KilledCapper).collect();
    assert!(!k.is_empty(), "no killed_capper");
    for e in k {
        assert!(
            of!(EVENTS, Kill).any(|d| d.tick == e.tick && d.attacker == Some(e.killer) && d.victim == e.victim),
            "no kill line for {e:?}"
        );
    }
}

#[test]
fn soldiers_hold_weapons_with_clips() {
    let alive: Vec<_> = of!(EVENTS, Snapshot)
        .filter(|s| s.round == Round::Running)
        .flat_map(|s| &s.players)
        .filter(|p| p.alive)
        .collect();
    let armed = alive.iter().filter(|p| p.weapon.is_some()).count();
    assert!(armed * 10 > alive.len() * 9, "{armed} of {} armed", alive.len());
    // m_iClip1 is networked +1 (so -1 fits unsigned): full clips are 4 (Original) and 8 (stickies).
    let clips = |class: Class, item: i64| -> Vec<i64> {
        alive
            .iter()
            .filter(|p| p.class == class)
            .filter_map(|p| p.weapon.as_ref())
            .filter(|w| w.item == item)
            .filter_map(|w| w.clip)
            .collect()
    };
    let rockets = clips(Class::Soldier, 513);
    assert!(
        !rockets.is_empty() && rockets.iter().all(|&c| (0..=4).contains(&c)) && rockets.contains(&4),
        "{rockets:?}"
    );
    let stickies = clips(Class::Demoman, 207);
    assert!(!stickies.is_empty() && stickies.iter().all(|&c| c <= 8) && stickies.contains(&8), "{stickies:?}");
    assert!(alive.iter().any(|p| p.class == Class::Medic && p.weapon.as_ref().is_some_and(|w| w.clip.is_none())));
}

fn misread_sentries(events: &[Line]) -> usize {
    // Only a real sentry has `mini`; a dispenser or teleporter reported as a sentry has none.
    of!(events, Snapshot)
        .flat_map(|s| &s.buildings)
        .filter(|b| b.kind == BuildingKind::Sentry && b.mini.is_none())
        .count()
}

fn kinds(events: &[Line]) -> HashSet<BuildingKind> {
    of!(events, Snapshot).flat_map(|s| &s.buildings).map(|b| b.kind).collect()
}

#[test]
fn buildings_are_recognised_by_what_they_are() {
    assert_eq!(misread_sentries(&EVENTS), 0);
    assert_eq!(misread_sentries(&CTF), 0);
    let seen: HashSet<BuildingKind> = kinds(&EVENTS).union(&kinds(&CTF)).copied().collect();
    assert!(seen.contains(&BuildingKind::Sentry) && seen.contains(&BuildingKind::Dispenser), "{seen:?}");
}

fn teamless_projectiles(events: &[Line]) -> usize {
    of!(events, Snapshot).flat_map(|s| &s.projectiles).filter(|p| p.team == Team::Unassigned).count()
}

/// An explosion used to drop the nearest projectile within 150 units, often a live sticky beside
/// the blast: it vanished, and came back without team or owner when it next moved.
#[test]
fn projectiles_keep_their_team_through_nearby_explosions() {
    assert_eq!(teamless_projectiles(&EVENTS), 0);
    assert_eq!(teamless_projectiles(&CTF), 0);
}

/// The server keeps a leaver's slot for a while; they used to stay on the map and HUD until then.
#[test]
fn players_who_leave_are_gone_from_the_next_snapshot() {
    let leaves: Vec<(PlayerId, u32)> = of!(CTF, PlayerLeave).map(|e| (e.player, e.tick)).collect();
    assert!(!leaves.is_empty(), "nobody left");
    for s in of!(CTF, Snapshot) {
        for p in &s.players {
            let after = leaves.iter().any(|&(id, tick)| p.id == id && s.tick > tick);
            assert!(!after, "{} at tick {}", p.name, s.tick);
        }
    }
}

#[test]
fn time_added_names_a_team_and_seconds() {
    let added: Vec<_> = of!(EVENTS, TimeAdded).collect();
    assert!(!added.is_empty(), "no time added");
    assert!(added.iter().all(|e| e.seconds > 0));
}

#[test]
fn building_upgrades_and_repairs() {
    assert!(of!(EVENTS, BuildingUpgrade).next().is_some(), "no building upgrades");
    assert!(of!(EVENTS, BuildingRepair).all(|e| e.amount > 0));
}

/// While carried, a building is not sent: it shows on its engineer, not on the map.
#[test]
fn engineers_carry_buildings() {
    let carrying: Vec<_> = of!(EVENTS, Snapshot).flat_map(|s| &s.players).filter(|p| p.carrying.is_some()).collect();
    assert!(!carrying.is_empty(), "nobody carried a building");
    for p in carrying {
        assert!(p.alive);
        assert!(matches!(p.carrying, Some(BuildingKind::Sentry | BuildingKind::Dispenser | BuildingKind::Teleporter)));
    }
}

#[test]
fn dead_players_count_down_to_respawn() {
    let mut last: HashMap<PlayerId, f64> = HashMap::new();
    let (mut seen, mut down, mut steps) = (0, 0, 0);
    for p in of!(EVENTS, Snapshot).flat_map(|s| &s.players) {
        match p.respawn {
            Some(t) => {
                assert!(!p.alive && t > 0.0);
                // The server can move a respawn wave later; nearly every step still counts down.
                if let Some(prev) = last.insert(p.id, t) {
                    steps += 1;
                    down += usize::from(t <= prev + 0.05);
                }
                seen += 1;
            }
            None => {
                last.remove(&p.id);
            }
        }
    }
    assert!(seen > 100);
    assert!(down * 100 >= steps * 98, "{down} of {steps} steps count down");
}

#[test]
fn players_carry_a_scoreboard_line() {
    let last = of!(EVENTS, Snapshot).last().unwrap();
    let lines: Vec<_> = last.players.iter().filter_map(|p| p.score.as_ref()).collect();
    assert!(lines.len() >= 12);
    for l in lines {
        let columns = [l.points, l.kills, l.deaths, l.assists, l.damage, l.healing, l.dominations, l.streak];
        assert!(columns.iter().all(|&c| c >= 0), "{l:?}");
    }
    let dealt = |p: &stvtf::output::Player| p.score.as_ref().is_some_and(|s| s.damage > 0);
    assert!(of!(EVENTS, Snapshot).any(|s| s.players.iter().any(dealt)));
}

/// Damage on the scoreboard is the damage events since the match (re)started, as logs.tf counts it.
#[test]
fn scoreboard_damage_adds_up_the_damage_events() {
    let mut points: HashMap<PlayerId, i64> = HashMap::new();
    let mut dealt: HashMap<PlayerId, i64> = HashMap::new();
    let mut last = None;
    for e in EVENTS.iter() {
        match e {
            Line::Snapshot(s) => {
                for p in &s.players {
                    let pts = p.score.as_ref().map_or(0, |s| s.points);
                    if points.insert(p.id, pts).is_some_and(|old| pts < old) {
                        dealt.clear();
                    }
                }
                last = Some(s);
            }
            Line::Damage(d) => {
                if let Some(attacker) = d.attacker.filter(|&a| a != d.victim) {
                    *dealt.entry(attacker).or_default() += i64::from(d.amount);
                }
            }
            _ => {}
        }
    }
    for p in &last.unwrap().players {
        let shown = p.score.as_ref().expect("a scoreboard line").damage;
        assert_eq!(shown, dealt.get(&p.id).copied().unwrap_or(0), "{}", p.name);
    }
}

#[test]
fn engineer_and_spy_events_name_players() {
    let players: HashSet<PlayerId> = of!(EVENTS, Snapshot).flat_map(|s| s.players.iter().map(|p| p.id)).collect();
    let teleports: Vec<_> = of!(EVENTS, Teleported).collect();
    assert_eq!(teleports.len(), 2);
    assert!(teleports.iter().all(|t| players.contains(&t.player) && t.builder.is_some() && t.dist > 100.0));
    let sapped: Vec<_> = of!(EVENTS, Sapped).collect();
    assert_eq!(sapped.len(), 4);
    assert!(
        sapped
            .iter()
            .all(|s| players.contains(&s.player) && players.contains(&s.owner) && s.object != BuildingKind::Unknown)
    );
    assert_eq!(of!(EVENTS, BuildingRemoved).count(), 1);
    let dets: Vec<_> = of!(EVENTS, DetStickies).collect();
    assert_eq!(dets.len(), 119);
    assert!(dets.iter().all(|d| players.contains(&d.player)));
}

#[test]
fn captures_start_with_their_cappers() {
    let started: Vec<_> = of!(EVENTS, CaptureStarted).collect();
    assert_eq!(started.len(), 16);
    assert!(started.iter().all(|c| matches!(c.team, Team::Red | Team::Blue) && !c.cappers.is_empty()));
    assert!(started.iter().flat_map(|c| &c.cappers).all(Option::is_some));
}

#[test]
fn damage_to_buildings() {
    let npc: Vec<_> = of!(EVENTS, NpcDamage).collect();
    assert_eq!(npc.len(), 117);
    assert!(npc.iter().all(|n| n.amount > 0));
    let buildings: HashSet<u32> = of!(EVENTS, Snapshot).flat_map(|s| s.buildings.iter().map(|b| b.id)).collect();
    let on_buildings = npc.iter().filter(|n| buildings.contains(&u32::from(n.id))).count();
    assert!(on_buildings >= 110, "{on_buildings} of them on buildings");
    // A sapper drains a building with no attacker.
    assert!(npc.iter().any(|n| n.attacker.is_none()) && npc.iter().any(|n| n.attacker.is_some()));
}

#[test]
fn votes_start_and_pass() {
    let started: Vec<_> = of!(CTF, VoteStarted).collect();
    assert_eq!(started.len(), 1);
    let v = started[0];
    assert_eq!(
        (v.issue.as_str(), v.details.as_str(), v.yes_no, v.team),
        ("#TF_vote_nextlevel", "pl_goldrush", true, Team::Unassigned)
    );
    assert!(v.caller.is_some(), "the caller is a player");
    let passed: Vec<_> = of!(CTF, VotePassed).collect();
    assert_eq!(passed.len(), 1);
    assert_eq!((passed[0].issue.as_str(), passed[0].details.as_str()), ("#TF_vote_passed_nextlevel", "pl_goldrush"));
}
