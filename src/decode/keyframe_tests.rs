use std::collections::BTreeMap;

use super::decoder::Decoder;
use super::send_tables::Tables;
use crate::demo::{Next, Packet, Reader};
use crate::net::bits::{BitBuf, BitReader};
use crate::net::messages::{SVC_CREATESTRINGTABLE, SVC_UPDATESTRINGTABLE};
use crate::output::Line;

/// The demo's tables, signon streams and frames.
fn packets(path: &str) -> (Tables, Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let tables = Tables::from_demo(std::path::Path::new(&path)).unwrap();
    let mut r = Reader::new(std::fs::File::open(path).unwrap()).unwrap();
    let (mut signon, mut frames) = (Vec::new(), Vec::new());
    while let Next::Packet(p) = r.next_packet().unwrap() {
        match p {
            Packet::Signon { data, .. } => signon.push(data),
            Packet::Message { data, .. } => frames.push(data),
            _ => {}
        }
    }
    (tables, signon, frames)
}

/// What of a snapshot comes from the entities alone: the rest (scores, a dead player's last
/// place, respawn times, health, who left) is gathered from events and frames a late decoder
/// never saw. Players are by id, to compare those both know.
fn entities(lines: Vec<Line>) -> Vec<(u32, BTreeMap<u16, String>, serde_json::Value)> {
    lines
        .into_iter()
        .filter_map(|l| match l {
            Line::Snapshot(s) => Some(s),
            _ => None,
        })
        .map(|s| {
            let players = s.players.iter().map(|p| (p.id.0, format!("{:?} {:?}", p.class, p.team))).collect();
            (s.tick, players, serde_json::json!([s.buildings, s.projectiles, s.objectives]))
        })
        .collect()
}

fn assert_same_world(a: &Decoder, b: &Decoder, what: &str) {
    let (a, b) = (a.world_compared(), b.world_compared());
    assert_eq!(a.len(), b.len(), "{what}: entities");
    for (k, x) in &a {
        assert_eq!(Some(x), b.get(k), "{what}: entity {k}");
    }
}

#[test]
fn a_decoder_started_from_a_keyframe_follows_as_one_from_the_start() {
    // A live session recorded as a demo too: its deltas build on frames a few ticks back.
    let opt = crate::Options { snapshot_hz: 0.0, ..crate::Options::default() };
    let recorded = crate::demo::record_capture_for_tests("bots_cp_badlands.cap", &opt).1;
    let demos = ["pl_upward.dem", "ctf_turbine.dem", "pd_watergate.dem", "sd_doomsday.dem"]
        .map(|name| crate::testing::fixture(name).display().to_string())
        .into_iter()
        .chain([recorded.display().to_string()]);
    for demo in demos {
        follows_from_a_keyframe(&demo, 1, 2);
    }
}

/// A live recording of one's own, keyframes at several points.
#[test]
#[ignore = "a demo in STVTF_KEYFRAME_DEMO"]
fn a_decoder_follows_a_given_demo_from_keyframes() {
    let demo = std::env::var("STVTF_KEYFRAME_DEMO").unwrap();
    for at in 1..8 {
        follows_from_a_keyframe(&demo, at, 8);
    }
}

/// Starts a decoder from a keyframe `at`/`of` into `demo` and follows it to the end.
fn follows_from_a_keyframe(demo: &str, at: usize, of: usize) {
    {
        let (tables, signon, frames) = packets(demo);
        let mut whole = Decoder::new(&tables, 1000.0).unwrap();
        whole.keep_world();
        assert!(whole.keyframe(None).is_none(), "{demo}: nothing before the first full update");
        for s in &signon {
            whole.feed(s, s.len() * 8);
        }
        // The string table changes so far, as the relay's log keeps them for a late client.
        let half = frames.len() * at / of;
        let mut tables_since = Vec::new();
        let mut ticks = Vec::new();
        for f in &frames[..half] {
            let life = whole.feed(f, f.len() * 8);
            ticks.push(whole.server_tick());
            for span in
                life.spans.iter().filter(|s| matches!(u32::from(s.id), SVC_CREATESTRINGTABLE | SVC_UPDATESTRINGTABLE))
            {
                tables_since.push(BitReader::slice(f, span.bits.clone()));
            }
        }
        // Of the frame the next builds on.
        let (key_tick, key) = whole.keyframe(Some(&frames[half])).expect("a keyframe");
        assert!(whole.world_len() > 50, "{demo}: {} entities", whole.world_len());
        whole.take_output();

        let mut late = Decoder::new(&tables, 1000.0).unwrap();
        late.keep_world();
        for s in &signon {
            late.feed(s, s.len() * 8);
        }
        for t in &tables_since {
            late.feed(t.bytes(), t.nbits());
        }
        let life = late.feed(key.bytes(), key.nbits());
        assert!(life.full_update && !life.lost_frame, "{demo}");
        // Then the frames played since the one it is of, as the relay sends them with it.
        let recent: Vec<(u32, BitBuf)> =
            frames[..half].iter().zip(&ticks).map(|(f, t)| (*t, BitBuf::from_bytes(f.clone()))).collect();
        for f in whole.catch_up(key_tick, &recent) {
            let life = late.feed(f.bytes(), f.nbits());
            assert!(!life.lost_frame, "{demo}: a frame sent with the keyframe builds on one the client lacks");
        }
        // A demo's next frame builds on the last: the keyframe is of where `whole` is. (A live
        // session's builds on one a few ticks back, which the keyframe is of instead.)
        if whole.delta_base(&frames[half]) == Some(whole.server_tick()) {
            assert_same_world(&whole, &late, &format!("{demo} at the keyframe"));
        }
        late.take_output();

        for f in &frames[half..] {
            whole.feed(f, f.len() * 8);
            let life = late.feed(f, f.len() * 8);
            assert!(!life.lost_frame, "{demo}: every delta after builds on it");
        }
        assert_same_world(&whole, &late, &format!("{demo} at the end"));
        let (a, b) = (entities(whole.take_output()), entities(late.take_output()));
        assert!(a.len() > 10, "{demo}");
        assert_eq!(a.len(), b.len(), "{demo}");
        for ((tick, pa, xa), (_, pb, xb)) in a.iter().zip(&b) {
            assert_eq!(xa, xb, "{demo} at tick {tick}");
            for (id, p) in pa {
                if let Some(q) = pb.get(id) {
                    assert_eq!(p, q, "{demo} at tick {tick}: player {id}");
                }
            }
            assert!(pb.keys().filter(|id| pa.contains_key(id)).count() >= pa.len().min(4), "{demo}: players known");
        }
    }
}
