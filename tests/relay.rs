//! The relay end to end: it replays a recorded capture as its upstream, and stvtf connects to
//! it over UDP as any client would. What the client decodes must be what the capture holds.

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use stvtf::capture;
use stvtf::decode::Tables;
use stvtf::output::Status;
use stvtf::relay::{Relay, RelayConfig};
use stvtf::watch::{self, Watch, WatchError};
use stvtf::{Fatal, Line, Options};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn tables() -> Tables {
    Tables::from_demo_bytes(stvtf::builtin(stvtf::CURRENT_BUILD).unwrap().tables).unwrap()
}

/// Everything but the connection statuses, which differ (connecting, the reason for leaving).
fn data(lines: &[Line]) -> Vec<&Line> {
    lines.iter().filter(|l| !matches!(l, Line::Status(_))).collect()
}

#[test]
fn a_client_of_the_relay_decodes_what_the_capture_holds() {
    let bytes = std::fs::read(format!("{FIXTURES}/bots_cp_badlands.cap")).unwrap();
    let records = capture::read(&bytes).unwrap();
    let opt = Options { snapshot_hz: 10.0, ..Options::default() };
    let mut want = Vec::new();
    capture::replay(&records, &tables(), &opt, &mut |l| want.push(l)).unwrap();

    let relay = Relay::bind(&config("pw", 10)).unwrap();
    let mut w = Watch::new(&relay.local_addr().unwrap().to_string(), "pw");
    // No build given: the client asks the relay's A2S_INFO, through its challenge.
    w.snapshot_hz = opt.snapshot_hz;
    // A bound on a broken relay; the capture runs 29 s, here 4 s.
    w.duration = Some(Duration::from_secs(60));
    let (relay_stop, client_stop) = (AtomicBool::new(false), AtomicBool::new(false));
    let mut got = Vec::new();
    let mut relay_lines = Vec::new();
    std::thread::scope(|s| {
        s.spawn(|| {
            relay.replay(&records, &tables(), 8.0, &relay_stop, &mut |l| relay_lines.push(l)).unwrap();
            client_stop.store(true, Ordering::Relaxed);
        });
        watch::watch(&w, &client_stop, &mut |l| got.push(l)).unwrap();
    });
    let listen = relay.local_addr().unwrap().to_string();
    assert_eq!(relay_lines.first(), Some(&Line::Status(Status::Relay { listen })));
    let connecting = got.iter().find_map(|l| match l {
        Line::Status(Status::Connecting { build, .. }) => Some(build.as_str()),
        _ => None,
    });
    assert_eq!(connecting, Some(stvtf::CURRENT_BUILD), "the build, from the relay's A2S_INFO");
    let (got, want) = (data(&got), data(&want));
    for (n, (g, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(g, w, "line {n}");
    }
    assert_eq!(got.len(), want.len(), "number of lines");
}

fn config(password: &str, slots: usize) -> RelayConfig {
    RelayConfig { password: password.into(), slots, ..RelayConfig::new("127.0.0.1:0") }
}

/// Runs a relay of a capture while `client` runs against its address.
fn with_relay<T>(config: &RelayConfig, client: impl FnOnce(String) -> T) -> T {
    let bytes = std::fs::read(format!("{FIXTURES}/bots_cp_badlands.cap")).unwrap();
    let records = capture::read(&bytes).unwrap();
    let relay = Relay::bind(config).unwrap();
    let addr = relay.local_addr().unwrap().to_string();
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| relay.replay(&records, &tables(), 1.0, &stop, &mut |_| {}).unwrap());
        let out = client(addr);
        stop.store(true, Ordering::Relaxed);
        out
    })
}

fn watch_once(addr: &str, password: &str) -> Result<Vec<Line>, WatchError> {
    let mut w = Watch::new(addr, password);
    w.build = Some(stvtf::CURRENT_BUILD.to_string());
    w.duration = Some(Duration::from_secs(3));
    let mut lines = Vec::new();
    watch::watch(&w, &AtomicBool::new(false), &mut |l| lines.push(l)).map(|()| lines)
}

#[test]
fn a_wrong_password_is_refused_as_the_server_refuses_it() {
    let err = with_relay(&config("pw", 10), |addr| watch_once(&addr, "guess").unwrap_err());
    assert!(matches!(err, WatchError::Rejected { kind: Fatal::BadPassword, .. }), "{err}");
}

#[test]
fn a_full_relay_refuses_more_clients() {
    let lines = with_relay(&config("", 1), |addr| {
        std::thread::scope(|s| {
            let first = s.spawn(|| watch_once(&addr, "").unwrap());
            std::thread::sleep(Duration::from_millis(500));
            let second = watch_once(&addr, "").unwrap();
            assert!(
                second.iter().any(|l| matches!(l, Line::Status(Status::ConnectFailed { reason })
                    if reason.contains("ServerRejectServerFull"))),
                "{second:?}"
            );
            first.join().unwrap()
        })
    });
    assert!(lines.iter().any(|l| matches!(l, Line::Status(Status::Full { .. }))), "the first got in");
}

/// Anyone can forge the source address of a UDP packet: a reply larger than the packet that
/// asked for it would let the relay amplify traffic at someone else.
#[test]
fn connectionless_replies_are_no_larger_than_their_queries_until_a_challenge_comes_back() {
    with_relay(&config("pw", 10), |addr| {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 2048];
        let info = b"\xff\xff\xff\xffTSource Engine Query\0";
        sock.send_to(info, &addr).unwrap();
        let n = sock.recv(&mut buf).unwrap();
        assert!(n <= info.len(), "A2S_INFO answered with {n} bytes");
        assert_eq!(buf[4], b'A', "a challenge first");
        // A connect with a made-up challenge gets nothing at all.
        let mut connect = b"\xff\xff\xff\xffk".to_vec();
        connect.extend_from_slice(&24i32.to_le_bytes());
        connect.extend_from_slice(&2u32.to_le_bytes());
        connect.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        connect.extend_from_slice(b"x\0pw\0");
        sock.send_to(&connect, &addr).unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        assert!(sock.recv(&mut buf).is_err(), "no reply to a forged connect");
    });
}
