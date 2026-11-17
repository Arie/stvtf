use std::io::Cursor;

use super::header::Header;
use super::reader::{Next, Packet, Reader};
use super::voice::{Voiced, heard_at, with_voice};
use super::writer::Writer;
use crate::net::messages::SVC_CLASSINFO;

#[test]
fn a_demo_reads_as_its_packets() {
    let bytes = std::fs::read(crate::testing::fixture("pl_upward.dem")).unwrap();
    let mut r = Reader::new(Cursor::new(&bytes)).unwrap();
    assert_eq!(r.header.map, "pl_upward_f12");
    let (mut signon, mut messages, mut tables) = (0, 0, 0);
    loop {
        match r.next_packet().unwrap() {
            Next::Packet(Packet::Signon { .. }) => signon += 1,
            Next::Packet(Packet::Message { .. }) => messages += 1,
            Next::Packet(Packet::DataTables { .. }) => tables += 1,
            Next::Packet(_) => {}
            Next::Stop | Next::More => break,
        }
    }
    assert_eq!((signon, tables), (3, 1));
    assert!(messages > 100, "{messages}");
}

#[test]
fn a_demo_cut_short_waits_for_the_rest() {
    let bytes = std::fs::read(crate::testing::fixture("pl_upward.dem")).unwrap();
    // Cut a little way into the tenth packet.
    let mut whole = Reader::new(Cursor::new(&bytes)).unwrap();
    for _ in 0..9 {
        assert!(matches!(whole.next_packet().unwrap(), Next::Packet(_)));
    }
    let at = whole.pos;
    let tenth = whole.next_packet().unwrap();
    let mut r = Reader::new(Cursor::new(bytes[..at as usize + 10].to_vec())).unwrap();
    for _ in 0..9 {
        assert!(matches!(r.next_packet().unwrap(), Next::Packet(_)));
    }
    assert_eq!(r.next_packet().unwrap(), Next::More);
    assert_eq!(r.next_packet().unwrap(), Next::More, "and again");
    assert_eq!(r.pos, at, "it stays before the packet");
    // The rest arrives: reading goes on with that packet.
    let mut grown = Reader::new(Cursor::new(&bytes)).unwrap();
    grown.seek_to(r.pos).unwrap();
    assert_eq!(grown.next_packet().unwrap(), tenth);
}

#[test]
fn voice_is_written_into_a_demo_as_frames_of_its_own_at_their_ticks() {
    let header = Header {
        protocol: 24,
        server: "s".into(),
        client: "SourceTV Demo".into(),
        map: "cp_x".into(),
        game: "tf".into(),
        seconds: 0.0,
        ticks: 0,
        frames: 0,
        signon_bytes: 0,
    };
    let mut w = Writer::new(Cursor::new(Vec::new()), header, 0.015).unwrap();
    // As older recordings have it: the tables first. TF2 will not play that.
    w.data_tables(b"tables").unwrap();
    w.signon(b"one").unwrap();
    w.signon(&[SVC_CLASSINFO as u8, 1]).unwrap();
    for tick in [1000, 1004, 1010] {
        w.message(tick, b"game").unwrap();
    }
    let demo = w.finish().unwrap().into_inner();
    let said = |tick, slot, frame: u8| Voiced { tick, slot, account: 77, rate: 24000, frames: vec![vec![frame]] };
    // Out of order, before the first frame, and past the last: each lands where it can.
    let voices = [said(7, 3, 2), said(2, 3, 1), said(-5, 4, 9), said(99, 3, 3)];
    let out = with_voice(Cursor::new(&demo), &voices, Cursor::new(Vec::new())).unwrap().into_inner();
    let mut r = Reader::new(Cursor::new(&out)).unwrap();
    assert_eq!((r.header.frames, r.header.ticks, r.header.map.as_str()), (7, 10, "cp_x"));
    let (mut got, mut before) = (Vec::new(), Vec::new());
    while let Next::Packet(p) = r.next_packet().unwrap() {
        match p {
            Packet::Message { tick, data } => got.push((tick, data)),
            Packet::Signon { data, .. } | Packet::DataTables { data, .. } => before.push(data),
            _ => {}
        }
    }
    assert_eq!(
        before,
        [b"one".to_vec(), b"tables".to_vec(), vec![SVC_CLASSINFO as u8, 1]],
        "the tables before the class list"
    );
    // A speaker's frames are numbered on, in the order they are heard.
    let (mut three, mut four) = (0, 0);
    let mut voice = |slot, frame: u8| {
        let seq = if slot == 3 { &mut three } else { &mut four };
        crate::decode::steam_voice(slot, 77, 24000, &[vec![frame]], seq).unwrap().bytes().to_vec()
    };
    let game = b"game".to_vec();
    let expected = [
        (0, voice(4, 9)),
        (0, game.clone()),
        (2, voice(3, 1)),
        (4, game.clone()),
        (7, voice(3, 2)),
        (10, game),
        (10, voice(3, 3)),
    ];
    assert_eq!(got, expected);
    assert_eq!(three, 3);
}

#[test]
fn voice_is_heard_no_earlier_than_what_went_before_it_whatever_a_demos_ticks_say() {
    assert_eq!(heard_at(7, 4, 10), 7);
    assert_eq!(heard_at(2, 4, 10), 4);
    // A demo whose ticks go back: at the frame's, no panic.
    assert_eq!(heard_at(-5, 0, -1), -1);
}

#[test]
fn a_demo_still_being_written_gets_its_voice_and_an_end() {
    let mut w = Writer::new(Cursor::new(Vec::new()), Header { map: "cp_x".into(), ..blank() }, 0.015).unwrap();
    w.signon(b"one").unwrap();
    w.message(1000, b"game").unwrap();
    w.message(1004, b"game").unwrap();
    w.flush_packets().unwrap();
    // No stop packet, and the header's counts not written yet.
    let cut = w.w.into_inner();
    let voices = [Voiced { tick: 2, slot: 1, account: 5, rate: 24000, frames: vec![vec![1]] }];
    let out = with_voice(Cursor::new(&cut), &voices, Cursor::new(Vec::new())).unwrap().into_inner();
    let mut r = Reader::new(Cursor::new(&out)).unwrap();
    assert_eq!((r.header.frames, r.header.ticks), (3, 4), "counted as it is copied");
    assert!(r.header.signon_bytes > 0, "and the signon measured");
    let mut n = 0;
    let end = loop {
        match r.next_packet().unwrap() {
            Next::Packet(_) => n += 1,
            end => break end,
        }
    };
    assert_eq!((n, end), (5, Next::Stop), "signon, sync, two frames and the voice, then the end");
}

fn blank() -> Header {
    Header {
        protocol: 24,
        server: String::new(),
        client: String::new(),
        map: String::new(),
        game: "tf".into(),
        seconds: 0.0,
        ticks: 0,
        frames: 0,
        signon_bytes: 0,
    }
}

#[test]
fn a_written_demo_reads_back() {
    let header = Header {
        protocol: 24,
        server: "s".into(),
        client: "SourceTV Demo".into(),
        map: "cp_x".into(),
        game: "tf".into(),
        seconds: 0.0,
        ticks: 0,
        frames: 0,
        signon_bytes: 0,
    };
    let mut w = Writer::new(Cursor::new(Vec::new()), header, 0.015).unwrap();
    w.data_tables(b"tables").unwrap();
    w.signon(b"one").unwrap();
    w.message(1000, b"two").unwrap();
    w.message(1004, b"three").unwrap();
    let bytes = w.finish().unwrap().into_inner();
    let mut r = Reader::new(Cursor::new(&bytes)).unwrap();
    assert_eq!((r.header.frames, r.header.ticks), (2, 4));
    assert!(r.header.signon_bytes > 0);
    let mut got = Vec::new();
    let end = loop {
        match r.next_packet().unwrap() {
            Next::Packet(p) => got.push(p),
            end => break end,
        }
    };
    assert_eq!(end, Next::Stop);
    assert_eq!(
        got,
        [
            Packet::DataTables { tick: 0, data: b"tables".to_vec() },
            Packet::Signon { tick: 0, data: b"one".to_vec() },
            Packet::SyncTick { tick: 0 },
            Packet::Message { tick: 0, data: b"two".to_vec() },
            Packet::Message { tick: 4, data: b"three".to_vec() },
        ]
    );
}
