//! A snapshot: the full state of the match.

use super::{
    BuildingKind, Class, Condition, FlagStatus, MedigunKind, PlayerId, Position, ProjectileKind, Resist, Round, Team,
    Ubered,
};
use serde::{Deserialize, Deserializer, Serialize};

/// Reads a field that may be null into `Some`, so `Option<Option<T>>` tells absent (None) from
/// null (`Some(None)`); use with `#[serde(default)]`.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}
/// The full state of the match (not a delta).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Server tick.
    pub tick: u32,
    /// Everyone on the server but the `SourceTV` bot and those who left.
    pub players: Vec<Player>,
    /// Projectiles in flight.
    pub projectiles: Vec<Projectile>,
    /// Buildings, except those being carried.
    pub buildings: Vec<Building>,
    /// Control points, carts and flags.
    pub objectives: Vec<Objective>,
    /// The round state.
    pub round: Round,
    /// Seconds since the round went live (stopping at its end), or none before.
    pub round_time: Option<f64>,
    /// Seconds left of `mp_timelimit`, if one is set.
    pub time_left: Option<f64>,
    /// `mp_winlimit`, if set.
    pub win_limit: Option<i64>,
    /// Seconds since the match started.
    pub match_time: Option<f64>,
    /// Rounds won.
    pub score: Score,
    /// Flags captured this round, on maps with flags; goals scored on pass time maps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captures: Option<Score>,
    /// Robot or player destruction points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<Points>,
    /// Mann vs. Machine's wave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mvm: Option<Mvm>,
    /// The round timers the HUD shows.
    pub timers: Vec<Timer>,
}

/// A player in a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Player {
    /// User id.
    pub id: PlayerId,
    /// Entity index.
    pub ent: u32,
    /// Name, as the player set it: any Unicode.
    pub name: String,
    /// `SteamID` (`[U:1:…]`), or `BOT`.
    pub steamid: String,
    /// Team.
    pub team: Team,
    /// Class.
    pub class: Class,
    /// Alive or dead.
    pub alive: bool,
    /// Health.
    pub hp: u16,
    /// Maximum health.
    pub max_hp: u16,
    /// Where they are, or where they died (none if before connecting).
    pub pos: Option<Position>,
    /// View direction in degrees, 0 = +x, counter-clockwise.
    pub yaw: Option<f64>,
    /// View pitch in degrees.
    pub pitch: Option<f64>,
    /// Medigun charge percentage.
    pub uber: u8,
    /// Scoreboard ping.
    pub ping: u16,
    /// Conditions they are in.
    pub conds: Vec<Condition>,
    /// The medigun charge they are under.
    pub ubered: Option<Ubered>,
    /// A medic's medigun.
    pub medigun: Option<Medigun>,
    /// The weapon in hand.
    pub weapon: Option<Weapon>,
    /// A pyro's flamethrower while it fires.
    pub flames: Option<Flames>,
    /// Seconds until a dead player respawns, once the server has set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub respawn: Option<f64>,
    /// The scoreboard line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<PlayerScore>,
    /// The building an engineer is carrying.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrying: Option<BuildingKind>,
}

/// A player's scoreboard line. Points, kills and deaths are the server's; assists, damage and
/// healing are counted from events, as logs.tf does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "the scoreboard's columns")]
pub struct PlayerScore {
    pub points: i64,
    pub kills: i64,
    pub deaths: i64,
    pub assists: i64,
    pub damage: i64,
    pub healing: i64,
    pub dominations: i64,
    /// Current killstreak.
    pub streak: i64,
}

/// A weapon in hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Weapon {
    /// Item definition index.
    pub item: i64,
    /// Rounds loaded, for a weapon with a clip.
    pub clip: Option<i64>,
}

/// A firing flamethrower.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flames {
    /// Crit flames.
    pub crit: bool,
    /// Hurting someone.
    pub hit: bool,
}

/// A medic's medigun.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Medigun {
    /// Which one.
    pub kind: MedigunKind,
    /// Charge being released.
    pub releasing: bool,
    /// The vaccinator's resistance.
    pub resist: Option<Resist>,
    /// Who it heals.
    pub target: Option<PlayerId>,
}

/// A projectile in flight.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Projectile {
    /// Entity index.
    pub id: u32,
    /// What it is.
    pub kind: ProjectileKind,
    /// Its rotation in degrees.
    pub yaw: f64,
    /// Team.
    pub team: Team,
    /// Position.
    pub pos: Position,
    /// Who fired it.
    pub owner: Option<PlayerId>,
    /// Crit.
    pub crit: bool,
}

/// A building.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Building {
    /// Entity index.
    pub id: u32,
    /// Sentry, dispenser or teleporter.
    pub kind: BuildingKind,
    /// Team.
    pub team: Team,
    /// Upgrade level.
    pub level: u8,
    /// Health.
    pub hp: u16,
    /// Maximum health.
    pub max_hp: u16,
    /// Position.
    pub pos: Position,
    /// Direction it was placed in, in degrees.
    pub yaw: f64,
    /// The engineer.
    pub builder: Option<PlayerId>,
    /// Sapped.
    pub sapped: bool,
    /// Teleporters: entrance or exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrance: Option<bool>,
    /// Sentries: a mini-sentry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mini: Option<bool>,
    /// Sentries: still being built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub building: Option<bool>,
    /// Sentries whose turret has been seen: the direction it aims, in degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aim: Option<f64>,
    /// Sentries whose turret has been seen: who it targets (`Some(None)`: nobody).
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub target: Option<Option<PlayerId>>,
    /// Sentries whose turret has been seen: firing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attacking: Option<bool>,
}

/// An objective, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Objective {
    /// A control point.
    ControlPoint {
        /// Its index, as `point_captured` and the other point events give it.
        index: u8,
        /// Where it is.
        pos: Option<Position>,
        /// The team that owns it.
        owner: Team,
        /// The team capturing it.
        capping_team: Team,
        /// Capture progress, 0 to 1.
        cap_progress: f64,
        /// Progress per second (negative while it decays).
        cap_rate: f64,
        /// Locked.
        locked: bool,
        /// Blocked by a defender.
        blocked: bool,
        /// Players on the point.
        on_point: OnPoint,
    },
    /// A payload cart.
    Cart {
        /// Entity index.
        id: u32,
        /// Position.
        pos: Position,
        /// The pushing team.
        team: Team,
        /// Progress, 0 to 1.
        progress: f64,
        /// Players pushing.
        cappers: i64,
        /// Speed level (negative when rolling back).
        speed: i64,
    },
    /// An intelligence briefcase, or a neutral flag such as the bball ball.
    Flag {
        /// Entity index.
        id: u32,
        /// Position (its carrier's while carried).
        pos: Position,
        /// Team.
        team: Team,
        /// Where it is.
        status: FlagStatus,
        /// Who carries it.
        carrier: Option<PlayerId>,
        /// Seconds until a dropped flag returns.
        returns_in: Option<f64>,
        /// What it is worth, when the mode counts that and it is not 0: the points a robot
        /// destruction reactor holds to be stolen, the bottles a player destruction pickup holds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        points: Option<i64>,
    },
    /// The pass time ball.
    Ball {
        /// Entity index.
        id: u32,
        /// Position (its carrier's while carried).
        pos: Position,
        /// The team of the player who has it or last had it; none when neutral.
        team: Team,
        /// Who carries it.
        carrier: Option<PlayerId>,
    },
    /// A pass time goal.
    Goal {
        /// Entity index.
        id: u32,
        /// Position.
        pos: Position,
        /// The team that scores in it.
        team: Team,
    },
    /// A robot destruction robot.
    Robot {
        /// Entity index.
        id: u32,
        /// Position.
        pos: Position,
        /// Team.
        team: Team,
        /// Health.
        hp: u16,
        /// Maximum health.
        max_hp: u16,
    },
    /// A robot destruction core, dropped by a destroyed robot until a player collects it.
    Core {
        /// Entity index.
        id: u32,
        /// Position.
        pos: Position,
        /// The team of the robot that dropped it.
        team: Team,
    },
    /// A Mann vs. Machine revive marker: a dead player a medic can revive.
    Revive {
        /// Entity index.
        id: u32,
        /// Position.
        pos: Position,
        /// The dead player.
        owner: Option<PlayerId>,
    },
    /// Mann vs. Machine money on the ground.
    Money {
        /// Entity index.
        id: u32,
        /// Position.
        pos: Position,
    },
    /// A Mann vs. Machine tank.
    Tank {
        /// Entity index.
        id: u32,
        /// Position.
        pos: Position,
        /// Health left, 0 to 1.
        health: f64,
    },
}

/// Robot and player destruction points, or what a map that reuses their logic counts with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Points {
    /// Red's points.
    pub red: i64,
    /// Blue's points.
    pub blue: i64,
    /// Points to win, when the map sets it.
    pub max: Option<i64>,
    /// Player destruction: red's team leader.
    pub red_leader: Option<PlayerId>,
    /// Player destruction: blue's team leader.
    pub blue_leader: Option<PlayerId>,
}

/// Mann vs. Machine's wave.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mvm {
    /// The current wave, from 1.
    pub wave: u16,
    /// Waves in the mission.
    pub waves: u16,
    /// Robots in the current wave.
    pub enemies: u16,
    /// Between waves (players are upgrading and readying up).
    pub between_waves: bool,
    /// Money lying on the ground.
    pub money: i64,
    /// The robots of the wave, as its HUD lists them.
    pub classes: Vec<MvmClass>,
}

/// One kind of robot in a Mann vs. Machine wave.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(clippy::struct_excessive_bools, reason = "TF2's class flags")]
pub struct MvmClass {
    /// Its icon's name (`scout`, `soldier_barrage`, `sentry_buster`, …).
    pub name: String,
    /// How many are left to fight.
    pub count: u16,
    /// Some are on the field now.
    pub active: bool,
    /// Giant robots.
    pub giant: bool,
    /// Always crit.
    pub crit: bool,
    /// Support robots, which keep coming and do not count towards the wave.
    pub support: bool,
    /// Mission robots: sentry busters, spies, engineers, sent at the players.
    pub mission: bool,
}

/// Players on a control point, per team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "per team")]
pub struct OnPoint {
    pub red: u8,
    pub blue: u8,
}

/// A count per team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "per team")]
pub struct Score {
    pub red: i64,
    pub blue: i64,
}

/// A round timer on the HUD.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timer {
    /// Whose timer (on King of the Hill each team has one).
    pub team: Team,
    /// Seconds left.
    pub remaining: f64,
    /// Paused.
    pub paused: bool,
    /// The setup timer.
    pub setup: bool,
}
