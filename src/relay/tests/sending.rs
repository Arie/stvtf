use super::*;

#[test]
fn the_first_block_goes_alone_then_all_subchannels_at_once() {
    let mut x = 1u32;
    let noise = (0..20_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect();
    let mut queue = vec![BitBuf::from_bytes(noise)];
    let mut c = Client::new("x".into(), 1, 2, Instant::now());
    c.queue.append(&mut queue);
    let first = c.next_region(1).unwrap();
    assert!(c.next_region(2).is_none(), "nothing before the first block is acked");
    let mut r = Reassembler::default();
    assert_eq!(r.feed(first.bytes(), 0).unwrap().completed.len(), 0);
    let ack = |seq, state| Header {
        sequence: seq,
        ack: seq,
        flags: 0,
        reliable_state: state,
        choked: None,
        challenge: None,
        body_offset: 0,
    };
    c.acked(&ack(1, 0b1));
    let blocks: Vec<BitBuf> = (2..).map_while(|seq| c.next_region(seq)).collect();
    assert_eq!(blocks.len(), usize::from(SUBCHANNELS));
    // The client gets all but the one on subchannel 2, which comes again.
    for (i, b) in blocks.iter().enumerate().filter(|(i, _)| *i != 2) {
        assert_eq!(r.feed(b.bytes(), 0).unwrap().subchannel, i as u8);
    }
    c.acked(&ack(9, 0b1111_1011 ^ 0b1));
    let again = c.next_region(10).unwrap();
    assert_eq!(
        BitReader::new(again.bytes()).read_bits(SUBCHANNEL_BITS).unwrap(),
        0,
        "lost block on the first free subchannel"
    );
    let mut done = r.feed(again.bytes(), 0).unwrap().completed;
    let mut state = 0b1111_1011 ^ 0b1 ^ 0b1;
    let mut seq = 10;
    while c.outgoing.is_some() {
        let sub = (0..SUBCHANNELS).find(|i| c.outgoing.as_ref().unwrap().in_flight(usize::from(*i)));
        if let Some(sub) = sub {
            state ^= 1 << sub;
        }
        c.acked(&ack(seq, state));
        seq += 1;
        while let Some(b) = c.next_region(seq) {
            done.extend(r.feed(b.bytes(), 0).unwrap().completed);
            let sub = BitReader::new(b.bytes()).read_bits(SUBCHANNEL_BITS).unwrap();
            state ^= 1 << sub;
            c.acked(&ack(seq, state));
            seq += 1;
        }
    }
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].len(), 20_000);
}

#[test]
fn notes_go_again_after_every_full_update() {
    let (mut hub, active) = hub_with_one_in();
    let msg = |v: u32| {
        let mut w = BitWriter::new();
        w.write_bits(v, 16);
        w.finish()
    };
    let (key, note) = (msg(1), msg(2));
    // The reliable data sent: the pause, then the note.
    let sent = |hub: &mut Hub| {
        let c = hub.clients.get_mut(&active).unwrap();
        c.outgoing.take().map_or_default(|o| o.data().to_vec())
    };
    let mut w = BitWriter::new();
    w.write_buf(&set_pause(false));
    w.write_buf(&note);
    let expected = w.into_bytes();
    hub.set_notes(vec![note.clone()]);
    assert!(hub.clients[&active].queue.is_empty(), "for full updates to come; shown now by the player");
    hub.rekey_all(Instant::now());
    hub.send_keyframe(1, &key, &[], false);
    assert!(sent(&mut hub).starts_with(&expected), "after its full update");
    // One coming in while a demo plays gets a full update of its own, not the datagrams since.
    hub.clients.get_mut(&active).unwrap().set_phase(Phase::Signon, Instant::now());
    hub.mode = Mode::Play { paused: false };
    hub.since_full = Some(vec![key.clone()]);
    hub.start_entities(active, Instant::now());
    assert_eq!(hub.clients[&active].phase, Phase::WaitingFull);
    hub.send_keyframe(2, &key, &[], false);
    assert!(sent(&mut hub).starts_with(&expected), "and after that one");
}

#[test]
fn notes_go_again_once_a_client_is_in_and_after_a_hud_reset() {
    let (mut hub, active) = hub_with_one_in();
    let mut w = BitWriter::new();
    w.write_bits(2, 16);
    let note = w.finish();
    hub.set_notes(vec![note.clone()]);
    let now = Instant::now();
    hub.clients.get_mut(&active).unwrap().enter_notes(&hub.notes.clone(), now);
    let queued = |hub: &mut Hub| std::mem::take(&mut hub.clients.get_mut(&active).unwrap().queue);
    assert_eq!(queued(&mut hub), std::slice::from_ref(&note), "with its entities");
    hub.send_due_notes(now);
    assert_eq!(queued(&mut hub), [], "not yet");
    hub.send_due_notes(now + NOTES_AFTER_ENTERING);
    assert_eq!(queued(&mut hub), std::slice::from_ref(&note), "once its HUD is up");
    hub.send_due_notes(now + NOTES_AFTER_ENTERING * 2);
    assert_eq!(queued(&mut hub), [], "once");
    hub.reshow_notes(now);
    hub.send_due_notes(now + NOTES_AFTER_RESET);
    assert_eq!(queued(&mut hub), std::slice::from_ref(&note), "past a HUD reset");
    // A reset as it goes in (a seek's full update) leaves the wait for its HUD.
    hub.clients.get_mut(&active).unwrap().enter_notes(&[], now);
    hub.reshow_notes(now);
    hub.send_due_notes(now + NOTES_AFTER_RESET);
    assert_eq!(queued(&mut hub), std::slice::from_ref(&note), "past the reset");
    hub.send_due_notes(now + NOTES_AFTER_ENTERING);
    assert_eq!(queued(&mut hub), [note], "and once its HUD is up");
}

#[test]
fn a_late_client_gets_the_table_changes_once_and_one_in_the_game_skips_them() {
    let (mut hub, active) = hub_with_one_in();
    let now = Instant::now();
    let (bits, life) = datagram_with_table_change();
    for _ in 0..3 {
        hub.upstream_stream(&bits, Part::Unreliable, &life);
        hub.upstream_flush();
    }
    assert_eq!(hub.backlog.len(), 1, "logged once, however often it comes");
    let c = &hub.clients[&active];
    assert_eq!((c.cursor, c.queue.len()), (1, 0), "a client in the game has it from the datagram");
    let mut late = Client::new("late".into(), 2, 2, now);
    late.gate = None;
    late.pump(&hub.backlog);
    assert_eq!(late.queue, [bits]);
    assert!(!late.through_backlog(&hub.backlog), "no entities before the change is acked");
}

#[test]
fn tables_set_back_go_again_on_every_jump_back() {
    let (mut hub, active) = hub_with_one_in();
    let (later, life) = datagram_with_table_change();
    let mut w = BitWriter::new();
    w.write_bits(SVC_UPDATESTRINGTABLE, TYPE_BITS);
    w.write_bits(0x1234, 16);
    let back = w.finish();
    // Back from past the change, on to it again, and back again: the same set-back each time.
    for round in 1..=2 {
        hub.upstream_stream(&later, Part::Unreliable, &life);
        hub.upstream_flush();
        hub.clients.get_mut(&active).unwrap().queue.clear();
        hub.set_tables_back(std::slice::from_ref(&later), std::slice::from_ref(&back));
        assert_eq!(hub.clients[&active].queue, std::slice::from_ref(&back), "jump back {round}");
    }
}

/// A player leaves a slot, another takes it and leaves too: the second clearing is the first's
/// bits, and a change all the same. A demo's own repeat right after is not.
#[test]
fn a_demos_table_change_the_same_as_an_earlier_one_reaches_the_clients_again() {
    let (mut hub, active) = hub_with_one_in();
    hub.start_playing();
    let change = |value: u32| {
        let mut w = BitWriter::new();
        w.write_bits(SVC_UPDATESTRINGTABLE, TYPE_BITS);
        w.write_bits(value, 16);
        w.finish()
    };
    let (cleared, taken) = (change(1), change(2));
    hub.add_table_changes(&[cleared.clone(), taken.clone(), cleared.clone(), cleared.clone()]);
    assert_eq!(hub.clients[&active].queue, [cleared.clone(), taken.clone(), cleared.clone()]);
    // As played: in a datagram's unreliable part.
    let (bits, life) = datagram_with_table_change();
    for msg in [&bits, &taken, &bits] {
        let life = Lifecycle { spans: life.spans.clone(), ..Lifecycle::default() };
        hub.upstream_stream(msg, Part::Unreliable, &life);
    }
    assert_eq!(hub.clients[&active].queue[3..], [bits.clone(), taken, bits]);
}
