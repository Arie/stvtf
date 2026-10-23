//! Payload: carts, rollback and the first capture, from a real highlander match.
//!
//! `pl_upward.dem` is the first 3.4 minutes of an STV demo of a 9v9 on `pl_upward_f12`
//! (serveme.tf, 2026-10-18, from demos.tf), cut with `cargo run --example cut_demo -- IN.dem OUT.dem 13600`:
//! setup, BLU pushing the cart, a rollback, and the first point captured.

mod common;

use common::{dist, of};
use std::sync::LazyLock;
use stvtf::Line;
use stvtf::output::{Objective, Position, Round, Snapshot, Team};

static EVENTS: LazyLock<Vec<Line>> = LazyLock::new(|| common::demo("pl_upward.dem", 20.0));

fn snapshots() -> impl Iterator<Item = &'static Snapshot> {
    of!(EVENTS, Snapshot)
}

/// A cart's position, team, progress, cappers and speed.
struct Cart {
    pos: Position,
    team: Team,
    progress: f64,
    cappers: i64,
    speed: i64,
}

fn carts(snapshot: &Snapshot) -> impl Iterator<Item = Cart> {
    snapshot.objectives.iter().filter_map(|o| match *o {
        Objective::Cart { pos, team, progress, cappers, speed, .. } => {
            Some(Cart { pos, team, progress, cappers, speed })
        }
        _ => None,
    })
}

fn cart(snapshot: &Snapshot) -> Option<Cart> {
    carts(snapshot).next()
}

/// Control point `index`: its position and owner.
fn point(snapshot: &Snapshot, index: u8) -> (Option<Position>, Team) {
    snapshot
        .objectives
        .iter()
        .find_map(|o| match *o {
            Objective::ControlPoint { index: i, pos, owner, .. } if i == index => Some((pos, owner)),
            _ => None,
        })
        .unwrap()
}

#[test]
fn one_blu_cart_in_every_running_snapshot() {
    let running: Vec<_> = snapshots().filter(|s| s.round == Round::Running).collect();
    assert!(running.len() > 1000);
    for s in running {
        assert_eq!(carts(s).count(), 1, "tick {}", s.tick);
        let c = cart(s).unwrap();
        assert_eq!(c.team, Team::Blue);
        assert!((0.0..=1.0).contains(&c.progress), "progress {}", c.progress);
        assert!((-1..=3).contains(&c.speed), "speed {}", c.speed);
    }
}

#[test]
fn cart_advances_along_the_track() {
    let carts: Vec<_> = snapshots().filter_map(cart).collect();
    let start = carts[0].pos;
    let furthest = carts.iter().map(|c| c.progress).fold(0.0, f64::max);
    assert!(furthest > 0.2, "furthest progress {furthest}");
    let moved = carts.iter().map(|c| dist(c.pos, start)).fold(0.0, f64::max);
    assert!(moved > 2000.0, "cart moved at most {moved} units");
    // Pushing it means players on it: the speed level follows the number of cappers.
    assert!(carts.iter().any(|c| c.cappers >= 3 && c.speed == 3));
}

#[test]
fn cart_rolls_back_with_nobody_on_it() {
    let rolling: Vec<_> = snapshots().filter_map(cart).filter(|c| c.speed < 0 && c.cappers == 0).collect();
    assert!(rolling.len() > 20, "{} rollback snapshots", rolling.len());
    let first = rolling[0].pos;
    assert!(rolling.iter().any(|c| dist(c.pos, first) > 20.0), "the cart moves while rolling back");
}

#[test]
fn first_point_is_captured_by_the_cart() {
    let caps: Vec<_> = of!(EVENTS, PointCaptured).collect();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0].point, 0);
    assert_eq!(caps[0].team, Team::Blue);
    assert!(!caps[0].cappers.is_empty(), "a capture without cappers");
    let before = snapshots().filter(|s| s.tick <= caps[0].tick).last().unwrap();
    let gap = dist(cart(before).unwrap().pos, point(before, 0).0.unwrap());
    assert!(gap < 100.0, "cart {gap} units from the point it captures");
    assert_eq!(point(snapshots().last().unwrap(), 0).1, Team::Blue);
}

#[test]
fn setup_timer_runs_out_before_the_push() {
    // The demo starts in the previous round; setup runs from round_active to setup_finished.
    let active = of!(EVENTS, RoundActive).next().unwrap().tick;
    let setup_end = of!(EVENTS, SetupFinished).next().unwrap().tick;
    let during: Vec<_> =
        snapshots().filter(|s| s.round == Round::Running && (active..setup_end).contains(&s.tick)).collect();
    assert!(during.len() > 1000, "{} setup snapshots", during.len());
    for s in during {
        assert!(s.timers.iter().any(|t| t.setup), "tick {}", s.tick);
        assert_eq!(cart(s).unwrap().progress, 0.0, "the cart cannot move during setup");
    }
}
