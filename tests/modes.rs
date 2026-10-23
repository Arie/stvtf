//! The non-standard game modes: pass time, robot and player destruction, special delivery,
//! Mann vs. Machine and territorial control, each from the start of a match on demos.tf.

mod common;

use common::{dist, of};
use std::sync::LazyLock;
use stvtf::Line;
use stvtf::output::{Objective, PlayerId, Position, Snapshot, Team};

static PASS: LazyLock<Vec<Line>> = LazyLock::new(|| common::demo("pass_stadium.dem", 20.0));
static RD: LazyLock<Vec<Line>> = LazyLock::new(|| common::demo("rd_asteroid.dem", 20.0));
static PD: LazyLock<Vec<Line>> = LazyLock::new(|| common::demo("pd_watergate.dem", 20.0));
static MVM: LazyLock<Vec<Line>> = LazyLock::new(|| common::demo("mvm_crossing.dem", 5.0));

fn player_pos(snapshot: &Snapshot, id: PlayerId) -> Option<Position> {
    snapshot.players.iter().find(|p| p.id == id).and_then(|p| p.pos)
}

/// The ball of each snapshot that has one: position, team, carrier.
fn balls(lines: &[Line]) -> Vec<(&Snapshot, Position, Team, Option<PlayerId>)> {
    of!(lines, Snapshot)
        .filter_map(|s| {
            let mut balls = s.objectives.iter().filter_map(|o| match *o {
                Objective::Ball { pos, team, carrier, .. } => Some((s, pos, team, carrier)),
                _ => None,
            });
            let ball = balls.next();
            assert!(balls.next().is_none(), "one ball at tick {}", s.tick);
            ball
        })
        .collect()
}

#[test]
fn the_pass_time_ball_moves_and_is_carried_by_players() {
    let balls = balls(&PASS);
    assert!(balls.len() > 500, "{} snapshots with a ball", balls.len());
    let carried: Vec<_> = balls.iter().filter(|b| b.3.is_some()).collect();
    assert!(carried.len() > 100, "{} snapshots with the ball carried", carried.len());
    for (s, pos, team, carrier) in &carried {
        let id = carrier.unwrap();
        let player = s.players.iter().find(|p| p.id == id).expect("the carrier is a player");
        assert_eq!(*team, player.team, "the ball takes its carrier's team at tick {}", s.tick);
        assert_eq!(Some(*pos), player.pos, "a carried ball is where its carrier is");
    }
    let loose: Vec<Position> = balls.iter().filter(|b| b.3.is_none()).map(|b| b.1).collect();
    let spread = loose.iter().map(|p| dist(*p, loose[0])).fold(0.0, f64::max);
    assert!(spread > 500.0, "a loose ball flies about: {spread}");
}

#[test]
fn pass_time_has_a_goal_for_each_team() {
    let s = of!(PASS, Snapshot).last().unwrap();
    let goals: Vec<Team> = s
        .objectives
        .iter()
        .filter_map(|o| match o {
            Objective::Goal { team, .. } => Some(*team),
            _ => None,
        })
        .collect();
    assert!(goals.contains(&Team::Red) && goals.contains(&Team::Blue), "{goals:?}");
}

#[test]
fn pass_time_counts_goals_as_captures() {
    let first = of!(PASS, Snapshot).find_map(|s| s.captures.as_ref()).unwrap();
    assert_eq!((first.red, first.blue), (0, 0));
    let last = of!(PASS, Snapshot).last().unwrap();
    let goals = last.captures.as_ref().map(|c| c.red + c.blue);
    assert!(goals.is_some_and(|g| g > 0), "{goals:?}");
}

#[test]
fn robot_destruction_has_robots_and_points() {
    let snapshots: Vec<&Snapshot> = of!(RD, Snapshot).collect();
    let robots: Vec<(u16, u16, Team)> = snapshots
        .iter()
        .flat_map(|s| &s.objectives)
        .filter_map(|o| match *o {
            Objective::Robot { hp, max_hp, team, .. } => Some((hp, max_hp, team)),
            _ => None,
        })
        .collect();
    assert!(robots.len() > 1000);
    assert!(robots.iter().all(|&(hp, max, _)| hp <= max && max > 0));
    assert!(robots.iter().any(|r| r.2 == Team::Red) && robots.iter().any(|r| r.2 == Team::Blue));
    assert!(robots.iter().any(|r| r.0 < r.1), "robots get hurt");
    let points: Vec<_> = snapshots.iter().skip_while(|s| s.points.is_none()).map(|s| s.points.as_ref()).collect();
    assert!(points.len() + 1 >= snapshots.len(), "the points from the logic's first update on");
    let points: Vec<_> = points.into_iter().map(Option::unwrap).collect();
    let last = points.last().unwrap();
    assert!(last.red + last.blue > 0, "points are scored");
    let cores: Vec<Team> = snapshots
        .iter()
        .flat_map(|s| &s.objectives)
        .filter_map(|o| match o {
            Objective::Core { team, .. } => Some(*team),
            _ => None,
        })
        .collect();
    assert!(
        !cores.is_empty() && cores.iter().all(|t| matches!(t, Team::Red | Team::Blue)),
        "destroyed robots drop cores"
    );
}

#[test]
fn player_destruction_has_points_leaders_and_bottles() {
    let snapshots: Vec<&Snapshot> = of!(PD, Snapshot).collect();
    let last = snapshots.last().unwrap().points.as_ref().unwrap();
    assert!(last.red + last.blue > 0 && last.max.is_some_and(|m| m > 0), "{last:?}");
    let with_leader = |pick: fn(&stvtf::output::Points) -> Option<PlayerId>| {
        snapshots
            .iter()
            .filter(|s| s.points.as_ref().and_then(pick).is_some_and(|l| player_pos(s, l).is_some()))
            .count()
    };
    // In this cut only red has a leader.
    assert!(with_leader(|p| p.red_leader) > 100);
    let bottles = snapshots
        .iter()
        .flat_map(|s| &s.objectives)
        .filter(|o| matches!(o, Objective::Flag { points: Some(p), .. } if *p > 0))
        .count();
    assert!(bottles > 0);
}

#[test]
fn mvm_has_its_wave_and_a_tank() {
    let snapshots: Vec<&Snapshot> = of!(MVM, Snapshot).collect();
    let waves: Vec<_> = snapshots.iter().skip_while(|s| s.mvm.is_none()).map(|s| s.mvm.as_ref()).collect();
    assert!(waves.len() + 1 >= snapshots.len(), "the wave from the objective resource's first update on");
    let waves: Vec<_> = waves.into_iter().map(Option::unwrap).collect();
    assert!(waves.iter().all(|w| w.waves == 5 && (1..=5).contains(&w.wave)));
    assert!(waves.iter().any(|w| w.between_waves) && waves.iter().any(|w| !w.between_waves));
    let tanks: Vec<f64> = snapshots
        .iter()
        .flat_map(|s| &s.objectives)
        .filter_map(|o| match *o {
            Objective::Tank { health, .. } => Some(health),
            _ => None,
        })
        .collect();
    assert!(!tanks.is_empty() && tanks.iter().all(|h| (0.0..=1.0).contains(h)), "{tanks:?}");
}

#[test]
fn mode_fields_only_where_the_mode_has_them() {
    let s = of!(PASS, Snapshot).last().unwrap();
    assert!(s.points.is_none() && s.mvm.is_none());
    let rd = of!(RD, Snapshot).last().unwrap();
    assert!(rd.mvm.is_none() && rd.captures.is_some(), "robot destruction has flags (cores)");
}

/// Each player id in the lines named, for the ids that must be players of the match.
fn known(lines: &[Line]) -> std::collections::HashSet<PlayerId> {
    of!(lines, Snapshot).flat_map(|s| s.players.iter().map(|p| p.id)).collect()
}

#[test]
fn pass_time_events_name_the_players_with_the_ball() {
    let players = known(&PASS);
    let gets: Vec<_> = of!(PASS, PassGet).collect();
    assert_eq!(gets.len(), 8);
    for get in gets {
        // The next snapshot has the ball with that player (unless they lost it at once).
        let next = of!(PASS, Snapshot).find(|s| s.tick > get.tick).unwrap();
        let carrier = next.objectives.iter().find_map(|o| match o {
            Objective::Ball { carrier, .. } => *carrier,
            _ => None,
        });
        assert_eq!(carrier, Some(get.player), "pass_get at {}", get.tick);
    }
    for free in of!(PASS, PassFree) {
        assert!(players.contains(&free.player) && free.attacker.is_none_or(|a| players.contains(&a)));
    }
    assert_eq!(of!(PASS, BallCarrierKilled).count(), 3);
    for killed in of!(PASS, BallCarrierKilled) {
        let kill = of!(PASS, Kill).find(|k| k.tick == killed.tick && k.victim == killed.victim);
        assert!(kill.is_some_and(|k| k.attacker == killed.attacker), "ball_carrier_killed at {}", killed.tick);
    }
    let caught: Vec<_> = of!(PASS, PassCaught).collect();
    assert!(caught.iter().all(|c| players.contains(&c.passer) && players.contains(&c.catcher) && c.dist > 0.0));
    let stolen: Vec<_> = of!(PASS, PassStolen).collect();
    assert!(stolen.len() == 1 && players.contains(&stolen[0].victim) && players.contains(&stolen[0].attacker));
}

#[test]
fn pass_time_goals_add_up_to_the_captures() {
    let scores: Vec<_> = of!(PASS, PassScore).collect();
    assert_eq!(scores.len(), 3);
    let last = of!(PASS, Snapshot).last().unwrap().captures.clone().unwrap();
    assert_eq!(scores.iter().map(|s| i64::from(s.points)).sum::<i64>(), last.red + last.blue);
    for score in scores {
        // The scorer carried the ball in the 10 seconds before (then threw or ran it in).
        let carried = of!(PASS, Snapshot).filter(|s| s.tick < score.tick && s.tick + 667 > score.tick).any(|s| {
            s.objectives.iter().any(|o| matches!(o, Objective::Ball { carrier: Some(c), .. } if *c == score.scorer))
        });
        assert!(carried, "the scorer had the ball before the goal at {}", score.tick);
    }
}

#[test]
fn destruction_events_name_players_of_the_match() {
    let players = known(&RD);
    let killed: Vec<_> = of!(RD, RobotKilled).collect();
    assert_eq!(killed.len(), 2);
    assert!(killed.iter().all(|k| k.attacker.is_some_and(|a| players.contains(&a)) && !k.weapon.is_empty()));
    let scored: Vec<_> = of!(RD, PointsScored).collect();
    assert_eq!(scored.len(), 10);
    assert!(scored.iter().all(|s| players.contains(&s.player) && s.method != stvtf::output::ScoreMethod::Unknown));
    let players = known(&PD);
    let special: Vec<_> = of!(PD, SpecialScore).collect();
    assert!(special.len() == 2 && special.iter().all(|s| players.contains(&s.player)));
    assert_eq!(of!(PD, TeamLeaderKilled).count(), 1);
    for leader in of!(PD, TeamLeaderKilled) {
        let kill = of!(PD, Kill).find(|k| k.tick == leader.tick && k.victim == leader.victim);
        assert!(kill.is_some_and(|k| k.attacker == leader.killer), "team_leader_killed at {}", leader.tick);
    }
    assert_eq!(of!(PD, Stun).count(), 2);
}

#[test]
fn special_delivery_opens_the_rocket_and_tc_selects_its_round() {
    let sd = common::demo("sd_doomsday.dem", 0.0);
    let open: Vec<_> = of!(sd, RocketOpen).collect();
    assert!(open.len() == 1 && matches!(open[0].team, Team::Red | Team::Blue));
    let tc = common::demo("tc_hydro.dem", 0.0);
    let rounds: Vec<&str> = of!(tc, RoundSelected).map(|r| r.round.as_str()).collect();
    assert!(!rounds.is_empty() && rounds.iter().all(|r| r.starts_with("round_")), "{rounds:?}");
}

#[test]
fn players_without_a_class_are_not_alive() {
    for lines in [&*PASS, &*PD] {
        let classless: Vec<_> = of!(lines, Snapshot)
            .flat_map(|s| &s.players)
            .filter(|p| p.class == stvtf::output::Class::Unknown)
            .collect();
        assert!(!classless.is_empty(), "players join before picking a class");
        assert!(classless.iter().all(|p| !p.alive && p.pos.is_none()), "{:?}", classless.iter().find(|p| p.alive));
    }
}

#[test]
fn mvm_waves_list_their_robots() {
    let waves: Vec<_> = of!(MVM, Snapshot).filter_map(|s| s.mvm.as_ref()).collect();
    let names = |wave: u16| -> Vec<&str> {
        let w = waves.iter().find(|w| w.wave == wave).unwrap();
        w.classes.iter().map(|c| c.name.as_str()).collect()
    };
    assert_eq!(names(1), ["scout", "soldier", "heavy", "soldier_barrage", "sentry_buster"]);
    assert_eq!(names(2), ["tank", "pyro", "pyro_flare", "heavy_heater", "sentry_buster"]);
    let buster = waves.last().unwrap().classes.iter().find(|c| c.name == "sentry_buster").unwrap();
    assert!(buster.mission && buster.giant && !buster.support);
    // Counts go down as robots die.
    let scouts = |w: &stvtf::output::Mvm| w.classes.iter().find(|c| c.name == "scout").map(|c| c.count);
    assert!(waves.windows(2).any(|w| w[0].wave == w[1].wave && scouts(w[1]) < scouts(w[0])));
}

#[test]
fn mvm_has_revive_markers_money_and_upgrades() {
    let players = known(&MVM);
    let objectives: Vec<&Objective> = of!(MVM, Snapshot).flat_map(|s| &s.objectives).collect();
    let owners: Vec<_> = objectives
        .iter()
        .filter_map(|o| match o {
            Objective::Revive { owner, .. } => Some(*owner),
            _ => None,
        })
        .collect();
    assert!(!owners.is_empty() && owners.iter().all(|o| o.is_some_and(|p| players.contains(&p))));
    assert!(objectives.iter().any(|o| matches!(o, Objective::Money { .. })));
    let bought: Vec<(u16, u16)> = of!(MVM, Upgrade).map(|u| (u.item, u.cost)).collect();
    assert_eq!(bought[..5], [(30015, 50), (25, 350), (30015, 50), (30015, 50), (30015, 50)]);
    assert!(of!(MVM, Upgrade).all(|u| players.contains(&u.player)));
}

#[test]
fn only_payload_maps_have_carts() {
    // pd_watergate's UFO rides a train watcher of no team: it is not a cart.
    assert!(!of!(PD, Snapshot).flat_map(|s| &s.objectives).any(|o| matches!(o, Objective::Cart { .. })));
}
