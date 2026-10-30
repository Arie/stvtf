//! Which build a watch offers: a fake `SourceTV` reports builds over `A2S_INFO` and refuses every
//! connect, as a server does a build not its own, and records the build each connect carried.

use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use stvtf::watch::{self, Watch};

const OOB: [u8; 4] = [0xff; 4];
const MAGIC_VERSION: u32 = 0x5A4F_4933;
const AUTH_HASHEDCDKEY: u32 = 2;
/// A bound on a watch that never connects as often as the test waits for.
const LIMIT: Duration = Duration::from_secs(30);

/// The server's `A2S_INFO` reply. `build` is unchecked: a forged reply can say anything.
fn info(build: &str) -> Vec<u8> {
    let mut v = OOB.to_vec();
    v.extend_from_slice(b"I\x11relay\0ctf_turbine\0tf\0Team Fortress\0");
    v.extend_from_slice(&440u16.to_le_bytes());
    v.extend_from_slice(&[0, 8, 0, b'p', b'l', 0, 0]);
    v.extend_from_slice(build.as_bytes());
    v.push(0);
    v
}

fn challenge(nonce: &[u8]) -> Vec<u8> {
    let mut v = OOB.to_vec();
    v.push(b'A');
    v.extend_from_slice(&MAGIC_VERSION.to_le_bytes());
    v.extend_from_slice(&7u32.to_le_bytes());
    v.extend_from_slice(nonce);
    v.extend_from_slice(&AUTH_HASHEDCDKEY.to_le_bytes());
    v
}

fn refusal(nonce: &[u8]) -> Vec<u8> {
    let mut v = OOB.to_vec();
    v.push(b'9');
    v.extend_from_slice(nonce);
    v.extend_from_slice(b"#GameUI_ServerRejectNewVersion\0");
    v
}

/// The nonce and the build string of a connect packet: after the kind come protocol, auth
/// protocol, challenge and nonce (4 bytes each), then name, password and build.
fn connect_fields(data: &[u8]) -> (&[u8], String) {
    const NONCE: usize = OOB.len() + 1 + 12;
    let mut strings = data[NONCE + 4..].split(|&b| b == 0);
    let build = strings.nth(2).unwrap();
    (&data[NONCE..NONCE + 4], String::from_utf8(build.to_vec()).unwrap())
}

/// Runs a watch against the fake until it has seen `connects` connects, and gives their builds.
/// The fake reports `reports[n]` after `n` connects, and the last of them from then on.
fn offered(reports: &[&str], connects: usize, setup: impl FnOnce(&mut Watch)) -> Vec<String> {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    let mut w = Watch::new(&sock.local_addr().unwrap().to_string(), "tv");
    w.duration = Some(LIMIT);
    setup(&mut w);
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        let server = s.spawn(|| {
            let mut seen = Vec::new();
            let mut buf = [0u8; 2048];
            let end = Instant::now() + LIMIT;
            while seen.len() < connects && Instant::now() < end {
                let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
                let data = &buf[..n];
                let reply = match data[OOB.len()] {
                    b'T' => info(reports[seen.len().min(reports.len() - 1)]),
                    b'q' => challenge(&data[OOB.len() + 1..OOB.len() + 5]),
                    b'k' => {
                        let (nonce, build) = connect_fields(data);
                        seen.push(build);
                        refusal(nonce)
                    }
                    kind => panic!("unexpected packet {:?}", char::from(kind)),
                };
                sock.send_to(&reply, from).unwrap();
            }
            stop.store(true, Ordering::Relaxed);
            seen
        });
        watch::watch(&w, &stop, &mut |_| {}).unwrap();
        server.join().unwrap()
    })
}

#[test]
fn offers_an_unknown_build_the_server_reports() {
    assert_eq!(offered(&["99999999"], 1, |_| {}), ["99999999"]);
}

#[test]
fn follows_a_server_that_updates_during_a_watch() {
    assert_eq!(offered(&["99999998", "99999999"], 2, |_| {}), ["99999998", "99999999"]);
}

#[test]
fn an_older_build_than_ours_is_offered_too() {
    assert_eq!(offered(&["1000000"], 1, |_| {}), ["1000000"]);
}

#[test]
fn an_implausible_report_offers_the_current_build() {
    assert_eq!(offered(&["nonsense\"; quit"], 1, |_| {}), [stvtf::CURRENT_BUILD]);
}

#[test]
fn an_implausible_report_keeps_the_last_good_build() {
    assert_eq!(offered(&["99999998", "nonsense"], 2, |_| {}), ["99999998", "99999998"]);
}

#[test]
fn an_explicit_build_wins() {
    let tables = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/tables_11138802.dem"));
    let got = offered(&["99999999"], 1, |w| {
        w.build = Some("1234567".to_string());
        w.crc = Some(1);
        w.tables = Some(tables);
    });
    assert_eq!(got, ["1234567"]);
}
