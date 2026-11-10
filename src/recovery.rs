//! Recovery end to end. A client that misses a frame acks tick -1 until a full update comes, and
//! a relay asks its server on behalf of its clients. A replay of a capture cannot answer, so here
//! a fake server sends a capture's real packets, renumbered, and answers -1 by going back to the
//! capture's full update. Ticks go back with it, which is how a test sees that it came.

use crate::capture;
use crate::decode::{Decoder, Tables};
use crate::net::compress;
use crate::net::handshake::{self, FromClient};
use crate::net::netchan::{self, Kind};
use crate::net::receiver::Receiver;
use crate::net::split::SplitReassembler;
use crate::output::{Line, Status};
use crate::relay::{ClientMsg, Relay, RelayConfig, client_messages};
use crate::watch::{self, Watch};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// A bound on what should take moments.
const LIMIT: Duration = Duration::from_secs(30);
/// Time between packets, for a capture of a few seconds.
const PACE: Duration = Duration::from_millis(2);
/// -1 comes again every 100 ms until the full update is through: only the first gets an answer,
/// unless a busy machine takes longer than this to get through it.
const ANSWER_ONCE_PER: Duration = Duration::from_millis(500);
/// How many packets go missing: enough that no delta after them has its base.
const LOSS: usize = 20;
/// When helpers give up, should a failed test never tell them to stop: it must fail, not hang.
const HELPER_LIMIT: Duration = Duration::from_secs(70);
/// Snapshots to see after a full update before calling it recovered.
const SNAPSHOTS: usize = 5;

/// The build the fake server says it is: unknown, as any newer server's is.
const FAKE_BUILD: &str = "99999999";

/// A capture as plain channel packets.
struct Recording {
    challenge: u32,
    packets: Vec<Vec<u8>>,
    /// The first packet of the full update's reliable transfer.
    full_update: usize,
}

impl Recording {
    fn read() -> Self {
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/bots_cp_badlands.cap")).unwrap();
        let records = capture::read(&bytes).unwrap();
        let challenge = records[0].challenge().unwrap();
        let now = Instant::now();
        let mut splits = SplitReassembler::default();
        let mut packets = Vec::new();
        for r in records.iter().skip(1).filter(|r| r.kind == capture::Kind::Datagram as u8) {
            let mut d = r.data.to_vec();
            if netchan::classify(&d) == Kind::Split {
                match splits.feed(&d, now) {
                    Some(joined) => d = joined,
                    None => continue,
                }
            }
            if netchan::classify(&d) == Kind::Compressed {
                let block = netchan::SEQUENCE_LEN + compress::SNAPPY_MAGIC.len();
                d = compress::snappy(&d[block..]).unwrap();
            }
            packets.push(d);
        }

        let tables = tables();
        let mut decoder = Decoder::new(&tables, 0.0).unwrap();
        let mut rx = Receiver::new(Some(challenge));
        let complete = packets
            .iter()
            .position(|p| {
                let got = rx.feed(p, now).unwrap();
                got.streams.iter().any(|s| decoder.feed(s.bits.bytes(), s.bits.nbits()).full_update)
            })
            .expect("a full update in the capture");
        let reliable = |p: &[u8]| netchan::decode_header(p).unwrap().reliable();
        let full_update =
            packets[..complete].iter().rposition(|p| !reliable(p)).expect("something before the transfer") + 1;
        assert!(complete - full_update > 1, "a transfer of several packets");
        Recording { challenge, packets, full_update }
    }
}

fn tables() -> Tables {
    Tables::from_demo_bytes(crate::builtin(crate::CURRENT_BUILD).unwrap().tables).unwrap()
}

/// A server playing a recording, which goes back to its full update when asked for one.
struct FakeServer {
    sock: UdpSocket,
    recording: Recording,
    /// Packets still to leave out; a test sets it when it is ready for the loss.
    lose: AtomicUsize,
    /// The packet to send next.
    position: AtomicUsize,
    /// Full updates sent because a client asked.
    answered: AtomicUsize,
    stop: AtomicBool,
}

impl FakeServer {
    fn new() -> Self {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(1))).unwrap();
        FakeServer {
            sock,
            recording: Recording::read(),
            lose: AtomicUsize::new(0),
            position: AtomicUsize::new(0),
            answered: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        }
    }

    fn addr(&self) -> SocketAddr {
        self.sock.local_addr().unwrap()
    }

    fn run(&self) {
        let rec = &self.recording;
        let mut rx = Receiver::new(Some(rec.challenge));
        let mut buf = vec![0u8; crate::net::MAX_DATAGRAM];
        let mut client = None;
        let mut sequence = 1;
        let mut next_send = Instant::now();
        let mut last_answer: Option<Instant> = None;
        let started = Instant::now();
        while !self.stop.load(Ordering::Relaxed) && started.elapsed() < HELPER_LIMIT {
            if let Ok((n, from)) = self.sock.recv_from(&mut buf) {
                let data = &buf[..n];
                if netchan::classify(data) == Kind::Oob {
                    let reply = match handshake::parse_from_client(data) {
                        Some(FromClient::GetChallenge { nonce }) => handshake::build_challenge(rec.challenge, nonce),
                        Some(FromClient::Connect { nonce, .. }) => {
                            client = Some(from);
                            handshake::build_accept(nonce)
                        }
                        Some(FromClient::Info { .. }) => handshake::build_info(&handshake::Info {
                            name: "fake",
                            map: "cp_badlands",
                            players: 0,
                            max_players: 8,
                            build: FAKE_BUILD,
                        }),
                        None => continue,
                    };
                    self.sock.send_to(&reply, from).unwrap();
                } else if let Some(got) = rx.feed(data, Instant::now()) {
                    let asked = got.streams.iter().any(|s| client_messages(&s.bits).contains(&ClientMsg::Tick(-1)));
                    if asked
                        && self.position.load(Ordering::Relaxed) > rec.full_update
                        && last_answer.is_none_or(|t| t.elapsed() > ANSWER_ONCE_PER)
                    {
                        last_answer = Some(Instant::now());
                        self.position.store(rec.full_update, Ordering::Relaxed);
                        self.answered.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            let (Some(client), true) = (client, Instant::now() >= next_send) else { continue };
            next_send = Instant::now() + PACE;
            let position = self.position.load(Ordering::Relaxed);
            let Some(packet) = rec.packets.get(position) else { continue };
            self.position.store(position + 1, Ordering::Relaxed);
            sequence += 1;
            if self.lose.load(Ordering::Relaxed) > 0 {
                self.lose.fetch_sub(1, Ordering::Relaxed);
                continue;
            }
            let mut packet = packet.clone();
            packet[..netchan::SEQUENCE_LEN].copy_from_slice(&i32::to_le_bytes(sequence));
            netchan::reseal(&mut packet);
            self.sock.send_to(&packet, client).unwrap();
        }
    }
}

/// A connection between a client and a relay, on which a test can lose what goes to the client.
struct LossyLink {
    front: UdpSocket,
    back: UdpSocket,
    lose: AtomicUsize,
}

impl LossyLink {
    fn new(to: SocketAddr) -> Self {
        let front = UdpSocket::bind("127.0.0.1:0").unwrap();
        let back = UdpSocket::bind("127.0.0.1:0").unwrap();
        back.connect(to).unwrap();
        for sock in [&front, &back] {
            sock.set_read_timeout(Some(Duration::from_millis(5))).unwrap();
        }
        LossyLink { front, back, lose: AtomicUsize::new(0) }
    }

    fn addr(&self) -> SocketAddr {
        self.front.local_addr().unwrap()
    }

    /// Passes packets on both ways until `stop`.
    fn run(&self, stop: &AtomicBool) {
        let client = std::sync::Mutex::new(None);
        let started = Instant::now();
        let running = || !stop.load(Ordering::Relaxed) && started.elapsed() < HELPER_LIMIT;
        std::thread::scope(|s| {
            s.spawn(|| {
                let mut buf = vec![0u8; crate::net::MAX_DATAGRAM];
                while running() {
                    if let Ok((n, from)) = self.front.recv_from(&mut buf) {
                        *client.lock().unwrap() = Some(from);
                        let _ = self.back.send(&buf[..n]);
                    }
                }
            });
            let mut buf = vec![0u8; crate::net::MAX_DATAGRAM];
            while running() {
                let Ok(n) = self.back.recv(&mut buf) else { continue };
                if self.lose.load(Ordering::Relaxed) > 0 {
                    self.lose.fetch_sub(1, Ordering::Relaxed);
                } else if let Some(to) = *client.lock().unwrap() {
                    let _ = self.front.send_to(&buf[..n], to);
                }
            }
        });
    }
}

fn watch_of(target: SocketAddr) -> Watch {
    let mut w = Watch::new(&target.to_string(), "");
    w.build = Some(crate::CURRENT_BUILD.to_string());
    w.duration = Some(LIMIT);
    w
}

/// A client until `done`, with `in_game` called on the first snapshot with players.
fn client_until(w: &Watch, in_game: impl FnOnce(), mut done: impl FnMut(&Line) -> bool) -> Vec<Line> {
    let stop = AtomicBool::new(false);
    let mut in_game = Some(in_game);
    let mut lines = Vec::new();
    watch::watch(w, &stop, &mut |line| {
        if matches!(&line, Line::Snapshot(s) if !s.players.is_empty())
            && let Some(f) = in_game.take()
        {
            f();
        }
        if done(&line) {
            stop.store(true, Ordering::Relaxed);
        }
        lines.push(line);
    })
    .unwrap();
    lines
}

/// True once the ticks went back, which only a full update does here, and entities came after it.
fn went_back_and_on() -> impl FnMut(&Line) -> bool {
    let (mut latest, mut after) = (0, 0);
    move |line| {
        if let Line::Snapshot(s) = line
            && !s.players.is_empty()
        {
            if after > 0 || s.tick < latest {
                after += 1;
            }
            latest = latest.max(s.tick);
        }
        after >= SNAPSHOTS
    }
}

/// A client also ends at `LIMIT`, recovered or not.
#[track_caller]
fn assert_recovered(lines: &[Line]) {
    assert!(lines.iter().any(went_back_and_on()), "no full update came: {} lines", lines.len());
}

fn lost(lines: &[Line]) -> u64 {
    lines
        .iter()
        .find_map(|l| match l {
            Line::Status(Status::Disconnected { lost, .. }) => Some(*lost),
            _ => None,
        })
        .expect("the client disconnects")
}

/// The test of the tests: what recovers below must be recovering from something.
#[test]
fn without_loss_nobody_asks_for_a_full_update() {
    let server = FakeServer::new();
    let w = watch_of(server.addr());
    let lines = std::thread::scope(|s| {
        s.spawn(|| server.run());
        let mut snapshots = 0;
        let lines = client_until(
            &w,
            || {},
            |l| {
                snapshots += usize::from(matches!(l, Line::Snapshot(s) if !s.players.is_empty()));
                snapshots >= 40
            },
        );
        server.stop.store(true, Ordering::Relaxed);
        lines
    });
    assert_eq!(lost(&lines), 0);
    assert_eq!(server.answered.load(Ordering::Relaxed), 0);
}

#[test]
fn a_client_that_loses_frames_gets_a_full_update_and_goes_on() {
    let server = FakeServer::new();
    let w = watch_of(server.addr());
    let lines = std::thread::scope(|s| {
        s.spawn(|| server.run());
        let lines = client_until(&w, || server.lose.store(LOSS, Ordering::Relaxed), went_back_and_on());
        server.stop.store(true, Ordering::Relaxed);
        lines
    });
    assert_recovered(&lines);
    assert!(lost(&lines) >= LOSS as u64 / 2, "{}", lost(&lines));
    assert!(server.answered.load(Ordering::Relaxed) >= 1, "someone asked the server");
}

/// Serves a relay of `server` while `client` runs against the relay.
fn with_relay_of<T>(server: &FakeServer, client: impl FnOnce(SocketAddr, &Relay) -> T) -> T {
    let relay = Relay::bind(&RelayConfig { password: String::new(), ..RelayConfig::new("127.0.0.1:0") }).unwrap();
    let mut w = watch_of(server.addr());
    w.snapshot_hz = 0.0;
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| server.run());
        s.spawn(|| relay.watch(&w, &stop, &mut |_| {}).unwrap());
        let out = client(relay.local_addr().unwrap(), &relay);
        stop.store(true, Ordering::Relaxed);
        server.stop.store(true, Ordering::Relaxed);
        out
    })
}

/// Lost on the way from the relay: the server knows of no loss, and the relay must ask for it.
#[test]
fn a_client_losing_frames_from_a_relay_gets_a_full_update_through_it() {
    let server = FakeServer::new();
    let lines = with_relay_of(&server, |relay, _| {
        let link = LossyLink::new(relay);
        let stop = AtomicBool::new(false);
        let w = watch_of(link.addr());
        std::thread::scope(|s| {
            s.spawn(|| link.run(&stop));
            let lines = client_until(&w, || link.lose.store(LOSS, Ordering::Relaxed), went_back_and_on());
            stop.store(true, Ordering::Relaxed);
            lines
        })
    });
    assert_recovered(&lines);
    assert!(lost(&lines) >= LOSS as u64 / 2, "{}", lost(&lines));
    assert!(server.answered.load(Ordering::Relaxed) >= 1, "someone asked the server");
}

/// Lost on the way to the relay, so to everyone: what the relay asks for reaches the client too.
/// The relay numbers its own packets, so the client has no packet to call lost.
#[test]
fn frames_lost_on_the_way_to_a_relay_are_made_up_for_its_clients() {
    let server = FakeServer::new();
    let lines = with_relay_of(&server, |relay, _| {
        client_until(&watch_of(relay), || server.lose.store(LOSS, Ordering::Relaxed), went_back_and_on())
    });
    assert_recovered(&lines);
    assert_eq!(lost(&lines), 0);
    assert!(server.answered.load(Ordering::Relaxed) >= 1, "someone asked the server");
}

/// Long after the full update, what the relay kept is gone: a new client needs another.
#[test]
fn a_client_joining_a_relay_late_gets_a_full_update_asked_for_it() {
    let server = FakeServer::new();
    let late = 1000;
    let (joined_late, lines) = with_relay_of(&server, |relay, _| {
        let limit = Instant::now() + LIMIT;
        while server.position.load(Ordering::Relaxed) < late && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(5));
        }
        let joined_late = server.position.load(Ordering::Relaxed) >= late;
        (
            joined_late,
            client_until(&watch_of(relay), || {}, |l| matches!(l, Line::Snapshot(s) if !s.players.is_empty())),
        )
    });
    assert!(joined_late, "the relay never connected");
    let players = |l: &Line| matches!(l, Line::Snapshot(s) if !s.players.is_empty());
    assert!(lines.iter().any(players), "entities through a full update");
    assert!(server.answered.load(Ordering::Relaxed) >= 1, "someone asked the server");
}

/// The relay must say what its server says, not what stvtf was built for: found on a live server
/// of an older build. A client deciding by it, or a relay of this relay, would offer the wrong one.
#[test]
fn a_relay_reports_the_build_of_its_server() {
    let server = FakeServer::new();
    let relay = Relay::bind(&RelayConfig { password: String::new(), ..RelayConfig::new("127.0.0.1:0") }).unwrap();
    let addr = relay.local_addr().unwrap();
    let mut upstream = watch_of(server.addr());
    upstream.build = None;
    upstream.snapshot_hz = 0.0;
    let stop = AtomicBool::new(false);
    let lines = std::thread::scope(|s| {
        s.spawn(|| server.run());
        s.spawn(|| relay.watch(&upstream, &stop, &mut |_| {}).unwrap());
        let limit = Instant::now() + LIMIT;
        while server.position.load(Ordering::Relaxed) == 0 && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut client = watch_of(addr);
        client.build = None;
        let lines = client_until(&client, || {}, |l| matches!(l, Line::Status(Status::Connecting { .. })));
        stop.store(true, Ordering::Relaxed);
        server.stop.store(true, Ordering::Relaxed);
        lines
    });
    let offered = lines.iter().find_map(|l| match l {
        Line::Status(Status::Connecting { build, .. }) => Some(build.as_str()),
        _ => None,
    });
    assert_eq!(offered, Some(FAKE_BUILD));
}
