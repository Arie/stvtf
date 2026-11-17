use tf_demo_parser::demo::message::generated::SignOnState;

use super::decoder::{Decoder, SnapshotClock, UNPARSED, signon, snapshot_every};
use super::for_tests::tests_tables;
use super::send_tables::Tables;
use crate::net::messages::Signon;

#[test]
fn snapshot_timing() {
    let sent = |frames: &[u32], every| {
        let mut clock = SnapshotClock::default();
        frames.iter().copied().filter(|&t| clock.due(t, every)).collect::<Vec<_>>()
    };
    assert_eq!(sent(&[5, 6, 7, 8, 9, 10, 11], 3), [5, 8, 11]);
    let every_third: Vec<u32> = (1..=8).map(|n| n * 3).collect();
    assert_eq!(sent(&every_third, 4), [3, 9, 12, 15, 21, 24], "frames 3 ticks apart: one per 4 ticks");
    assert_eq!(sent(&[4, 8, 13, 17, 21, 25], 4), [4, 8, 13, 17, 21, 25], "every frame of a slower STV");
    assert_eq!(sent(&[5, 6, 100, 101, 103], 3), [5, 100, 103], "after a stall, counted from there");
    assert_eq!(sent(&[500, 501, 2, 3, 5], 3), [500, 2, 5], "tick restarted at a map load");
    assert_eq!(sent(&[u32::MAX - 1, u32::MAX], 3), [u32::MAX - 1, u32::MAX], "saturates near u32::MAX");
    assert_eq!(sent(&[5, 6], 0), [] as [u32; 0], "disabled");
    assert_eq!(snapshot_every(20.0, 0.015), 4, "16.7/s: never over the cap");
    assert_eq!(snapshot_every(33.32, 0.015), 3, "just over a whole tick ratio still rounds up");
    assert_eq!(snapshot_every(20.0, 1.0 / 128.0), 7, "128-tick server");
    assert_eq!(snapshot_every(64.0, 1.0 / 128.0), 2, "an exact divisor stays exact");
    assert_eq!(snapshot_every(1000.0, 0.015), 1, "every tick");
    assert_eq!(snapshot_every(0.0, 0.015), 0);
}

fn tables() -> Tables {
    tests_tables()
}

#[test]
fn tables_come_only_from_a_readable_demo() {
    assert!(Tables::from_demo(std::path::Path::new("/nonexistent.dem")).is_err());
    assert!(Tables::from_demo_bytes(b"not a demo").is_err());
}

/// Read from `g_SendTableCRC` in that build's `engine_srv.so`.
#[test]
fn the_crc_is_the_engines() {
    assert_eq!(tables().sendtable_crc(), 0x5FCD_4733);
    let full_demo = Tables::from_demo(&crate::testing::fixture("pl_upward.dem")).unwrap();
    assert_eq!(full_demo.sendtable_crc(), 0x5FCD_4733, "any demo of the build");
}

#[test]
fn a_message_that_does_not_parse_counts_and_asks_for_a_full_update() {
    let tables = tables();
    let mut dec = Decoder::new(&tables, 0.0).unwrap();
    let life = dec.feed(&[0xff; 64], 64 * 8);
    assert!(life.lost_frame);
    assert!(dec.parse_errors() >= 1);
    assert_eq!(life.spans.last().map(|s| s.id), Some(UNPARSED));
}

#[test]
fn too_few_bits_for_a_message_are_nothing() {
    let tables = tables();
    let mut dec = Decoder::new(&tables, 0.0).unwrap();
    let life = dec.feed(&[0xff], 3);
    assert!(!life.lost_frame && life.spans.is_empty());
    assert_eq!(dec.parse_errors(), 0);
}

/// The server tells us to leave in a message the parser has no type for.
#[test]
fn a_disconnect_gives_its_reason() {
    let tables = tables();
    let mut dec = Decoder::new(&tables, 0.0).unwrap();
    let bye = crate::net::messages::disconnect("Server shutting down");
    let life = dec.feed(bye.bytes(), bye.nbits());
    assert_eq!(life.disconnect.as_deref(), Some("Server shutting down"));
}

#[test]
fn signon_states_map_one_to_one() {
    let want = [
        (SignOnState::None, Signon::None),
        (SignOnState::Challenge, Signon::Challenge),
        (SignOnState::Connected, Signon::Connected),
        (SignOnState::New, Signon::New),
        (SignOnState::PreSpawn, Signon::PreSpawn),
        (SignOnState::Spawn, Signon::Spawn),
        (SignOnState::Full, Signon::Full),
        (SignOnState::ChangeLevel, Signon::ChangeLevel),
    ];
    for (state, signon_state) in want {
        assert_eq!(signon(&state), signon_state);
    }
}

/// What makes a session ask for a full update: nothing in a signon that was going well.
#[test]
fn a_delta_before_the_first_full_update_asks_for_nothing() {
    use crate::net::netchan::{self, Kind};
    use crate::net::receiver::Receiver;
    let bytes = std::fs::read(crate::testing::fixture("delta_before_full_update.cap")).unwrap();
    let records = crate::capture::read(&bytes).unwrap();
    let challenge = records[0].challenge().unwrap();
    let tables = tables();
    let mut dec = Decoder::new(&tables, 0.0).unwrap();
    let mut rx = Receiver::new(Some(challenge));
    let now = std::time::Instant::now();
    let mut full_updates = 0;
    for r in records.iter().skip(1).filter(|r| netchan::classify(r.data) != Kind::Oob) {
        let Some(got) = rx.feed(r.data, now) else { continue };
        for s in &got.streams {
            let life = dec.feed(s.bits.bytes(), s.bits.nbits());
            assert!(!life.lost_frame, "after {full_updates} full updates");
            full_updates += usize::from(life.full_update);
        }
    }
    assert_eq!(full_updates, 1);
}
