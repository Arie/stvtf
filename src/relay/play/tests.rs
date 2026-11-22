use std::time::{Duration, Instant};

use tf_demo_parser::demo::gameevent_gen::GameEvent;
use tf_demo_parser::demo::message::Message;

use super::beams::past_beams;
use super::cursor::{Cursor, without_cues};
use super::index::Index;
use super::playback::{Playback, Want};
use super::player::Player;
use super::tuning::{FRAME_WAIT, NOT_A_FRAME};
use crate::decode::beams::Beam;
use crate::decode::{BeamModel, Entities, ModelsSent, Span};
use crate::demo::Packet;
use crate::net::bits::{BitBuf, BitWriter};
use crate::net::messages::{self, SVC_GAMEEVENT, SVC_TEMPENTITIES, SVC_USERMESSAGE, Signon, TYPE_BITS};
use crate::net::receiver::Receiver;
use crate::relay::Relay;
use crate::relay::client::Phase;
use crate::relay::hub::Part;
use crate::relay::server_msg::vgui_menu;
use crate::relay::tests::Bench;

/// `pl_upward.dem` decoded until `done`.
pub(super) fn upward_until(done: impl Fn(&Cursor) -> bool) -> Cursor {
    let path = crate::testing::fixture("pl_upward.dem");
    let mut c = Cursor::open_detached(&path).unwrap();
    while !done(&c) {
        match c.next_tick() {
            None => panic!("the demo ended"),
            Some(NOT_A_FRAME) => c.skip_next(),
            Some(_) => drop(c.decode()),
        }
    }
    c
}

/// `pl_upward.dem` a few frames in, with the next one read as the server's changelevel would
/// have it: `net_SignonState(CHANGELEVEL)` and a panel shown put in after its first message.
/// What the frame's message types are without the changelevel, too.
fn upward_with_changelevel() -> (Cursor, Vec<u8>) {
    let (mut c, mut copy) = (upward_until(four_frames_kept), upward_until(four_frames_kept));
    let Some(Packet::Message { tick, .. }) = copy.peek().cloned() else { panic!("a frame next") };
    let (bits, life) = copy.decode().unwrap();
    let mut w = BitWriter::new();
    let mut ids = Vec::new();
    for (i, span) in life.spans.iter().enumerate() {
        w.write_range(bits.bytes(), span.bits.clone());
        ids.push(span.id);
        if i == 0 {
            w.write_buf(&messages::signonstate(Signon::ChangeLevel, 3));
            w.write_buf(&vgui_menu(crate::relay::tuning::SPECTATORS, true));
            ids.push(u8::try_from(SVC_USERMESSAGE).unwrap());
        }
    }
    c.peek();
    c.next = Some(Packet::Message { tick, data: w.into_bytes() });
    (c, ids)
}

pub(super) fn four_frames_kept(c: &Cursor) -> bool {
    c.recent.len() >= 4
}

/// The message types of each stream the bench's client is sent without a transfer, as read by
/// `reader`.
fn sent_unreliably(b: &Bench, reader: &mut Cursor) -> Vec<Vec<u8>> {
    b.client.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let mut rx = Receiver::new(Some(b.challenge));
    let mut buf = vec![0u8; crate::net::MAX_DATAGRAM];
    let mut sent = Vec::new();
    while let Ok(n) = b.client.recv(&mut buf) {
        let Some(pkt) = rx.feed(&buf[..n], Instant::now()) else { continue };
        for s in pkt.streams.iter().filter(|s| !s.reliable) {
            sent.push(reader.dec.feed(s.bits.bytes(), s.bits.nbits()).spans.iter().map(|s| s.id).collect());
        }
    }
    sent
}

/// The bench's client, in the game.
pub(super) fn bench_with_one_in() -> Bench {
    let mut b = Bench::new("", 1);
    b.connect();
    let c = b.client_state();
    c.gate = None;
    c.set_phase(Phase::Active, Instant::now());
    b
}

/// The frames the bench's client is sent, as a game client keeps them: it drops those older than
/// the base of each delta, and all on a full update. Panics on a delta from a frame it lacks. How
/// many full updates, and deltas.
fn follow_as_a_game_client(b: &Bench, reader: &Cursor) -> (usize, usize) {
    b.client.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let mut rx = Receiver::new(Some(b.challenge));
    let mut buf = vec![0u8; crate::net::MAX_DATAGRAM];
    let mut frames = std::collections::BTreeSet::new();
    let (mut fulls, mut deltas) = (0, 0);
    while let Ok(len) = b.client.recv(&mut buf) {
        let Some(pkt) = rx.feed(&buf[..len], Instant::now()) else { continue };
        for s in pkt.streams.iter().filter(|s| !s.reliable) {
            let Some(tick) = reader.dec.frame_tick(s.bits.bytes()) else { continue };
            match reader.dec.entities(s.bits.bytes()) {
                None => continue,
                Some(Entities::Full) => {
                    frames.clear();
                    fulls += 1;
                }
                Some(Entities::Delta(base)) => {
                    assert!(frames.contains(&base), "a delta to {tick} from {base}, the client has {frames:?}");
                    frames.retain(|t| *t >= base);
                    deltas += 1;
                }
            }
            frames.insert(tick);
        }
    }
    (fulls, deltas)
}

/// A relay with the bench's hub, its client in the game, and a player of `c` toward `tick`, at
/// speed 0: the tick stays.
pub(super) fn relay_playing(b: &mut Bench, c: Cursor, tick: u32) -> (Relay, Cursor, Want) {
    let config = crate::relay::RelayConfig::new("127.0.0.1:0");
    let spare = crate::relay::hub::Hub::new(std::net::UdpSocket::bind("127.0.0.1:0").unwrap(), &config);
    let hub = std::mem::replace(&mut b.hub, spare);
    let relay = Relay {
        sock: std::net::UdpSocket::bind("127.0.0.1:0").unwrap(),
        port: 0,
        hub: hub.into(),
        taken_over: None.into(),
    };
    let want = Want {
        demo: c.path.clone(),
        tick,
        at: Instant::now(),
        paused: false,
        speed: 0.0,
        from_start: false,
        camera: None,
    };
    (relay, c, want)
}

/// The clients ack the last frame they were sent, as a game client that got it does.
pub(super) fn clients_ack(relay: &Relay) {
    for c in relay.hub().clients.values_mut() {
        if let Some(last) = c.frames.back().copied() {
            (c.acked, c.keyed) = (Some(last), None);
        }
    }
}

/// The string tables a client given the hub's backlog has are those of `p`'s demo where it is;
/// None when they are not.
fn client_tables_are_the_demos(relay: &Relay, p: &Player<'_>, signon: usize) -> Option<()> {
    let c = p.cur.as_ref().unwrap();
    let mut client = Cursor::open_detached(&c.path).unwrap();
    let hub = relay.hub();
    for i in signon..hub.backlog.len() {
        let m = &hub.backlog.get(i).unwrap().bits;
        client.dec.feed(m.bytes(), m.nbits());
    }
    c.dec.set_tables_back(&client.dec).filter(Vec::is_empty).map(drop)
}

/// A game client's `table` from the backlog at `signon`, if it differs from the demo's.
fn game_client_table_differs(relay: &Relay, player: &Player<'_>, signon: usize, table: &str) -> Option<String> {
    use bitbuffer::{BitRead, BitReadBuffer, BitReadStream, LittleEndian};
    use tf_demo_parser::demo::message::MessageType;
    let cur = player.cur.as_ref().unwrap();
    let state = cur.dec.parser_state_for_tests();
    let hub = relay.hub();
    let mut names: Vec<Vec<Option<String>>> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    let mut misplaced = Vec::new();
    for i in signon..hub.backlog.len() {
        let bits = &hub.backlog.get(i).unwrap().bits;
        let mut stream = BitReadStream::new(BitReadBuffer::new(bits.bytes(), LittleEndian));
        let Ok(kind) = MessageType::read(&mut stream) else { continue };
        match Message::from_type(kind, &mut stream, state) {
            Ok(Message::ServerInfo(_)) => {
                names.clear();
                ids.clear();
                misplaced.clear();
            }
            Ok(Message::CreateStringTable(created)) => {
                ids.push(created.table.name.to_string());
                names.push(created.table.entries.iter().map(|(_, e)| e.text.as_deref().map(str::to_string)).collect());
            }
            Ok(Message::UpdateStringTable(update)) => {
                let id = usize::from(update.table_id);
                let Some(have) = names.get_mut(id) else { continue };
                for (index, entry) in &update.entries {
                    if usize::from(*index) < have.len() {
                        continue;
                    }
                    if usize::from(*index) != have.len() {
                        misplaced.push((ids[id].clone(), *index, have.len()));
                    }
                    have.push(entry.text.as_deref().map(str::to_string));
                }
            }
            _ => {}
        }
    }
    let client: std::collections::BTreeMap<u16, Option<String>> =
        ids.iter().position(|name| name == table).map_or_default(|id| {
            names[id].iter().enumerate().map(|(i, name)| (u16::try_from(i).unwrap(), name.clone())).collect()
        });
    let demo = cur.dec.table_names_for_tests(table).unwrap_or_default();
    let wrong: Vec<_> = demo
        .iter()
        .filter(|(i, name)| client.get(i) != Some(name))
        .map(|(i, name)| (*i, name.clone(), client.get(i).cloned()))
        .collect();
    (!wrong.is_empty() || !misplaced.is_empty()).then(|| format!("demo vs client {wrong:?}, misplaced {misplaced:?}"))
}

/// A relay playing `pl_upward.dem` some way in, its one client in the game with all it was sent
/// acked; the player, what it wants, and where the backlog's signon ends.
fn upward_played_to_a_client(b: &mut Bench) -> (Relay, Want, usize) {
    let path = crate::testing::fixture("pl_upward.dem");
    let first = upward_until(|c| c.started);
    let from = i64::from(first.dec.server_tick());
    let (relay, _, mut want) = relay_playing(b, first, 0);
    relay.hub().start_playing();
    let c = Cursor::open(&path, &relay.hub).unwrap();
    let signon = relay.hub().backlog.len();
    let mut p = Player::new(&relay);
    p.start_demo(c);
    want.paused = true;
    want.tick = u32::try_from(from + 5000).unwrap();
    for _ in 0..20 {
        p.step(&want).unwrap();
    }
    {
        let mut hub = relay.hub();
        let end = hub.backlog.len();
        for c in hub.clients.values_mut() {
            (c.cursor, c.gate, c.outgoing) = (end, None, None);
            c.queue.clear();
            c.set_phase(Phase::Active, Instant::now());
        }
    }
    let h = p.hand_over();
    assert!(relay.hub().silent && relay.hub().clients.is_empty());
    drop(p);
    let h: crate::relay::Handover = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
    assert_eq!(h.clients(), 1);
    let config = crate::relay::RelayConfig::new("127.0.0.1:0");
    let next = Relay::bind(&config).unwrap();
    next.hub().start_playing();
    assert!(next.take_over(&h));
    assert_eq!(next.hub().backlog.len(), relay.hub().backlog.len());
    (next, want, signon)
}

/// A relay playing `pl_upward.dem` some way in to its one client, in the game with all it was
/// sent acked; its player and what it wants.
fn upward_playing_to_a_client(b: &mut Bench) -> (Relay, Want) {
    let first = upward_until(|c| c.started);
    let from = i64::from(first.dec.server_tick());
    let (relay, _, mut want) = relay_playing(b, first, 0);
    relay.hub().start_playing();
    want.paused = true;
    want.tick = u32::try_from(from + 5000).unwrap();
    (relay, want)
}

mod demos;
mod frames;
mod handover;
mod lossy;
mod updates;
