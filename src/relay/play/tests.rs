use super::beams::past_beams;
use super::cursor::{Cursor, without_cues};
use super::*;
use crate::decode::{BeamModel, Entities, ModelsSent, Span};
use crate::demo::Packet;
use crate::net::bits::BitWriter;
use crate::net::messages::{self, SVC_USERMESSAGE, Signon, TYPE_BITS};
use crate::net::receiver::Receiver;
use crate::relay::client::Phase;
use crate::relay::hub::Part;
use crate::relay::server_msg::vgui_menu;
use crate::relay::tests::Bench;
use std::time::{Duration, Instant};

use crate::net::messages::{SVC_GAMEEVENT, SVC_TEMPENTITIES};
use tf_demo_parser::demo::gameevent_gen::GameEvent;
use tf_demo_parser::demo::message::Message;

/// `pl_upward.dem` decoded until `done`.
pub(super) fn upward_until(done: impl Fn(&Cursor) -> bool) -> Cursor {
    let path = crate::fixture("pl_upward.dem");
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

#[test]
fn a_full_update_holds_the_round_timer_and_the_next_runs_it_again() {
    // Twenty seconds in, with the round's timer on the HUD: the relay's, and a client's.
    let twenty = || {
        upward_until(|c| c.started && c.dec.server_tick() >= c.first.unwrap_or(i64::MAX).saturating_add(20 * 66) as u32)
    };
    let (mut relay, mut client) = (twenty(), twenty());
    let running = client.dec.extractor().snapshot().timers;
    assert!(!running.is_empty() && running.iter().all(|t| !t.paused), "{running:?}");
    relay.peek();
    let next = relay.next_frame();
    let (_, held) = relay.dec.keyframe_held(next).expect("a full update");
    client.dec.feed(held.bytes(), held.nbits());
    let timers = client.dec.extractor().snapshot().timers;
    assert_eq!(timers.len(), running.len());
    for (h, r) in timers.iter().zip(&running) {
        assert!(h.paused, "{h:?}");
        assert!((h.remaining - r.remaining).abs() < 0.5, "held at {} where it was {}", h.remaining, r.remaining);
    }
    // Going on: a full update as it is runs them again, though running is their baseline.
    let (_, plain) = relay.dec.keyframe(next).expect("a full update");
    client.dec.feed(plain.bytes(), plain.nbits());
    let timers = client.dec.extractor().snapshot().timers;
    assert!(timers.iter().all(|t| !t.paused), "{timers:?}");
}

#[test]
fn a_full_update_gives_every_weapon_the_reset_parity_a_client_starts_it_with() {
    use tf_demo_parser::demo::sendprop::SendPropIdentifier;
    let charged = |c: &Cursor| c.dec.extractor().snapshot().players.iter().any(|p| p.uber >= 40);
    let (mut relay, mut client) = (upward_until(charged), upward_until(charged));
    relay.peek();
    let next = relay.next_frame();
    let (_, key) = relay.dec.keyframe(next).expect("a full update");
    client.dec.feed(key.bytes(), key.nbits());
    let state = client.dec.parser_state_for_tests();
    let parity = SendPropIdentifier::new("DT_TFWeaponBase", "m_bResetParity");
    let mut weapons = 0;
    for (i, (class, _, props)) in client.dec.world_dump() {
        let Some(at) = super::super::super::decode::props_index_for_tests(state, class, parity) else { continue };
        weapons += 1;
        // Created with it set, a client resets the weapon: a medigun's charge goes to 0.
        let value = props.iter().find(|(j, _)| *j == at).map(|(_, v)| v.as_str());
        assert_eq!(value, Some("Integer(0)"), "weapon {i}");
    }
    assert!(weapons > 10, "{weapons} weapons");
}

#[test]
fn the_timers_are_told_to_redraw_with_the_demos_own_event() {
    let c = upward_until(|c| c.started);
    let msg = c.dec.update_timer().expect("the demo has the event");
    let state = c.dec.parser_state_for_tests();
    let mut s = bitbuffer::BitReadStream::new(bitbuffer::BitReadBuffer::new(msg.bytes(), bitbuffer::LittleEndian));
    let t: tf_demo_parser::demo::message::MessageType = bitbuffer::BitRead::read(&mut s).unwrap();
    let Message::GameEvent(e) = Message::from_type(t, &mut s, state).unwrap() else { panic!() };
    assert!(matches!(e.event, GameEvent::TeamPlayUpdateTimer(_)), "{:?}", e.event);
}

#[test]
fn the_clock_is_put_past_beams_it_would_go_back_under() {
    let interval = 0.5;
    // Paused a minute, a line drawn 59 s in, playback going on at 100 s: gone by 162 s.
    assert_eq!(past_beams(160.0, 100.0, 161.0, interval), Some(324));
    assert_eq!(past_beams(160.0, 100.0, 159.0, interval), None, "nothing showing");
    assert_eq!(past_beams(100.2, 100.0, 102.0, interval), None, "a client just in, playback going");
    assert_eq!(past_beams(100.0, 400.0, 102.0, interval), None, "ahead: they run out at once");
}

#[test]
fn beams_read_back_as_the_games_own_drawn_with_a_sprite_of_the_match() {
    use crate::decode::beams::{BASE, POINTS};
    use tf_demo_parser::demo::sendprop::{SendPropIdentifier, SendPropValue};
    use tf_demo_parser::demo::vector::Vector;
    let c = upward_until(|c| c.started);
    let beam = Beam {
        from: [1.0, 2.0, 3.0],
        to: [100.0, -200.0, 300.0],
        rgb: [255, 128, 0],
        life: 4.0,
        width: 2.0,
        speed: 20,
    };
    let BeamModel { index: model, load } =
        c.dec.beam_model(None, ModelsSent::default()).expect("a model to draw beams with");
    let msgs = c.dec.beams(&[beam; 40], model).expect("a beam class");
    assert_eq!(msgs.len(), 3, "16 a message");
    assert!(msgs.iter().all(|m| m.nbits() / 8 < 1000), "each fits a datagram");
    let state = c.dec.parser_state_for_tests();
    let mut events = Vec::new();
    for m in &msgs {
        let mut s = tf_demo_parser::Stream::from(bitbuffer::BitReadBuffer::new_owned(
            m.bytes().to_vec(),
            bitbuffer::LittleEndian,
        ));
        assert_eq!(s.read_int::<u32>(TYPE_BITS).unwrap(), SVC_TEMPENTITIES);
        let Message::TempEntities(te) =
            Message::from_type(tf_demo_parser::demo::message::MessageType::TempEntities, &mut s, state).unwrap()
        else {
            panic!()
        };
        events.extend(te.events);
    }
    assert_eq!(events.len(), 40);
    let get = |t: &str, n: &str| {
        events[39]
            .props
            .iter()
            .find(|p| p.identifier == SendPropIdentifier::new(t, n))
            .map(|p| p.value.clone())
            .unwrap()
    };
    assert_eq!(get(POINTS, "m_vecEndPoint"), SendPropValue::Vector(Vector { x: 100.0, y: -200.0, z: 300.0 }));
    assert_eq!(get(BASE, "g"), SendPropValue::Integer(128));
    assert_eq!(get(BASE, "m_nSpeed"), SendPropValue::Integer(20));
    assert_eq!(get(BASE, "m_nModelIndex"), SendPropValue::Integer(i64::from(model)));
    // The match did not load the laser: it goes after the last of the models, as a game client
    // adds it whatever index it is sent with.
    let load = load.expect("the laser loaded by the relay");
    let mut s = tf_demo_parser::Stream::from(bitbuffer::BitReadBuffer::new_owned(
        load.bytes().to_vec(),
        bitbuffer::LittleEndian,
    ));
    assert_eq!(s.read_int::<u32>(TYPE_BITS).unwrap(), messages::SVC_UPDATESTRINGTABLE);
    let msg = Message::from_type(tf_demo_parser::demo::message::MessageType::UpdateStringTable, &mut s, state).unwrap();
    let Message::UpdateStringTable(u) = msg else { panic!() };
    let table = &state.string_tables[usize::from(u.table_id)];
    assert_eq!(u.entries.len(), 1);
    assert_eq!(u.entries[0].1.text.as_deref(), Some("sprites/laserbeam.vmt"));
    assert_eq!(u.entries[0].0, model);
    assert!(model < table.max_entries);
    assert_eq!(
        c.dec.beam_model(Some(model), ModelsSent::default()).unwrap().index,
        model,
        "where it went, while the match leaves it be"
    );
    let SendPropValue::Float(life) = get(BASE, "m_fLife") else { panic!() };
    assert!((life - 4.0).abs() < 0.2, "{life}");
}

#[test]
fn a_camera_cue_names_the_players_entity_and_the_demos_own_go() {
    let path = crate::fixture("pl_upward.dem");
    let mut c = Cursor::open_detached(&path).unwrap();
    let mut cues = 0;
    // Ten seconds in, with the players there.
    while c.dec.server_tick() < c.first.unwrap_or(i64::MAX).saturating_add(660) as u32 || c.first.is_none() {
        match c.next_tick() {
            None => panic!("the demo ended"),
            Some(NOT_A_FRAME) => c.skip_next(),
            Some(_) => {
                let Some((bits, life)) = c.decode() else { continue };
                cues += life.spans.iter().filter(|s| c.dec.is_director_cue(bits.bytes(), s)).count();
                let (bits, life) = without_cues(&c.dec, &bits, &life);
                assert!(!life.spans.iter().any(|s| c.dec.is_director_cue(bits.bytes(), s)));
            }
        }
    }
    eprintln!("{cues} director cues in the first 10 s");
    let player = c.dec.extractor().snapshot().players.iter().find(|p| p.alive).cloned().unwrap();
    let msg = c.dec.hltv_chase(player.id.0).unwrap();
    let span = Span { id: u8::try_from(SVC_GAMEEVENT).unwrap(), bits: 0..msg.nbits() };
    assert!(c.dec.is_director_cue(msg.bytes(), &span));
    let state = c.dec.parser_state_for_tests();
    let mut s = bitbuffer::BitReadStream::new(bitbuffer::BitReadBuffer::new(msg.bytes(), bitbuffer::LittleEndian));
    let t: tf_demo_parser::demo::message::MessageType = bitbuffer::BitRead::read(&mut s).unwrap();
    let Message::GameEvent(e) = Message::from_type(t, &mut s, state).unwrap() else { panic!() };
    let GameEvent::HLTVChase(chase) = e.event else { panic!("{:?}", e.event) };
    assert_eq!(u32::from(chase.target_1), player.ent);
    assert_eq!(chase.in_eye, 1);
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
            w.write_buf(&vgui_menu(crate::relay::SPECTATORS, true));
            ids.push(u8::try_from(SVC_USERMESSAGE).unwrap());
        }
    }
    c.peek();
    c.next = Some(Packet::Message { tick, data: w.into_bytes() });
    (c, ids)
}

fn four_frames_kept(c: &Cursor) -> bool {
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
fn bench_with_one_in() -> Bench {
    let mut b = Bench::new("", 1);
    b.connect();
    let c = b.client_state();
    c.gate = None;
    c.set_phase(Phase::Active, Instant::now());
    b
}

#[test]
fn a_played_frame_reaches_a_client_without_the_servers_changelevel() {
    let (mut c, ids) = upward_with_changelevel();
    let mut reader = upward_until(four_frames_kept);
    let (bits, life) = c.decode().unwrap();
    let mut b = bench_with_one_in();
    b.hub.upstream_stream(&bits, Part::Unreliable, &life);
    b.hub.upstream_flush();
    assert_eq!(sent_unreliably(&b, &mut reader), [ids]);
}

#[test]
fn the_frames_sent_with_a_full_update_are_without_the_servers_changelevel() {
    let (mut c, ids) = upward_with_changelevel();
    let mut reader = upward_until(four_frames_kept);
    c.decode().unwrap();
    c.peek();
    let (_, key) = c.dec.keyframe(c.next_frame()).unwrap();
    // From two frames back: the changelevel's frame, and the one it builds on, go as they are.
    let recent = c.recent.make_contiguous();
    let since = c.dec.catch_up(recent[recent.len() - 3].0, recent);
    let mut b = bench_with_one_in();
    b.client_state().set_phase(Phase::WaitingFull, Instant::now());
    b.hub.send_keyframe(&key, &since, false);
    let sent = sent_unreliably(&b, &mut reader);
    assert_eq!(sent.last(), Some(&ids), "{sent:?}");
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

/// A relay with the bench's hub, its client in the game, and a player of `c` toward `tick`.
fn relay_playing(b: &mut Bench, c: Cursor, tick: u32) -> (Relay, Cursor, Want) {
    let config = crate::relay::RelayConfig::new("127.0.0.1:0");
    let spare = crate::relay::Hub::new(std::net::UdpSocket::bind("127.0.0.1:0").unwrap(), &config);
    let hub = std::mem::replace(&mut b.hub, spare);
    let relay = Relay { sock: std::net::UdpSocket::bind("127.0.0.1:0").unwrap(), port: 0, hub: hub.into() };
    let want = Want {
        demo: c.path.clone(),
        tick,
        at: Instant::now(),
        paused: false,
        speed: 1.0,
        from_start: false,
        camera: None,
    };
    (relay, c, want)
}

/// A live recording's frames build on one a few back: after a full update the clients follow
/// them as they are, not a full update each.
#[test]
fn a_live_recording_plays_on_after_a_full_update() {
    let opt = crate::Options { snapshot_hz: 0.0, ..crate::Options::default() };
    let live = crate::demo::record_capture_for_tests("bots_cp_badlands.cap", &opt).1;
    // Keyed at a few places, each its own.
    for at in [40, 41, 42, 43, 44, 45] {
        let mut c = Cursor::open_detached(&live).unwrap();
        while c.recent.len() < at {
            match c.next_tick() {
                Some(NOT_A_FRAME) => c.skip_next(),
                Some(_) => drop(c.decode()),
                None => panic!("the demo ended"),
            }
        }
        let mut b = bench_with_one_in();
        b.client_state().set_phase(Phase::WaitingFull, Instant::now());
        let tick = c.dec.server_tick() + 40;
        let (relay, c, want) = relay_playing(&mut b, c, tick);
        let mut p = Player::new(&relay);
        p.cur = Some(c);
        for _ in 0..60 {
            p.step(&want).unwrap();
        }
        // Read as the player's own decoder reads them.
        let (fulls, deltas) = follow_as_a_game_client(&b, p.cur.as_ref().unwrap());
        assert_eq!(fulls, 1, "keyed after {at}");
        assert!(deltas >= 20, "{deltas} after {at}");
    }
}

/// A demo with a full update, then a delta from before it (`E, F, G`): a game client given `F`
/// has dropped `E`. The clients in the game get a full update of their own instead, and `G`
/// only when they have what it builds on.
#[test]
fn a_full_update_in_the_demo_then_a_delta_from_before_it_reach_a_client_it_can_follow() {
    let mut c = upward_until(four_frames_kept);
    let e = c.dec.server_tick();
    let mut copy = upward_until(four_frames_kept);
    let Some(Packet::Message { tick: demo_tick, .. }) = copy.peek().cloned() else { panic!("a frame next") };
    copy.decode().unwrap();
    let (n1, full) = copy.dec.keyframe(None).unwrap();
    let from_e = copy.dec.delta_for_tests(e, n1).unwrap();
    c.peek();
    c.next = Some(Packet::Message { tick: demo_tick, data: full.bytes().to_vec() });

    let mut b = bench_with_one_in();
    let (relay, c, want) = relay_playing(&mut b, c, e + 10);
    let mut p = Player::new(&relay);
    p.cur = Some(c);
    p.step(&want).unwrap();
    p.cur.as_mut().unwrap().next = Some(Packet::Message { tick: demo_tick, data: from_e.bytes().to_vec() });
    for _ in 0..4 {
        p.step(&want).unwrap();
    }
    let reader = upward_until(four_frames_kept);
    let (fulls, deltas) = follow_as_a_game_client(&b, &reader);
    assert!(fulls >= 1 && deltas >= 2, "a full update, and frames after it: {fulls}, {deltas}");
}

/// The frames sent after a keyframe, where their bases go back: one from a frame a game client
/// dropped (older than the base of one before it) is rewritten from one it has.
#[test]
fn the_frames_after_a_keyframe_build_only_on_frames_a_client_still_has() {
    let opt = crate::Options { snapshot_hz: 0.0, ..crate::Options::default() };
    let live = crate::demo::record_capture_for_tests("bots_cp_badlands.cap", &opt).1;
    let mut c = Cursor::open_detached(&live).unwrap();
    // On until four frames back are kept.
    let kept = loop {
        match c.next_tick() {
            Some(NOT_A_FRAME) => c.skip_next(),
            Some(_) => drop(c.decode()),
            None => panic!("the demo ended"),
        }
        let kept: Vec<u32> = c.recent.iter().map(|(t, _)| *t).filter(|t| c.dec.has_frame_for_tests(*t)).collect();
        if kept.len() >= 4 {
            break kept[kept.len() - 4..].to_vec();
        }
    };
    let [k0, k1, k2, k3] = kept[..] else { unreachable!() };
    let d = |from, to| (to, c.dec.delta_for_tests(from, to).unwrap());
    let recent = [d(k0, k1), d(k1, k2), d(k0, k3)];
    let key = c.dec.keyframe_at_for_tests(k0).unwrap();
    let mut frames = std::collections::BTreeSet::new();
    for s in std::iter::once(key).chain(c.dec.catch_up(k0, &recent)) {
        let tick = c.dec.frame_tick(s.bytes()).unwrap();
        match c.dec.entities(s.bytes()).unwrap() {
            Entities::Full => frames.clear(),
            Entities::Delta(base) => {
                assert!(frames.contains(&base), "to {tick} from {base}, the client has {frames:?}");
                frames.retain(|t| *t >= base);
            }
        }
        frames.insert(tick);
    }
    // Frames from before the keyframe's are rewritten from it: the next builds on it.
    for s in &c.dec.catch_up(k1, &[d(k0, k2), d(k0, k3)]) {
        assert_eq!(c.dec.entities(s.bytes()), Some(Entities::Delta(k1)));
    }
}

/// The match loads the laser after the clients were given the relay's: they sign on again, after
/// which the match's own is used.
#[test]
fn the_match_loading_the_laser_after_the_relays_signs_the_clients_on_again() {
    let c = upward_until(four_frames_kept);
    let ours = c.dec.beam_model(None, ModelsSent::default()).unwrap().index;
    let past = ModelsSent::next_for_tests(ours + 1);
    let theirs = c.dec.beam_model(None, past).unwrap();
    let (theirs, load) = (theirs.index, theirs.load.unwrap());
    let mut b = bench_with_one_in();
    let tick = c.dec.server_tick();
    let (relay, c, want) = relay_playing(&mut b, c, tick);
    let mut p = Player::new(&relay);
    p.laser = Some(ours);
    p.cur = Some(c);
    p.step(&want).unwrap();
    assert!(p.cur.is_some(), "the match has not loaded it yet");
    p.cur.as_mut().unwrap().dec.feed(load.bytes(), load.nbits());
    assert_eq!(p.cur.as_ref().unwrap().dec.beam_model(None, ModelsSent::default()).unwrap().index, theirs);
    p.step(&want).unwrap();
    assert!(p.cur.is_none(), "a new signon");
}

/// Kept beams are drawn only to clients in the game, past a pending full update: at a demo's
/// start, before playback skims to the wanted moment, the relay loads no laser of its own. Once the
/// match loaded its own after the relay's, the relay's is not loaded again for that demo.
#[test]
fn the_relays_laser_is_loaded_once_per_demo_whose_match_loads_its_own() {
    let c = upward_until(four_frames_kept);
    let ours = c.dec.beam_model(None, ModelsSent::default()).unwrap().index;
    let load = c.dec.beam_model(None, ModelsSent::next_for_tests(ours + 1)).unwrap().load.unwrap();
    let mut b = bench_with_one_in();
    b.client_state().set_phase(Phase::Signon, Instant::now());
    let tick = c.dec.server_tick();
    let (relay, c, want) = relay_playing(&mut b, c, tick);
    let playback = Playback::new(want.clone());
    let beam = Beam { from: [0.0; 3], to: [1.0; 3], rgb: [255, 0, 0], life: 1.0, width: 2.0, speed: 0 };
    playback.keep_beams(vec![beam]);
    let mut p = Player::new(&relay);
    p.start_demo(c);
    p.show_kept(&playback);
    assert_eq!(p.laser, None, "nobody in the game");
    let in_game = |relay: &Relay, phase| {
        for c in relay.hub().clients.values_mut() {
            c.set_phase(phase, Instant::now());
        }
    };
    in_game(&relay, Phase::Active);
    p.rekeyed = true;
    p.show_kept(&playback);
    assert_eq!(p.laser, None, "a full update pending");
    p.rekeyed = false;
    p.show_kept(&playback);
    assert_eq!(p.laser, Some(ours), "in the game");
    // The match loads its own: a new signon, after which the relay's is not loaded again.
    p.cur.as_mut().unwrap().dec.feed(load.bytes(), load.nbits());
    p.step(&want).unwrap();
    assert!(p.cur.is_none(), "a new signon");
    p.start_demo(upward_until(four_frames_kept));
    p.kept.clear();
    p.show_kept(&playback);
    assert_eq!(p.laser, None, "not again");
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

/// Far ahead, back, and ahead again from a checkpoint: the clients' string tables are the demo's
/// each time, and the checkpoint gives them only the changes past those they had.
#[test]
fn a_jump_ahead_from_a_checkpoint_gives_the_clients_only_the_table_changes_passed_over() {
    let path = crate::fixture("pl_upward.dem");
    let mut b = bench_with_one_in();
    let first = upward_until(|c| c.started);
    let from = i64::from(first.dec.server_tick());
    let (relay, _, mut want) = relay_playing(&mut b, first, 0);
    relay.hub().start_playing();
    let c = Cursor::open(&path, &relay.hub).unwrap();
    let signon = relay.hub().backlog.len();
    let mut p = Player::new(&relay);
    p.start_demo(c);
    let index = index::Index::shared(&path);
    let (early, mid, late) = (from + 3000, from + 9000, from + 13_000);
    let since = Instant::now();
    while index.before(late).is_none_or(|m| i64::from(m.tick) < mid) {
        assert!(since.elapsed() < Duration::from_secs(60), "the demo has a checkpoint by then");
        std::thread::sleep(Duration::from_millis(50));
    }
    p.index = Some(index);
    want.paused = true;
    let mut go = |p: &mut Player<'_>, tick: i64| {
        want.tick = u32::try_from(tick).unwrap();
        want.at = Instant::now();
        for _ in 0..20 {
            p.step(&want).unwrap();
        }
        assert_eq!(p.cur.as_ref().unwrap().tick() / 100, tick / 100, "there");
    };
    go(&mut p, late);
    assert!(client_tables_are_the_demos(&relay, &p, signon).is_some(), "ahead");
    let changes = p.cur.as_ref().unwrap().tables.len();
    go(&mut p, early);
    assert!(client_tables_are_the_demos(&relay, &p, signon).is_some(), "back");
    let (had, sent) = (p.cur.as_ref().unwrap().tables.len(), relay.hub().backlog.len());
    assert!(had < changes, "the demo has string table changes between");
    go(&mut p, mid);
    assert!(client_tables_are_the_demos(&relay, &p, signon).is_some(), "ahead from a checkpoint");
    let passed = p.cur.as_ref().unwrap().tables.len() - had;
    assert!(relay.hub().backlog.len() - sent <= passed, "none of those before");
}

/// A demo that does not open is tried again five times a second: the log says why once.
#[test]
fn a_demo_that_cannot_be_played_is_logged_once_until_the_reason_changes() {
    let mut b = bench_with_one_in();
    let (relay, c, want) = relay_playing(&mut b, upward_until(|c| c.started), 0);
    drop(c);
    let mut p = Player::new(&relay);
    assert!(p.refuses_anew(&want.demo, "other class tables"));
    assert!(!p.refuses_anew(&want.demo, "other class tables"), "the next try");
    assert!(p.refuses_anew(&want.demo, "not written yet"), "another reason");
    assert!(p.refuses_anew(std::path::Path::new("next.dem"), "not written yet"), "another demo");
}

/// A demo not there yet is tried again; one that is no demo, never.
#[test]
fn a_demo_that_will_never_open_is_not_tried_again() {
    let mut b = bench_with_one_in();
    let (relay, c, mut want) = relay_playing(&mut b, upward_until(|c| c.started), 0);
    drop(c);
    let dir = std::env::temp_dir().join(format!("stvtf-never-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let playback = Playback::new(want.clone());
    let mut p = Player::new(&relay);
    want.demo = dir.join("later.dem");
    assert!(p.open_wanted(&want, &playback).is_break());
    assert_eq!(p.given_up, None, "not written yet");
    std::fs::write(&want.demo, [0u8; 16]).unwrap();
    assert!(p.open_wanted(&want, &playback).is_break());
    assert_eq!(p.given_up, None, "its header is not all there");
    want.demo = dir.join("no.dem");
    std::fs::write(&want.demo, vec![7u8; 4096]).unwrap();
    assert!(p.open_wanted(&want, &playback).is_break());
    assert_eq!(p.given_up.as_ref(), Some(&want.demo));
    std::fs::remove_file(&want.demo).unwrap();
    assert!(p.open_wanted(&want, &playback).is_break());
    assert_eq!(p.given_up.as_ref(), Some(&want.demo), "not opened again: gone, it would be one not there yet");
    std::fs::remove_dir_all(&dir).unwrap();
}
