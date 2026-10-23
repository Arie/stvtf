//! The intelligence and the bball ball: where it is, who carries it, and its events.
//!
//! `ctf_ballin_sky.dem` is a 2v2 bball match (4.4 minutes, 2026-10-17, from demos.tf): three
//! ball spawns of which one is enabled at a time, pickups, drops, defences and captures.

mod common;

use common::of;
use std::sync::LazyLock;
use stvtf::Line;
use stvtf::output::{FlagAction, FlagStatus, Objective, PlayerId, Position, Round, Snapshot, Team};

static EVENTS: LazyLock<Vec<Line>> = LazyLock::new(|| common::demo("ctf_ballin_sky.dem", 20.0));

/// A flag's position, team, status, carrier and return time.
struct FlagSeen {
    pos: Position,
    team: Team,
    status: FlagStatus,
    carrier: Option<PlayerId>,
    returns_in: Option<f64>,
}

fn flags(snapshot: &Snapshot) -> Vec<FlagSeen> {
    snapshot
        .objectives
        .iter()
        .filter_map(|o| match *o {
            Objective::Flag { pos, team, status, carrier, returns_in, .. } => {
                Some(FlagSeen { pos, team, status, carrier, returns_in })
            }
            _ => None,
        })
        .collect()
}

#[test]
fn one_neutral_ball_at_a_time() {
    let snapshots: Vec<_> = of!(EVENTS, Snapshot).filter(|s| s.round == Round::Running).collect();
    assert!(snapshots.len() > 1000);
    for s in snapshots {
        let f = flags(s);
        assert!(f.len() <= 1, "{} flags at tick {}", f.len(), s.tick);
        if let Some(f) = f.first() {
            assert_eq!(f.team, Team::Unassigned);
        }
    }
}

#[test]
fn a_carried_ball_is_where_its_carrier_is() {
    let mut carried = 0;
    for s in of!(EVENTS, Snapshot) {
        for f in flags(s).into_iter().filter(|f| f.status == FlagStatus::Carried) {
            let holder = s.players.iter().find(|p| Some(p.id) == f.carrier).expect("a carrier");
            assert_eq!(Some(f.pos), holder.pos);
            carried += 1;
        }
    }
    assert!(carried > 1000, "{carried}");
}

#[test]
fn a_dropped_ball_counts_down_to_going_home() {
    let dropped: Vec<f64> = of!(EVENTS, Snapshot)
        .flat_map(flags)
        .filter(|f| f.status == FlagStatus::Dropped)
        .map(|f| f.returns_in.unwrap())
        .collect();
    assert!(!dropped.is_empty(), "the ball was never dropped");
    // The map's return time is 60 s; the reset time is set a tick ahead of the snapshot's clock.
    assert!(dropped.iter().all(|&t| t > 0.0 && t <= 60.2), "{dropped:?}");
}

#[test]
fn flag_events_name_the_players() {
    let events: Vec<_> = of!(EVENTS, Flag).collect();
    let count = |kind: FlagAction| events.iter().filter(|e| e.event == kind).count();
    assert!(
        count(FlagAction::Pickup) > 20
            && count(FlagAction::Captured) > 10
            && count(FlagAction::Dropped) > 10
            && count(FlagAction::Defended) > 10
    );
    for e in &events {
        assert!(e.player.is_some(), "{e:?}");
    }
    // A defence stops a carrier: it names them.
    assert!(events.iter().filter(|e| e.event == FlagAction::Defended).all(|e| e.carrier.is_some()));
}
