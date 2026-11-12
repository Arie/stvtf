use super::beams::past_beams;
use super::cursor::Cursor;
use super::player::without_cues;
use super::*;
use crate::decode::Span;
use crate::demo::Packet;
use crate::net::bits::BitWriter;
use crate::net::messages::{self, SVC_USERMESSAGE, Signon, TYPE_BITS};
use crate::net::receiver::Receiver;
use crate::relay::client::Phase;
use crate::relay::server_msg::show_panel;
use crate::relay::tests::Bench;
use std::time::{Duration, Instant};

use crate::net::messages::{SVC_GAMEEVENT, SVC_TEMPENTITIES};
use tf_demo_parser::demo::gameevent_gen::GameEvent;
use tf_demo_parser::demo::message::Message;

/// `pl_upward.dem` decoded until `done`.
pub(super) fn upward_until(done: impl Fn(&Cursor) -> bool) -> Cursor {
    let path = crate::fixture("pl_upward.dem");
    let mut c = Cursor::open_with(&path, None).unwrap();
    while !done(&c) {
        match c.next_tick() {
            None => panic!("the demo ended"),
            Some(NOT_A_FRAME) => c.next = None,
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
    let (model, load) = c.dec.beam_model(None).expect("a model to draw beams with");
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
    assert_eq!(c.dec.beam_model(Some(model)).unwrap().0, model, "where it went, while the match leaves it be");
    let SendPropValue::Float(life) = get(BASE, "m_fLife") else { panic!() };
    assert!((life - 4.0).abs() < 0.2, "{life}");
}

#[test]
fn a_camera_cue_names_the_players_entity_and_the_demos_own_go() {
    let path = crate::fixture("pl_upward.dem");
    let mut c = Cursor::open_with(&path, None).unwrap();
    let mut cues = 0;
    // Ten seconds in, with the players there.
    while c.dec.server_tick() < c.first.unwrap_or(i64::MAX).saturating_add(660) as u32 || c.first.is_none() {
        match c.next_tick() {
            None => panic!("the demo ended"),
            Some(NOT_A_FRAME) => c.next = None,
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
    let (mut c, mut copy) = (upward_until(few_in), upward_until(few_in));
    let Some(Packet::Message { tick, .. }) = copy.peek().cloned() else { panic!("a frame next") };
    let (bits, life) = copy.decode().unwrap();
    let mut w = BitWriter::new();
    let mut ids = Vec::new();
    for (i, span) in life.spans.iter().enumerate() {
        w.write_range(bits.bytes(), span.bits.clone());
        ids.push(span.id);
        if i == 0 {
            w.write_buf(&messages::signonstate(Signon::ChangeLevel, 3));
            w.write_buf(&show_panel(crate::relay::SPECTATORS, true));
            ids.push(u8::try_from(SVC_USERMESSAGE).unwrap());
        }
    }
    c.peek();
    c.next = Some(Packet::Message { tick, data: w.into_bytes() });
    (c, ids)
}

fn few_in(c: &Cursor) -> bool {
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
    let mut reader = upward_until(few_in);
    let (bits, life) = c.decode().unwrap();
    let mut b = bench_with_one_in();
    b.hub.upstream_stream(&bits, false, &life);
    b.hub.upstream_flush();
    assert_eq!(sent_unreliably(&b, &mut reader), [ids]);
}

#[test]
fn the_frames_sent_with_a_full_update_are_without_the_servers_changelevel() {
    let (mut c, ids) = upward_with_changelevel();
    let mut reader = upward_until(few_in);
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
