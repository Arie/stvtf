use super::*;

#[test]
fn koth_shows_each_teams_timer_and_not_old_ones() {
    let mut x = Extractor::default();
    let shown = RoundTimer { show: true, remaining: 180.0, paused: true, ..Default::default() };
    for idx in [5, 6, 7] {
        x.ents.timers.insert(idx, shown.clone());
    }
    x.koth_timers = [Some(6), Some(5)];
    let teams: Vec<out::Team> = x.snapshot().timers.iter().map(|t| t.team).collect();
    assert_eq!(teams, [out::Team::Blue, out::Team::Red]);
    x.koth_timers = [None; 2];
    assert_eq!(x.snapshot().timers.len(), 3);
}
