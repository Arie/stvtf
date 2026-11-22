use super::*;
use crate::output::StopwatchState;

/// Stopwatch with BLU (3 points) attacking RED (`red` points), its timer entity 9.
fn with_stopwatch(state: i64, timer: &RoundTimer, red: i64) -> Extractor {
    let mut x = Extractor {
        round_state: Round::Running,
        stopwatch: StopwatchRules { on: true, state, timer: Some(9) },
        ..Default::default()
    };
    x.ents.teams.insert(1, TeamScore { team: 2, score: red, role: 1, ..Default::default() });
    x.ents.teams.insert(2, TeamScore { team: 3, score: 3, role: 2, ..Default::default() });
    x.ents.timers.insert(9, RoundTimer { stopwatch: true, ..timer.clone() });
    x
}

fn stopwatch(state: i64, timer: &RoundTimer, red: i64) -> Option<out::Stopwatch> {
    with_stopwatch(state, timer, red).snapshot().stopwatch
}

#[test]
fn setting_the_time_shows_it_as_of_the_last_capture() {
    let timer = RoundTimer { watching_caps: true, total: 92.84, end_time: 1.0, ..Default::default() };
    let sw = stopwatch(1, &timer, 0).unwrap();
    assert_eq!((sw.attackers, sw.setting, sw.state), (out::Team::Blue, true, StopwatchState::Running));
    assert_eq!((sw.time, sw.paused, sw.points), (92.8, true, 3), "the attackers' points");
}

#[test]
fn beating_it_counts_down_with_the_points_still_to_get() {
    let timer = RoundTimer { end_time: 100.0, ..Default::default() };
    let sw = stopwatch(1, &timer, 4).unwrap();
    assert_eq!((sw.setting, sw.time, sw.paused, sw.points), (false, 100.0, false, 1));
    let sw = stopwatch(2, &RoundTimer { paused: true, ..timer }, 4).unwrap();
    assert_eq!((sw.state, sw.paused, sw.points), (StopwatchState::Overtime, true, 2), "one more than the time");
}

#[test]
fn its_time_is_said_to_be_on_the_hud_only_while_the_round_timer_shows_it() {
    let mut x = with_stopwatch(1, &RoundTimer { end_time: 100.0, ..Default::default() }, 4);
    x.ents.timers.insert(5, RoundTimer { show: true, end_time: 150.0, ..Default::default() });
    assert!(!x.snapshot().stopwatch.unwrap().in_hud, "the map's own time, longer");
    x.ents.timers.get_mut(&5).unwrap().end_time = 100.0;
    assert!(x.snapshot().stopwatch.unwrap().in_hud, "held to the stopwatch's");
    x.ents.timers.get_mut(&5).unwrap().setup = true;
    assert!(!x.snapshot().stopwatch.unwrap().in_hud, "setup is another time");
}

#[test]
fn no_stopwatch_without_one_or_both_roles() {
    assert!(stopwatch(0, &RoundTimer::default(), 0).is_some());
    let mut x = Extractor {
        round_state: Round::Running,
        stopwatch: StopwatchRules { on: true, ..Default::default() },
        ..Default::default()
    };
    x.ents.teams.insert(2, TeamScore { team: 3, role: 2, ..Default::default() });
    assert_eq!(x.snapshot().stopwatch, None, "no defenders");
    x.ents.teams.insert(1, TeamScore { team: 2, role: 1, ..Default::default() });
    assert!(x.snapshot().stopwatch.is_some());
    x.round_state = Round::GameOver;
    assert_eq!(x.snapshot().stopwatch, None, "game over");
    x.round_state = Round::Running;
    x.stopwatch.on = false;
    assert_eq!(x.snapshot().stopwatch, None);
}
