use super::*;

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
    let (tick, key) = c.dec.keyframe(c.next_frame()).unwrap();
    // From two frames back: the changelevel's frame, and the one it builds on, go as they are.
    let recent = c.recent.make_contiguous();
    let since = c.dec.catch_up(recent[recent.len() - 3].0, recent);
    let mut b = bench_with_one_in();
    b.client_state().set_phase(Phase::WaitingFull, Instant::now());
    b.hub.send_keyframe(tick, &key, &since, false);
    let sent = sent_unreliably(&b, &mut reader);
    assert_eq!(sent.last(), Some(&ids), "{sent:?}");
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
            clients_ack(&relay);
        }
        // Read as the player's own decoder reads them.
        let (fulls, deltas) = follow_as_a_game_client(&b, p.cur.as_ref().unwrap());
        assert_eq!(fulls, 1, "keyed after {at}");
        assert!(deltas >= 20, "{deltas} after {at}");
    }
}

/// A demo a player recorded: its string tables as recording started reach the client, and its
/// entities, which enter on baselines only that player's game had, are written anew. A decoder
/// with no more than a client has ends with the same entities, looking where the player did.
#[test]
fn a_players_own_demo_plays_to_a_client_that_has_only_its_signon() {
    let path = crate::testing::fixture("pov_koth_viaduct.dem");
    let mut c = Cursor::open_detached(&path).unwrap();
    while c.recent.len() < 120 {
        match c.next_tick() {
            Some(NOT_A_FRAME) => c.skip_next(),
            Some(_) => drop(c.decode()),
            None => panic!("the demo ended"),
        }
    }
    assert!(c.dec.players_own() && c.dec.own_baselines());
    let mut b = bench_with_one_in();
    b.client_state().set_phase(Phase::WaitingFull, Instant::now());
    let tick = c.dec.server_tick() + 60;
    let (relay, c, want) = relay_playing(&mut b, c, tick);
    let mut p = Player::new(&relay);
    p.cur = Some(c);
    for _ in 0..90 {
        p.step(&want).unwrap();
        clients_ack(&relay);
    }
    // As a client: the signon, then what it is sent.
    let mut client = Cursor::open_detached(&path).unwrap();
    b.client.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let mut rx = Receiver::new(Some(b.challenge));
    let mut buf = vec![0u8; crate::net::MAX_DATAGRAM];
    let (mut fulls, mut deltas, mut looks) = (0, 0, 0);
    while let Ok(len) = b.client.recv(&mut buf) {
        let Some(pkt) = rx.feed(&buf[..len], Instant::now()) else { continue };
        for s in &pkt.streams {
            let life = client.dec.feed(s.bits.bytes(), s.bits.nbits());
            assert!(!life.lost_frame, "a frame the client cannot take");
            looks += life.spans.iter().filter(|s| u32::from(s.id) == messages::SVC_FIXANGLE).count();
            match client.dec.entities(s.bits.bytes()) {
                Some(Entities::Full) => fulls += 1,
                Some(Entities::Delta(_)) => deltas += 1,
                None => {}
            }
        }
    }
    let played = &p.cur.as_ref().unwrap().dec;
    assert_eq!((fulls, client.dec.server_tick()), (1, played.server_tick()));
    assert!(deltas >= 30 && looks >= deltas, "{deltas} deltas, told where to look {looks} times");
    assert!(played.player_for_tests().is_some());
    assert!(client.dec.world_compared() == played.world_compared(), "the client's entities are the demo's");
}

/// A recording that stopped with no end written (its server gone for good): a client coming in
/// at its last frame gets a full update of that one, once no frame came after it for a while.
#[test]
fn a_client_coming_in_at_the_end_of_a_recording_cut_short_gets_a_full_update() {
    let opt = crate::Options { snapshot_hz: 0.0, ..crate::Options::default() };
    let whole = std::fs::read(crate::demo::record_capture_for_tests("bots_cp_badlands.cap", &opt).1).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let cut = dir.join("cut.dem");
    // Without the end: the stop and its tick.
    std::fs::write(&cut, &whole[..whole.len() - 5]).unwrap();
    let mut c = Cursor::open_detached(&cut).unwrap();
    loop {
        match c.next_tick() {
            Some(NOT_A_FRAME) => c.skip_next(),
            Some(_) => drop(c.decode()),
            None => break,
        }
    }
    assert!(c.started && !c.ended, "no more written, and no end");
    let mut b = bench_with_one_in();
    b.client_state().set_phase(Phase::WaitingFull, Instant::now());
    let tick = c.dec.server_tick();
    let (relay, c, want) = relay_playing(&mut b, c, tick);
    let mut p = Player::new(&relay);
    p.cur = Some(c);
    let phase = || relay.hub().clients.values().next().unwrap().phase;
    p.step(&want).unwrap();
    assert_eq!(phase(), Phase::WaitingFull, "the next frame may be on its way");
    std::thread::sleep(FRAME_WAIT * 2);
    p.step(&want).unwrap();
    assert_eq!(phase(), Phase::Active);
    assert_eq!(follow_as_a_game_client(&b, p.cur.as_ref().unwrap()).0, 1);
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
    clients_ack(&relay);
    p.cur.as_mut().unwrap().next = Some(Packet::Message { tick: demo_tick, data: from_e.bytes().to_vec() });
    for _ in 0..4 {
        p.step(&want).unwrap();
        clients_ack(&relay);
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
