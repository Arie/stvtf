//! The relay end to end: it replays a recorded capture as its upstream, and stvtf connects to
//! it over UDP as any client would. What the client decodes must be what the capture holds.

mod common;

use common::{A2S_INFO, LIMIT, OOB, fixture, has_players, offered_build, tables, wait_for};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use stvtf::capture;
use stvtf::output::Status;
use stvtf::relay::{Relay, RelayConfig};
use stvtf::watch::{self, Watch, WatchError};
use stvtf::{Fatal, Line, Options};

/// Everything but the connection statuses, which differ (connecting, the reason for leaving).
fn data(lines: &[Line]) -> Vec<&Line> {
    lines.iter().filter(|l| !matches!(l, Line::Status(_))).collect()
}

/// One go of a client of a relay replaying a capture.
struct Attempt {
    speed: f64,
    lines: Vec<Line>,
    /// Why it could not even be compared.
    failed: Option<String>,
}

impl Attempt {
    /// The client's count of packets it never got, once it has disconnected.
    fn lost(&self) -> Option<u64> {
        self.lines.iter().find_map(|l| match l {
            Line::Status(Status::Disconnected { lost, .. }) => Some(*lost),
            _ => None,
        })
    }

    /// What went wrong against `want`, in a line: none if it all matches.
    fn difference(&self, want: &[&Line]) -> Option<String> {
        if let Some(failed) = &self.failed {
            return Some(failed.clone());
        }
        let got = data(&self.lines);
        if got == want {
            return None;
        }
        let at = got.iter().zip(want).position(|(g, w)| g != w).unwrap_or(got.len().min(want.len()));
        Some(format!(
            "{} of {} lines, {:?} packets lost, first difference at line {at}: got {}, wanted {}",
            got.len(),
            want.len(),
            self.lost(),
            brief(got.get(at).copied()),
            brief(want.get(at).copied()),
        ))
    }
}

/// A line's type and tick: a whole snapshot says too much to read.
fn brief(line: Option<&Line>) -> String {
    line.map_or("nothing".to_string(), |l| {
        let v = serde_json::to_value(l).unwrap();
        format!("{} at tick {}", v["type"].as_str().unwrap_or("?"), v["tick"])
    })
}

/// What a client of a relay replaying `records` at `speed` decodes.
fn through_relay(records: &[capture::Record<'_>], opt: &Options, speed: f64) -> Attempt {
    let relay = Relay::bind(&config("pw", 10)).unwrap();
    let listen = relay.local_addr().unwrap().to_string();
    let mut w = Watch::new(&listen, "pw");
    // No build given: the client asks the relay's A2S_INFO, through its challenge.
    w.snapshot_hz = opt.snapshot_hz;
    // A bound on a broken relay.
    w.duration = Some(Duration::from_secs(60));
    let (relay_stop, client_stop) = (AtomicBool::new(false), AtomicBool::new(false));
    let mut lines = Vec::new();
    let mut relay_lines = Vec::new();
    let (replay_result, watch_result) = std::thread::scope(|s| {
        let replaying = s.spawn(|| {
            let out = relay.replay(records, &tables(), speed, &relay_stop, &mut |l| relay_lines.push(l));
            client_stop.store(true, Ordering::Relaxed);
            out
        });
        let out = watch::watch(&w, &client_stop, &mut |l| lines.push(l));
        relay_stop.store(true, Ordering::Relaxed);
        (replaying.join().unwrap(), out)
    });
    let connecting = offered_build(&lines);
    let failed = if let Err(e) = replay_result {
        Some(format!("the relay failed: {e:#}"))
    } else if let Err(e) = watch_result {
        Some(format!("the client failed: {e}"))
    } else if relay_lines.first() != Some(&Line::Status(Status::Relay { listen })) {
        Some(format!("the relay's first line was {}", brief(relay_lines.first())))
    } else if connecting != Some(stvtf::CURRENT_BUILD) {
        Some(format!("the build from the relay's A2S_INFO was {connecting:?}"))
    } else {
        None
    };
    Attempt { speed, lines, failed }
}

/// The relay sends one reliable transfer at a time, each waiting for its ack, while datagrams go
/// out on the capture's clock. At a replay's speed, a slow or busy client sees reliable events
/// some ticks late, or overflows its socket's buffer and loses frames. So a difference only counts
/// if it stays at the slowest speed, where a client has plenty of time; a bug in the relay shows
/// at any speed. The failure message says what each speed got.
#[test]
fn a_client_of_the_relay_decodes_what_the_capture_holds() {
    let bytes = std::fs::read(fixture("bots_cp_badlands.cap")).unwrap();
    let records = capture::read(&bytes).unwrap();
    let opt = Options { snapshot_hz: 10.0, ..Options::default() };
    let want = common::replay("bots_cp_badlands.cap", &opt);
    let want = data(&want);

    let mut tried = Vec::new();
    // The capture runs 29 s: here 4 s, then 7 s, 15 s, and only after all of those fail, 29 s. On
    // an overloaded machine a reliable event was seen a tick late even at 2x.
    for speed in [8.0, 4.0, 2.0, 1.0] {
        let attempt = through_relay(&records, &opt, speed);
        let Some(difference) = attempt.difference(&want) else { return };
        tried.push(format!("at {}x: {difference}", attempt.speed));
    }
    panic!("no speed gave what the capture holds:\n{}", tried.join("\n"));
}

fn config(password: &str, slots: usize) -> RelayConfig {
    RelayConfig { password: password.into(), slots, ..RelayConfig::new("127.0.0.1:0") }
}

/// Runs a relay of `bots_cp_badlands.cap` while `client` runs against its address.
fn with_relay<T>(config: &RelayConfig, client: impl FnOnce(String) -> T) -> T {
    with_replay("bots_cp_badlands.cap", 8.0, config, client)
}

/// Runs a relay of capture `name` at `speed` while `client` runs against its address.
fn with_replay<T>(name: &str, speed: f64, config: &RelayConfig, client: impl FnOnce(String) -> T) -> T {
    let bytes = std::fs::read(fixture(name)).unwrap();
    let records = capture::read(&bytes).unwrap();
    let relay = Relay::bind(config).unwrap();
    let addr = relay.local_addr().unwrap().to_string();
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| relay.replay(&records, &tables(), speed, &stop, &mut |_| {}).unwrap());
        let out = client(addr);
        stop.store(true, Ordering::Relaxed);
        out
    })
}

/// A watch of a relay, of the current build, ended by its test or else by `LIMIT`.
fn bounded_watch(addr: &str, password: &str) -> Watch {
    let mut w = Watch::new(addr, password);
    w.build = Some(stvtf::CURRENT_BUILD.to_string());
    w.duration = Some(LIMIT);
    w
}

/// Runs until `done` says so, or `LIMIT`, and gives the lines seen.
fn watch_until(w: &Watch, mut done: impl FnMut(&Line) -> bool) -> Vec<Line> {
    let stop = AtomicBool::new(false);
    let mut lines = Vec::new();
    watch::watch(w, &stop, &mut |l| {
        stop.fetch_or(done(&l), Ordering::Relaxed);
        lines.push(l);
    })
    .unwrap();
    lines
}

fn watch_once(addr: &str, password: &str) -> Result<Vec<Line>, WatchError> {
    let mut lines = Vec::new();
    watch::watch(&bounded_watch(addr, password), &AtomicBool::new(false), &mut |l| lines.push(l)).map(|()| lines)
}

#[test]
fn a_wrong_password_is_refused_as_the_server_refuses_it() {
    let err = with_relay(&config("pw", 10), |addr| watch_once(&addr, "guess").unwrap_err());
    assert!(matches!(err, WatchError::Rejected { kind: Fatal::BadPassword, .. }), "{err}");
}

#[test]
fn a_full_relay_refuses_more_clients() {
    let in_game = AtomicBool::new(false);
    let refused = AtomicBool::new(false);
    let second = with_relay(&config("", 1), |addr| {
        std::thread::scope(|s| {
            s.spawn(|| {
                watch::watch(&bounded_watch(&addr, ""), &refused, &mut |l| {
                    in_game.fetch_or(matches!(l, Line::Status(Status::Full { .. })), Ordering::Relaxed);
                })
                .unwrap();
            });
            wait_for("the first client getting in", || in_game.load(Ordering::Relaxed));
            let lines =
                watch_until(&bounded_watch(&addr, ""), |l| matches!(l, Line::Status(Status::ConnectFailed { .. })));
            refused.store(true, Ordering::Relaxed);
            lines
        })
    });
    assert!(
        second.iter().any(|l| matches!(l, Line::Status(Status::ConnectFailed { reason })
            if reason.contains("ServerRejectServerFull"))),
        "{second:?}"
    );
}

/// Anyone can forge the source address of a UDP packet: a reply larger than the packet that
/// asked for it would let the relay amplify traffic at someone else.
#[test]
fn connectionless_replies_are_no_larger_than_their_queries_until_a_challenge_comes_back() {
    with_relay(&config("pw", 10), |addr| {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 2048];
        sock.send_to(A2S_INFO, &addr).unwrap();
        let n = sock.recv(&mut buf).unwrap();
        assert!(n <= A2S_INFO.len(), "A2S_INFO answered with {n} bytes");
        assert_eq!(buf[4], b'A', "a challenge first");
        // A connect with a made-up challenge gets nothing at all.
        let mut connect = [OOB.as_slice(), b"k"].concat();
        connect.extend_from_slice(&24i32.to_le_bytes());
        connect.extend_from_slice(&2u32.to_le_bytes());
        connect.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        connect.extend_from_slice(b"x\0pw\0");
        sock.send_to(&connect, &addr).unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        assert!(sock.recv(&mut buf).is_err(), "no reply to a forged connect");
    });
}

/// `connect --relay`: a watch passing its server on. Its upstream here is another relay, so
/// the second-hand client must get what a client of the first would.
#[test]
fn a_relay_can_relay_a_relay() {
    let client = with_relay(&config("up", 10), |upstream| {
        let middle = Relay::bind(&config("down", 10)).unwrap();
        let downstream = middle.local_addr().unwrap().to_string();
        let mut w = bounded_watch(&upstream, "up");
        w.snapshot_hz = 0.0;
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| middle.watch(&w, &stop, &mut |_| {}).unwrap());
            let lines = watch_until(&bounded_watch(&downstream, "down"), has_players);
            stop.store(true, Ordering::Relaxed);
            lines
        })
    });
    assert!(client.iter().any(has_players), "{:?}", &client[..client.len().min(5)]);
}

/// A map change restarts signon on the upstream connection, and so on every client's.
#[test]
fn a_client_of_the_relay_follows_a_map_change() {
    let lines = with_replay("bots_map_change.cap", 32.0, &config("", 10), |addr| {
        let (mut changed, mut full_again) = (false, false);
        watch_until(&bounded_watch(&addr, ""), |l| {
            changed |= matches!(l, Line::Status(Status::MapChange));
            full_again |= changed && matches!(l, Line::Status(Status::Full { .. }));
            full_again && has_players(l)
        })
    });
    let change = lines.iter().position(|l| matches!(l, Line::Status(Status::MapChange))).expect("a map change");
    let after = &lines[change..];
    assert!(after.iter().any(|l| matches!(l, Line::Status(Status::Full { .. }))), "signon again");
    assert!(after.iter().any(has_players), "and entities after it");
}
