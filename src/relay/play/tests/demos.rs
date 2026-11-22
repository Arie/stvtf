use super::*;

/// The match loads the laser after the clients were given the relay's: they sign on again, after
/// which the match's own is used.
#[test]
fn the_match_loading_the_laser_after_the_relays_signs_the_clients_on_again() {
    let c = upward_until(four_frames_kept);
    let ours = c.dec.writer().beam_model(None, ModelsSent::default()).unwrap().index;
    let past = ModelsSent::next_for_tests(ours + 1);
    let theirs = c.dec.writer().beam_model(None, past).unwrap();
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
    assert_eq!(p.cur.as_ref().unwrap().dec.writer().beam_model(None, ModelsSent::default()).unwrap().index, theirs);
    p.step(&want).unwrap();
    assert!(p.cur.is_none(), "a new signon");
}

/// Kept beams are drawn only to clients in the game, past a pending full update: at a demo's
/// start, before playback skims to the wanted moment, the relay loads no laser of its own. Once
/// the match loaded its own after the relay's, the relay's is not loaded again for that demo.
#[test]
fn the_relays_laser_is_loaded_once_per_demo_whose_match_loads_its_own() {
    let c = upward_until(four_frames_kept);
    let ours = c.dec.writer().beam_model(None, ModelsSent::default()).unwrap().index;
    let load = c.dec.writer().beam_model(None, ModelsSent::next_for_tests(ours + 1)).unwrap().load.unwrap();
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

/// Far ahead, back, and ahead again from a checkpoint: the clients' string tables are the demo's
/// each time, and the checkpoint gives them only the changes past those they had.
#[test]
fn a_jump_ahead_from_a_checkpoint_gives_the_clients_only_the_table_changes_passed_over() {
    let path = crate::testing::fixture("pl_upward.dem");
    let mut b = bench_with_one_in();
    let first = upward_until(|c| c.started);
    let from = i64::from(first.dec.server_tick());
    let (relay, _, mut want) = relay_playing(&mut b, first, 0);
    relay.hub().start_playing();
    let c = Cursor::open(&path, &relay.hub).unwrap();
    let signon = relay.hub().backlog.len();
    let mut p = Player::new(&relay);
    p.start_demo(c);
    let index = Index::shared(&path);
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

/// Far ahead, then back to the start: the clients' tables are the demo's.
#[test]
fn far_ahead_and_back_to_the_start_leaves_the_clients_with_the_demos_tables() {
    let path = crate::testing::fixture("pl_upward.dem");
    let mut b = bench_with_one_in();
    let first = upward_until(|c| c.started);
    let from = i64::from(first.dec.server_tick());
    let (relay, _, mut want) = relay_playing(&mut b, first, 0);
    relay.hub().start_playing();
    let c = Cursor::open(&path, &relay.hub).unwrap();
    let signon = relay.hub().backlog.len();
    let mut p = Player::new(&relay);
    p.start_demo(c);
    want.paused = true;
    let mut go = |p: &mut Player<'_>, tick: i64| {
        want.tick = u32::try_from(tick).unwrap();
        want.at = Instant::now();
        for _ in 0..20 {
            p.step(&want).unwrap();
        }
    };
    go(&mut p, from + 13_000);
    assert!(client_tables_are_the_demos(&relay, &p, signon).is_some(), "ahead");
    assert_eq!(game_client_table_differs(&relay, &p, 0, "instancebaseline"), None, "ahead");
    go(&mut p, 0);
    assert!(client_tables_are_the_demos(&relay, &p, signon).is_some(), "back at the start");
    assert_eq!(game_client_table_differs(&relay, &p, 0, "instancebaseline"), None, "back at the start");
    go(&mut p, from + 400);
    assert!(client_tables_are_the_demos(&relay, &p, signon).is_some(), "on from the start");
    assert_eq!(game_client_table_differs(&relay, &p, 0, "instancebaseline"), None, "on from the start");
}

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
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
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
}
