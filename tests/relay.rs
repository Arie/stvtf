//! The relay end to end: it replays a recorded capture as its upstream, and stvtf connects to
//! it over UDP as any client would. What the client decodes must be what the capture holds.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use stvtf::capture;
use stvtf::decode::Tables;
use stvtf::relay::Relay;
use stvtf::watch::{self, Watch};
use stvtf::{Line, Options};

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

    let relay = Relay::bind("127.0.0.1:0").unwrap();
    let mut w = Watch::new(&relay.local_addr().unwrap().to_string(), "");
    w.build = Some(stvtf::CURRENT_BUILD.to_string());
    w.snapshot_hz = opt.snapshot_hz;
    // A bound on a broken relay; the capture runs 29 s, here 4 s.
    w.duration = Some(Duration::from_secs(60));
    let (relay_stop, client_stop) = (AtomicBool::new(false), AtomicBool::new(false));
    let mut got = Vec::new();
    std::thread::scope(|s| {
        s.spawn(|| {
            relay.replay(&records, &tables(), 8.0, &relay_stop, &mut |_| {}).unwrap();
            client_stop.store(true, Ordering::Relaxed);
        });
        watch::watch(&w, &client_stop, &mut |l| got.push(l)).unwrap();
    });
    let (got, want) = (data(&got), data(&want));
    for (n, (g, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(g, w, "line {n}");
    }
    assert_eq!(got.len(), want.len(), "number of lines");
}
