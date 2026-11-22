use super::*;

fn at(x: &mut Extractor, state: i64, transition: f32, seconds: f32) -> Option<f64> {
    x.round_state = round_from_raw(state);
    x.state_transition = transition;
    x.server_tick = (seconds / DEFAULT_TICK_INTERVAL).round() as u32;
    x.track_round_time();
    x.round_time().map(round1)
}

#[test]
fn time_left_counts_down_from_the_match_start() {
    let mut x = Extractor::default();
    assert_eq!(x.time_left(), None, "no mp_timelimit");
    x.time_limit = Some(30.0);
    x.match_start = 200.0;
    x.server_tick = (500.0 / DEFAULT_TICK_INTERVAL) as u32;
    assert!((x.time_left().unwrap() - 1500.0).abs() < 0.1, "{:?}", x.time_left());
    x.server_tick = (3000.0 / DEFAULT_TICK_INTERVAL) as u32;
    assert_eq!(x.time_left(), Some(0.0));
}

#[test]
fn round_time_runs_while_live_stops_at_the_win_and_resets() {
    let mut x = Extractor::default();
    assert_eq!(at(&mut x, 4, 100.0, 120.0), Some(20.0));
    assert_eq!(at(&mut x, 5, 160.0, 135.0), Some(35.0), "round won at 135 s");
    assert_eq!(at(&mut x, 5, 160.0, 150.0), Some(35.0), "frozen while the winners celebrate");
    assert_eq!(at(&mut x, 3, 170.0, 165.0), None, "pre-round of the next round");
    assert_eq!(at(&mut x, 4, 170.0, 180.0), Some(10.0));
}

#[test]
fn timer_counts_down_in_server_time_unless_paused() {
    let mut t = RoundTimer { end_time: 100.0, remaining: 42.0, ..Default::default() };
    let at = |t: &RoundTimer, tick| t.remaining_at(tick, DEFAULT_TICK_INTERVAL);
    assert!((at(&t, 4000) - 40.0).abs() < 1e-3, "tick 4000 is 60 s of server time");
    assert_eq!(at(&t, 9000), 0.0);
    t.paused = true;
    assert_eq!(at(&t, 4000), 42.0);
}
