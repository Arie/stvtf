use std::sync::Arc;
use std::time::{Duration, Instant};

use tf_demo_parser::demo::gameevent_gen::GameEvent;
use tf_demo_parser::demo::message::Message;

use super::cursor::Cursor;
use super::player::Player;
use super::tests::{bench_with_one_in, clients_ack, relay_playing};
use super::tuning::NOT_A_FRAME;
use super::{Camera, Path, Step, Want};
use crate::decode::Entities;
use crate::net::receiver::Receiver;
use crate::relay::client::Phase;
use crate::relay::tests::Bench;

/// A camera on its way over a map's middle for `secs`, looking at one spot.
pub(super) fn across(secs: f32) -> Camera {
    let aim = [0.0, 0.0, 300.0];
    Camera::Along(Arc::new(Path {
        id: 1,
        began: Instant::now(),
        steps: vec![
            Step { secs: 0.0, pos: [-800.0, -600.0, 900.0], aim },
            Step { secs, pos: [800.0, -600.0, 1200.0], aim },
        ],
    }))
}

/// What a client makes of what the relay sends it.
struct Watcher {
    rx: Receiver,
    dec: Cursor,
    fulls: usize,
    deltas: usize,
    /// The players it was told are the cameraman, and how often it was sent into a player's eyes.
    men: Vec<u16>,
    chases: usize,
    /// The spots it was told to watch from: where, the pitch and the yaw.
    spots: Vec<([i32; 3], i16, i16)>,
}

impl Watcher {
    fn new(b: &Bench, demo: &std::path::Path) -> Self {
        b.client.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let dec = Cursor::open_detached(demo).unwrap();
        Watcher {
            rx: Receiver::new(Some(b.challenge)),
            dec,
            fulls: 0,
            deltas: 0,
            men: Vec::new(),
            chases: 0,
            spots: Vec::new(),
        }
    }

    /// Takes all it was sent since last time.
    fn take(&mut self, b: &Bench) {
        let mut buf = vec![0u8; crate::net::MAX_DATAGRAM];
        while let Ok(len) = b.client.recv(&mut buf) {
            let Some(pkt) = self.rx.feed(&buf[..len], Instant::now()) else { continue };
            for s in &pkt.streams {
                let life = self.dec.dec.feed(s.bits.bytes(), s.bits.nbits());
                assert!(!life.lost_frame, "a frame the client cannot take");
                match self.dec.dec.entities(s.bits.bytes()) {
                    Some(Entities::Full) => self.fulls += 1,
                    Some(Entities::Delta(_)) => self.deltas += 1,
                    None => {}
                }
            }
        }
    }

    /// Notes the cameras it is told to take in what the relay has for it reliably, in a transfer
    /// or waiting for one, as if all of it came through.
    fn hear(&mut self, relay: &crate::relay::Relay) {
        use tf_demo_parser::demo::message::MessageType;
        let state = self.dec.dec.parser_state_for_tests();
        for c in relay.hub().clients.values_mut() {
            let sent = c.outgoing.take().map(|o| o.data().to_vec());
            for bytes in sent.into_iter().chain(c.queue.drain(..).map(|m| m.bytes().to_vec())) {
                let mut read =
                    bitbuffer::BitReadStream::new(bitbuffer::BitReadBuffer::new(&bytes, bitbuffer::LittleEndian));
                while read.bits_left() > 6 {
                    let Ok(t) = bitbuffer::BitRead::read(&mut read) else { break };
                    let t: MessageType = t;
                    match Message::from_type(t, &mut read, state) {
                        Ok(Message::GameEvent(e)) => match e.event {
                            GameEvent::HLTVCameraman(m) => self.men.push(m.index),
                            GameEvent::HLTVChase(_) => self.chases += 1,
                            GameEvent::HLTVFixed(f) => self.spots.push((
                                [f.pos_x, f.pos_y, f.pos_z].map(u32::cast_signed),
                                f.theta.cast_signed(),
                                f.phi.cast_signed(),
                            )),
                            _ => {}
                        },
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            }
        }
    }

    /// Its entities are the relay's: the prop's place and turn as near as they are sent.
    fn has_what(&self, relay: &crate::decode::Decoder) {
        let (mut mine, mut theirs) = (self.dec.dec.world_compared(), relay.world_compared());
        let floats = |text: &str| -> Vec<f32> {
            text.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).filter_map(|n| n.parse().ok()).collect()
        };
        let ours = relay.rig_for_tests().map_or_default(|(prop, man)| vec![prop, man]);
        for (i, (_, _, props)) in &mut theirs {
            let Some((_, _, got)) = mine.get_mut(i).filter(|_| ours.contains(i)) else { continue };
            for ((_, sent), (_, read)) in props.iter_mut().zip(got) {
                let (a, b) = (floats(sent), floats(read));
                if sent.starts_with("Vector")
                    && a.len() == b.len()
                    && a.iter().zip(&b).all(|(a, b)| (a - b).abs() < 0.05)
                {
                    read.clone_from(sent);
                }
            }
        }
        assert_eq!(self.dec.dec.server_tick(), relay.server_tick());
        for (i, e) in &theirs {
            assert!(mine.get(i) == Some(e), "entity {i}: the client's is not the relay's (ours are {ours:?})");
        }
        assert_eq!(mine.len(), theirs.len(), "the client has none but the relay's");
    }

    /// Which of the relay's own entities it has: the prop, the cameraman.
    fn ours(&self, relay: &crate::decode::Decoder) -> [bool; 2] {
        let Some((prop, man)) = relay.rig_for_tests() else { return [false, false] };
        let has = self.dec.dec.world_dump();
        [prop, man].map(|i| has.contains_key(&i))
    }
}

/// `demo` played to the bench's client for `frames` frames more, as `want` has it.
fn play(p: &mut Player<'_>, relay: &crate::relay::Relay, want: &mut Want, frames: u32, client: &mut Watcher) {
    want.tick = p.cur.as_ref().unwrap().dec.server_tick() + frames;
    for _ in 0..frames * 2 {
        p.step(want).unwrap();
        clients_ack(relay);
        client.hear(relay);
    }
}

/// A room's camera on a path: the clients have a prop where it is, turned the way it looks, and
/// a player who watches from it, whom they are told is the cameraman; both gone again where it
/// stays, which is a spot they are told. A recorded demo's frames, and a live recording's that build on ones a few back.
#[test]
fn a_camera_on_a_path_is_a_prop_the_clients_cameraman_watches_from() {
    let opt = crate::Options { snapshot_hz: 0.0, ..crate::Options::default() };
    let live = crate::demo::record_capture_for_tests("bots_cp_badlands.cap", &opt).1;
    for demo in [crate::testing::fixture("pl_upward.dem"), live] {
        let mut c = Cursor::open_detached(&demo).unwrap();
        while c.recent.len() < 120 {
            match c.next_tick() {
                Some(NOT_A_FRAME) => c.skip_next(),
                Some(_) => drop(c.decode()),
                None => panic!("the demo ended"),
            }
        }
        let mut b = bench_with_one_in();
        b.client_state().set_phase(Phase::WaitingFull, Instant::now());
        let (relay, c, mut want) = relay_playing(&mut b, c, 0);
        let mut p = Player::new(&relay);
        p.cur = Some(c);
        let mut client = Watcher::new(&b, &demo);

        // On its way along two seconds of path.
        want.camera = Some(across(2.0));
        play(&mut p, &relay, &mut want, 60, &mut client);
        client.take(&b);
        let played = &p.cur.as_ref().unwrap().dec;
        let man = played.scene().cameraman().expect("a cameraman");
        assert_eq!(client.fulls, 1, "{}", demo.display());
        assert!(client.deltas >= 30, "{} deltas", client.deltas);
        client.has_what(played);
        assert_eq!(client.ours(played), [true, true]);
        assert_eq!(played.rig_for_tests().map(|(_, man)| man), Some(man));
        assert!(man <= u32::from(played.extractor().max_players()));
        assert!(!client.men.is_empty() && client.men.iter().all(|m| u32::from(*m) == man), "{:?}", client.men);
        let on_the_way = p.shot.pose.expect("a pose");
        assert!(on_the_way.pos[0] > -400.0 && on_the_way.pos[0] < 400.0 && client.spots.is_empty(), "{on_the_way:?}");

        // Past its end: the spot it ended at, in whole degrees, and the cameraman gone a moment
        // after the clients were told so. The stream's frames go as they are from there.
        let began = Instant::now();
        while p.shot.pose.is_some() {
            assert!(began.elapsed() < Duration::from_secs(10), "the cameraman stays");
            play(&mut p, &relay, &mut want, 30, &mut client);
            client.take(&b);
        }
        play(&mut p, &relay, &mut want, 10, &mut client);
        assert!(!p.cur.as_ref().unwrap().dec.scene().shot_touched());
        client.take(&b);
        assert!(began.elapsed() >= Duration::from_millis(400));
        assert_eq!(client.spots.last(), Some(&([800, -600, 1200], 42, 143)));
        assert_eq!(client.ours(&p.cur.as_ref().unwrap().dec), [false, false]);
        client.has_what(&p.cur.as_ref().unwrap().dec);

        // A camera that only stays is that at once, the room paused or not: no cameraman.
        let mut steps = |want: &Want, client: &mut Watcher| {
            let began = Instant::now();
            while began.elapsed() < Duration::from_millis(600) {
                p.step(want).unwrap();
                clients_ack(&relay);
                client.hear(&relay);
                client.take(&b);
            }
        };
        want.paused = true;
        steps(&want, &mut client);
        let fulls = client.fulls;
        let still = Step { secs: 0.0, pos: [100.0, 200.0, 800.0], aim: [100.0, 100.0, 800.0] };
        want.camera = Some(Camera::Along(Arc::new(Path { id: 2, began: Instant::now(), steps: vec![still] })));
        steps(&want, &mut client);
        assert_eq!((client.fulls, p.shot.pose), (fulls, None));
        assert_eq!(client.spots.last(), Some(&([100, 200, 800], 0, -90)));
        want.paused = false;
        play(&mut p, &relay, &mut want, 20, &mut client);
        client.take(&b);
        client.has_what(&p.cur.as_ref().unwrap().dec);

        // Let go: both gone, and the clients sent into a player's eyes.
        want.camera = None;
        play(&mut p, &relay, &mut want, 20, &mut client);
        client.take(&b);
        assert!(p.cur.as_ref().unwrap().dec.scene().cameraman().is_none());
        client.has_what(&p.cur.as_ref().unwrap().dec);
        assert_eq!(client.chases, 1);
    }
}
