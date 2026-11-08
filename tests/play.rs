//! A relay playing a demo, end to end: stvtf connects to it over UDP as any client would, joins
//! while it plays, follows a jump back and a pause. What the client sees must be the demo's.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use stvtf::Line;
use stvtf::output::{Snapshot, Status};
use stvtf::relay::{Playback, Relay, RelayConfig, Want};
use stvtf::watch::{self, Watch};

const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pl_upward.dem");
/// A bound on what should take moments, for a broken relay.
const LIMIT: Duration = Duration::from_secs(30);

/// The demo's snapshots, one per frame, by tick.
fn demo_snapshots() -> BTreeMap<u32, Snapshot> {
    let bytes = std::fs::read(DEMO).unwrap();
    let mut out = BTreeMap::new();
    stvtf::decode::demo_file(&bytes, 1000.0, &mut |l| {
        if let Line::Snapshot(s) = l {
            out.insert(s.tick, s);
        }
    })
    .unwrap();
    out
}

/// Where the living players are, by id: what a client that came in late knows as well as one that
/// saw it all.
fn places(s: &Snapshot) -> BTreeMap<u16, (String, [f64; 3])> {
    s.players
        .iter()
        .filter(|p| p.alive)
        .filter_map(|p| {
            let pos = p.pos?;
            Some((p.id.0, (format!("{:?} {:?}", p.class, p.team), pos)))
        })
        .collect()
}

fn want(tick: u32, paused: bool, from_start: bool) -> Want {
    Want { demo: PathBuf::from(DEMO), tick, at: Instant::now(), paused, speed: 4.0, from_start, camera: None }
}

/// What one client got, with when.
type Seen = Arc<Mutex<Vec<(Instant, Line)>>>;

/// Plays the demo as `playback` says while `test` runs; `test` gets the address and can start
/// clients with [`client`].
fn playing<T>(playback: &Playback, test: impl FnOnce(&str) -> T) -> T {
    let config = RelayConfig { password: "pw".into(), ..RelayConfig::new("127.0.0.1:0") };
    let relay = Relay::bind(&config).unwrap();
    let addr = relay.local_addr().unwrap().to_string();
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| relay.play(playback, &stop, &mut |_| {}).unwrap());
        let _stop = StopOnDrop(&stop);
        test(&addr)
    })
}

/// Sets a stop flag however the scope ends, so a failed test does not leave threads running.
struct StopOnDrop<'a>(&'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// A client of the relay at `addr`, until `stop`; what it sees goes to the returned list.
fn client<'s>(s: &'s std::thread::Scope<'s, '_>, addr: &str, stop: &'s AtomicBool) -> Seen {
    let seen: Seen = Arc::default();
    let mut w = Watch::new(addr, "pw");
    w.build = Some(stvtf::CURRENT_BUILD.to_string());
    w.snapshot_hz = 1000.0;
    w.duration = Some(LIMIT);
    let into = seen.clone();
    s.spawn(move || {
        watch::watch(&w, stop, &mut |l| into.lock().unwrap().push((Instant::now(), l))).unwrap();
    });
    seen
}

fn snapshots(seen: &Seen) -> Vec<(Instant, Snapshot)> {
    seen.lock()
        .unwrap()
        .iter()
        .filter_map(|(t, l)| match l {
            Line::Snapshot(s) => Some((*t, s.clone())),
            _ => None,
        })
        .collect()
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + LIMIT;
    while !done() {
        assert!(Instant::now() < until, "{what} did not happen");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Every snapshot the client got is the demo's at that tick.
fn assert_as_the_demo(got: &[(Instant, Snapshot)], demo: &BTreeMap<u32, Snapshot>) {
    let mut compared = 0;
    for (_, s) in got {
        let Some(d) = demo.get(&s.tick) else { continue };
        let (a, b) = (places(s), places(d));
        for (id, p) in &a {
            if let Some(q) = b.get(id) {
                // Positions are rounded to a tenth, from props of their own precision.
                let near = p.1.iter().zip(&q.1).all(|(x, y)| (x - y).abs() <= 0.15);
                assert!(p.0 == q.0 && near, "player {id} at tick {}: {p:?} vs {q:?}", s.tick);
                compared += 1;
            }
        }
    }
    assert!(compared > 100, "only {compared} players compared");
}

#[test]
fn a_client_joins_a_playing_demo_where_it_is() {
    let demo = demo_snapshots();
    let first = *demo.keys().next().unwrap();
    let playback = Playback::new(want(0, false, true));
    let got = playing(&playback, |addr| {
        // The demo plays on before anyone is in: 1.5 s at 4x is 6 s into it.
        std::thread::sleep(Duration::from_millis(1500));
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            let _stop = StopOnDrop(&stop);
            let seen = client(s, addr, &stop);
            wait_for("a second of the demo", || snapshots(&seen).len() > 66);
            stop.store(true, Ordering::Relaxed);
            // The round started before it joined: it is told, so a win panel from before goes.
            let lines = seen.lock().unwrap();
            assert!(lines.iter().any(|(_, l)| matches!(l, Line::RoundStart(_))), "the round's start");
            drop(lines);
            snapshots(&seen)
        })
    });
    let start = got[0].1.tick;
    let into = f64::from(start - first) * 0.015;
    assert!((5.0..12.0).contains(&into), "the client started {into:.1} s into the demo");
    assert_as_the_demo(&got, &demo);
}

#[test]
fn a_client_follows_a_jump_back_and_a_pause() {
    let demo = demo_snapshots();
    let first = *demo.keys().next().unwrap();
    let playback = Playback::new(want(first + 20 * 66, false, false));
    let got = playing(&playback, |addr| {
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            let _stop = StopOnDrop(&stop);
            let seen = client(s, addr, &stop);
            wait_for("a second of the demo", || snapshots(&seen).len() > 66);
            // Back to 5 s in.
            playback.set(want(first + 5 * 66, false, false));
            let jumped = Instant::now();
            wait_for("the jump", || snapshots(&seen).iter().any(|(t, s)| *t > jumped && s.tick < first + 10 * 66));
            wait_for("a second after the jump", || snapshots(&seen).iter().filter(|(t, _)| *t > jumped).count() > 66);
            // Paused where it is: nothing new comes.
            let at = snapshots(&seen).last().unwrap().1.tick;
            playback.set(want(at, true, false));
            std::thread::sleep(Duration::from_millis(300));
            let paused = Instant::now();
            std::thread::sleep(Duration::from_millis(700));
            let during = snapshots(&seen).iter().filter(|(t, _)| *t > paused).count();
            assert_eq!(during, 0, "frames while paused");
            playback.set(want(at, false, false));
            let resumed = Instant::now();
            wait_for("frames after the pause", || snapshots(&seen).iter().filter(|(t, _)| *t > resumed).count() > 30);
            stop.store(true, Ordering::Relaxed);
            let lines = seen.lock().unwrap();
            let fulls = lines.iter().filter(|(_, l)| matches!(l, Line::Status(Status::Full { .. }))).count();
            assert_eq!(fulls, 1, "a jump back is a full update of where it went: no new signon");
            drop(lines);
            snapshots(&seen)
        })
    });
    assert!(got[0].1.tick >= first + 19 * 66, "the client started at tick {}", got[0].1.tick);
    assert_as_the_demo(&got, &demo);
}

/// Snapshots the client got since `since`.
fn since(seen: &Seen, since: Instant) -> Vec<Snapshot> {
    snapshots(seen).into_iter().filter(|(t, _)| *t > since).map(|(_, s)| s).collect()
}

#[test]
fn at_the_end_of_a_demo_playback_rests() {
    let demo = demo_snapshots();
    let last = *demo.keys().next_back().unwrap();
    // Two seconds before the end at 4x: half a second of frames, then the end.
    let playback = Playback::new(want(last - 2 * 66, false, false));
    playing(&playback, |addr| {
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            let _stop = StopOnDrop(&stop);
            let seen = client(s, addr, &stop);
            wait_for("the last frame", || snapshots(&seen).iter().any(|(_, s)| s.tick == last));
            // Nothing more: no full update over and over once past the end.
            std::thread::sleep(Duration::from_millis(300));
            let rest = Instant::now();
            std::thread::sleep(Duration::from_millis(1500));
            assert_eq!(since(&seen, rest).len(), 0, "frames after the end");
        });
    });
}

#[test]
fn a_client_follows_a_jump_ahead() {
    let demo = demo_snapshots();
    let first = *demo.keys().next().unwrap();
    let playback = Playback::new(want(first + 5 * 66, false, false));
    let got = playing(&playback, |addr| {
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            let _stop = StopOnDrop(&stop);
            let seen = client(s, addr, &stop);
            wait_for("a second of the demo", || snapshots(&seen).len() > 66);
            let before = snapshots(&seen).iter().map(|(_, s)| s.tick).max().unwrap();
            playback.set(want(first + 100 * 66, false, false));
            let jumped = Instant::now();
            wait_for("the jump", || since(&seen, jumped).iter().any(|s| s.tick >= first + 100 * 66));
            wait_for("a second after it", || since(&seen, jumped).len() > 66);
            // Frames on their way when it jumped aside, nothing between there and the moment wanted.
            let between: Vec<u32> = since(&seen, jumped)
                .iter()
                .map(|s| s.tick)
                .filter(|t| *t > before + 66 && *t < first + 100 * 66)
                .collect();
            assert!(between.is_empty(), "nothing in between: {:?}", &between[..between.len().min(20)]);
            snapshots(&seen)
        })
    });
    assert_as_the_demo(&got, &demo);
}

#[test]
fn a_client_joining_while_paused_sees_the_moment_and_waits() {
    let demo = demo_snapshots();
    let first = *demo.keys().next().unwrap();
    let at = first + 30 * 66;
    let playback = Playback::new(want(at, true, false));
    playing(&playback, |addr| {
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            let _stop = StopOnDrop(&stop);
            let seen = client(s, addr, &stop);
            wait_for("the paused moment", || !snapshots(&seen).is_empty());
            let start = snapshots(&seen)[0].1.tick;
            assert!(start.abs_diff(at) < 66, "joined at {start}, paused at {at}");
            let waited = Instant::now();
            std::thread::sleep(Duration::from_millis(800));
            assert!(since(&seen, waited).iter().all(|s| s.tick == start), "frames while paused");
            playback.set(want(start, false, false));
            let resumed = Instant::now();
            wait_for("playback going on", || since(&seen, resumed).iter().any(|s| s.tick > start + 66));
        });
    });
}
