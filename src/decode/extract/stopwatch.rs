//! Stopwatch, as TF2's HUD shows it (`CHudStopWatch::OnTick`, `tf_hud_tournament.cpp`).

use super::Extractor;
use super::convert::team_num;
use crate::output::{self as out, Round, StopwatchState, round1};

/// `m_iRole`: `TEAM_ROLE_DEFENDERS`, `TEAM_ROLE_ATTACKERS`.
const ROLE_DEFENDERS: i64 = 1;
const ROLE_ATTACKERS: i64 = 2;
/// `m_nStopWatchState`: `STOPWATCH_CAPTURE_TIME_NOT_SET`, `STOPWATCH_RUNNING`, `STOPWATCH_OVERTIME`.
const STATE_RUNNING: i64 = 1;
const STATE_OVERTIME: i64 = 2;

impl Extractor {
    /// The stopwatch, while TF2 shows it: in stopwatch, between pre-match and game over, with
    /// an attacking and a defending team.
    pub(super) fn stopwatch(&self) -> Option<out::Stopwatch> {
        let sw = &self.stopwatch;
        if !sw.on || matches!(self.round_state, Round::Init | Round::Pregame | Round::GameOver) {
            return None;
        }
        let role = |r: i64| self.ents.teams.values().find(|t| t.role == r);
        let (attackers, defenders) = (role(ROLE_ATTACKERS)?, role(ROLE_DEFENDERS)?);
        let timer = sw.timer.and_then(|idx| self.ents.timers.get(&idx));
        let setting = timer.is_some_and(|t| t.watching_caps);
        let (state, points) = match sw.state {
            STATE_RUNNING if setting => (StopwatchState::Running, attackers.score),
            STATE_RUNNING => (StopwatchState::Running, defenders.score - attackers.score),
            STATE_OVERTIME => (StopwatchState::Overtime, defenders.score - attackers.score + 1),
            _ => (StopwatchState::Unset, attackers.score),
        };
        Some(out::Stopwatch {
            attackers: team_num(attackers.team),
            setting,
            state,
            time: round1(timer.map_or(0.0, |t| t.remaining_at(self.server_tick, self.interval()))),
            paused: timer.is_none_or(|t| t.paused || setting),
            points,
        })
    }
}
