use super::*;

/// Handed to the next process, the clients go on with a full update: no signon, their string
/// tables the demo's still, and what a client coming in later is given is what they were.
#[test]
fn clients_handed_over_go_on_where_they_were_with_a_full_update() {
    let mut b = bench_with_one_in();
    let (next, mut want, signon) = upward_played_to_a_client(&mut b);
    let had = next.hub().backlog.len();
    let playback = Playback::new(want.clone());
    let mut p = Player::new(&next);
    assert!(p.open_wanted(&want, &playback).is_continue());
    assert_eq!(i64::from(want.tick) / 100, p.cur.as_ref().unwrap().tick() / 100, "where it was");
    want.tick += 2000;
    for _ in 0..20 {
        p.step(&want).unwrap();
        // It acks what it is sent.
        for c in next.hub().clients.values_mut() {
            c.outgoing = None;
            c.queue.clear();
        }
    }
    assert!(client_tables_are_the_demos(&next, &p, signon).is_some());
    let hub = next.hub();
    let c = hub.clients.values().next().unwrap();
    assert_eq!(
        (c.phase, c.cursor, c.reconnecting),
        (Phase::Active, hub.backlog.len(), false),
        "a full update, no signon"
    );
    assert!((had..hub.backlog.len()).all(|i| hub.backlog.get(i).unwrap().gate.is_none()));
}

/// Wanted on another demo than they are signed on to, the clients taken over sign on again,
/// connected still.
#[test]
fn clients_handed_over_sign_on_again_when_the_room_is_on_another_demo() {
    let mut b = bench_with_one_in();
    let (next, mut want, _) = upward_played_to_a_client(&mut b);
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    want.demo = dir.join("other.dem");
    std::fs::copy(crate::testing::fixture("pl_upward.dem"), &want.demo).unwrap();
    let playback = Playback::new(want.clone());
    let mut p = Player::new(&next);
    assert!(p.open_wanted(&want, &playback).is_continue());
    let hub = next.hub();
    let c = hub.clients.values().next().unwrap();
    assert_eq!((c.phase, c.reconnecting), (Phase::Signon, true));
}

/// The demo the next process records of the same map load goes on from the one playing: the
/// clients keep what they have and get a full update, and its signon is what one coming in later
/// is given. A demo of another load signs them on again.
#[test]
fn a_demo_of_the_same_map_load_goes_on_with_no_signon() {
    let mut b = bench_with_one_in();
    let (relay, mut want) = upward_playing_to_a_client(&mut b);
    let mut p = Player::new(&relay);
    p.start_demo(Cursor::open(&want.demo, &relay.hub).unwrap());
    let acks = |relay: &Relay| {
        let mut hub = relay.hub();
        let end = hub.backlog.len();
        for c in hub.clients.values_mut() {
            (c.cursor, c.gate, c.outgoing) = (end, None, None);
            c.queue.clear();
            if c.phase == Phase::Signon {
                c.set_phase(Phase::WaitingFull, Instant::now());
            }
        }
    };
    for _ in 0..20 {
        p.step(&want).unwrap();
        acks(&relay);
    }
    assert_eq!(relay.hub().clients.values().next().unwrap().phase, Phase::Active);
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let playback = Playback::new(want.clone());
    // The same server and load, begun before where playback is.
    want.demo = dir.join("next.dem");
    std::fs::copy(crate::testing::fixture("pl_upward.dem"), &want.demo).unwrap();
    let at = p.cur.as_ref().unwrap().tick();
    assert!(p.open_wanted(&want, &playback).is_continue());
    {
        let hub = relay.hub();
        let c = hub.clients.values().next().unwrap();
        assert_eq!(
            (c.phase, c.reconnecting, c.cursor, c.queue.len()),
            (Phase::WaitingFull, false, hub.backlog.len(), 0)
        );
        assert_eq!((p.cur.as_ref().unwrap().path.clone(), p.cur.as_ref().unwrap().tick()), (want.demo.clone(), at));
    }
    for _ in 0..5 {
        p.step(&want).unwrap();
        acks(&relay);
    }
    assert_eq!(relay.hub().clients.values().next().unwrap().phase, Phase::Active);
    // With the relay's laser where one coming in later gets it too; not with it elsewhere.
    let laser = p.cur.as_ref().unwrap().dec.writer().beam_model(None, ModelsSent::default()).unwrap();
    let again = dir.join("again.dem");
    std::fs::copy(crate::testing::fixture("pl_upward.dem"), &again).unwrap();
    p.laser = Some(laser.index + 1);
    assert!(!p.goes_on_into(&again), "the laser elsewhere");
    p.laser = Some(laser.index);
    assert!(p.goes_on_into(&again));
    assert_eq!(p.laser, Some(laser.index));
    assert!(relay.hub().backlog.has_table_change(&laser.load.unwrap()), "for those to come");
    // Another map: not the same load.
    assert!(!p.goes_on_into(&crate::testing::fixture("pass_stadium.dem")));
    want.demo = crate::testing::fixture("pass_stadium.dem");
    assert!(p.open_wanted(&want, &playback).is_continue());
    assert!(relay.hub().clients.values().next().unwrap().reconnecting);
}

/// Going into a demo played before (back into an earlier part of the match), playback starts
/// from the checkpoints made of it then: it stands where it would decoded from the demo's start,
/// and one coming in later is given the same.
#[test]
fn going_into_a_demo_played_before_starts_from_its_checkpoints() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let next = dir.join("next.dem");
    std::fs::copy(crate::testing::fixture("pl_upward.dem"), &next).unwrap();
    let went = |with_checkpoints: bool| {
        let mut b = bench_with_one_in();
        let (relay, mut want) = upward_playing_to_a_client(&mut b);
        let mut p = Player::new(&relay);
        p.start_demo(Cursor::open(&want.demo, &relay.hub).unwrap());
        for _ in 0..20 {
            p.step(&want).unwrap();
            let mut hub = relay.hub();
            let end = hub.backlog.len();
            for c in hub.clients.values_mut() {
                (c.cursor, c.gate, c.outgoing) = (end, None, None);
                c.queue.clear();
            }
        }
        let at = p.cur.as_ref().unwrap().tick();
        if with_checkpoints {
            let index = Index::shared(&next);
            let t = Instant::now();
            while index.before(at).is_none_or(|m| i64::from(m.tick) + 4000 < at) {
                assert!(t.elapsed() < Duration::from_secs(20), "its checkpoints");
                std::thread::sleep(Duration::from_millis(5));
            }
            p.past.push(index);
        }
        want.demo.clone_from(&next);
        assert!(p.goes_on_into(&want.demo));
        let c = p.cur.as_ref().unwrap();
        assert_eq!(c.tick(), at);
        let backlog: Vec<BitBuf> = relay.hub().backlog.entries().map(|e| e.bits.clone()).collect();
        let whole = Cursor::open_detached(&next).unwrap();
        (at, c.tables.len(), backlog, c.dec.set_tables_back(&whole.dec).map(|m| m.len()))
    };
    let (plain, checkpointed) = (went(false), went(true));
    assert!(plain.1 > 0 && plain == checkpointed);
}
