//! What the extractor keeps per entity between updates.

use super::MAX_TEAMS;
use crate::decode::modes::Placed;
use crate::output as out;
use tf_demo_parser::demo::data::game_state::Player;
use tf_demo_parser::demo::vector::Vector;

/// Unattended cap progress decays over `mp_capdeteriorate_time` (default 90 s).
const CAP_DECAY_SECONDS: f32 = 90.0;

/// The sentry's yaw pose parameter: 0 to 1 over a full turn, the placed direction at its middle.
const POSE_MIDDLE: f32 = 0.5;
const FULL_TURN: f32 = 360.0;

/// A player's line on the scoreboard. Points, kills and deaths are the server's match totals;
/// damage, healing and assists are counted from events, as logs.tf does.
#[derive(Default, Clone)]
pub(super) struct Stats {
    pub(super) points: i64,
    pub(super) kills: i64,
    pub(super) deaths: i64,
    pub(super) assists: i64,
    pub(super) damage: i64,
    pub(super) healing: i64,
    pub(super) dominations: i64,
    pub(super) streak: i64,
    pub(super) respawn_at: f32,
    pub(super) carrying: bool,
}

impl Stats {
    pub(super) fn score(&self) -> out::PlayerScore {
        out::PlayerScore {
            points: self.points,
            kills: self.kills,
            deaths: self.deaths,
            assists: self.assists,
            damage: self.damage,
            healing: self.healing,
            dominations: self.dominations,
            streak: self.streak,
        }
    }
}

/// A team entity's team number and score.
#[derive(Default, Clone, Copy)]
pub(super) struct TeamScore {
    pub(super) team: u8,
    pub(super) score: i64,
    /// Flags captured this round, the score of a CTF round.
    pub(super) captures: i64,
}

/// Where a player was and where they looked.
#[derive(Clone, Copy)]
pub(super) struct Pose {
    pub(super) pos: Vector,
    pub(super) yaw: f32,
    pub(super) pitch: f32,
}

impl Pose {
    pub(super) fn of(p: &Player) -> Self {
        Pose { pos: p.position, yaw: p.view_angle, pitch: p.pitch_angle }
    }
}

/// A medic's charge and whether it was being released.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct Charge {
    pub(super) charge: u8,
    pub(super) releasing: bool,
}

#[derive(Default, Clone)]
pub(super) struct Sentry {
    /// Turret pose parameters (0..1 over the model's range): [pitch, yaw].
    pub(super) pose: [f32; 2],
    /// The direction the sentry was placed in (tf-demo-parser leaves this 0).
    pub(super) yaw: f32,
    pub(super) enemy: Option<u32>,
    pub(super) shells: Option<i64>,
    pub(super) state: i64,
}

impl Sentry {
    /// World yaw of the turret: the yaw pose parameter covers a full turn, centred on the
    /// direction the sentry was placed in (checked against its targets while firing).
    pub(super) fn aim(&self) -> f32 {
        (self.yaw + (POSE_MIDDLE - self.pose[1]) * FULL_TURN).rem_euclid(FULL_TURN)
    }
}

/// An intelligence briefcase, or a neutral flag such as the bball ball.
#[derive(Default, Clone)]
pub(super) struct Flag {
    pub(super) at: Placed,
    pub(super) status: out::FlagStatus,
    pub(super) disabled: bool,
    pub(super) carrier: Option<u32>,
    /// Server time a dropped flag goes home.
    pub(super) reset_time: f32,
    /// What it is worth: a robot destruction reactor's points, a player destruction pickup's bottles.
    pub(super) points: i64,
    /// Not drawn: a map's spare flag (`pd_watergate` keeps two in the teams' bases).
    pub(super) hidden: bool,
}

#[derive(Default, Clone)]
pub(super) struct TrainWatcher {
    pub(super) team: u8,
    /// The cart model (`m_hGlowEnt`), which is attached to the cart's `func_tracktrain`.
    pub(super) glow: Option<u32>,
    pub(super) progress: f32,
    pub(super) cappers: i64,
    pub(super) speed: i64,
}

// Mirrors the timer entity's netprops.
#[derive(Default, Clone)]
#[expect(clippy::struct_excessive_bools)]
pub(super) struct RoundTimer {
    pub(super) team: u8,
    pub(super) end_time: f32,
    pub(super) paused: bool,
    pub(super) remaining: f32,
    pub(super) show: bool,
    pub(super) disabled: bool,
    pub(super) setup: bool,
}

impl RoundTimer {
    /// Seconds left at a server tick; the end time is in server time (tick * interval).
    pub(super) fn remaining_at(&self, tick: u32, interval: f32) -> f32 {
        if self.paused { self.remaining } else { (self.end_time - tick as f32 * interval).max(0.0) }
    }
}

#[derive(Default)]
pub(super) struct Flamethrower {
    pub(super) owner: Option<u32>,
    pub(super) state: i64,
    pub(super) crit: bool,
    pub(super) hit: bool,
}

#[derive(Default, Clone)]
pub(super) struct Weapon {
    pub(super) item: i64,
    /// -1 and below is TF2's "no clip".
    pub(super) clip: i64,
}

#[derive(Default, Clone)]
pub(super) struct Medigun {
    pub(super) owner: Option<u32>,
    pub(super) item: i64,
    pub(super) releasing: bool,
    pub(super) resist: i64,
    pub(super) target: Option<u32>,
}

#[derive(Default, Clone)]
pub(super) struct ControlPoint {
    pub(super) pos: Option<Vector>,
    pub(super) owner: u8,
    pub(super) capping_team: u8,
    /// Progress 0..1 of the capping team as of `progress_tick`. The server sends it
    /// (as `m_flLazyCapPerc`, counting down) only now and then; in between it is advanced
    /// at `cap_rate`, rolled forward whenever an input of the rate changes.
    pub(super) progress: f32,
    pub(super) progress_tick: u32,
    pub(super) locked: bool,
    pub(super) blocked: bool,
    pub(super) team_in_zone: u8,
    /// Per team: seconds to cap alone, and team members on the point.
    pub(super) cap_time: [f32; MAX_TEAMS],
    pub(super) members: [u8; MAX_TEAMS],
}

impl ControlPoint {
    /// Progress per second towards capping (negative while it decays back), the way
    /// TF2's client extrapolates between lazy updates: each extra capper adds 1/n.
    pub(super) fn cap_rate(&self) -> f32 {
        let team = self.capping_team as usize;
        let time = self.cap_time.get(team).copied().unwrap_or(0.0);
        if team == 0 || time <= 0.0 || self.blocked {
            return 0.0;
        }
        if self.team_in_zone == self.capping_team {
            (1..=self.members[team].max(1)).map(|n| 1.0 / f32::from(n)).sum::<f32>() / time
        } else if self.team_in_zone == 0 {
            -1.0 / CAP_DECAY_SECONDS
        } else {
            0.0
        }
    }

    pub(super) fn progress_at(&self, tick: u32, interval: f32) -> f32 {
        if self.capping_team == 0 {
            return 0.0;
        }
        let elapsed = tick.saturating_sub(self.progress_tick) as f32 * interval;
        (self.progress + self.cap_rate() * elapsed).clamp(0.0, 1.0)
    }

    pub(super) fn advance(&mut self, tick: u32, interval: f32) {
        self.progress = self.progress_at(tick, interval);
        self.progress_tick = tick;
    }
}
