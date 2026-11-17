//! What stvtf produces: one [`Line`] per event, snapshot or status change.
//!
//! Each type serializes to the JSON the README's Output section describes (one object per line,
//! tagged by `type`), and reads back from it, so saved output can be loaded into the same types.

mod events;
mod kinds;
mod line;
mod round;
mod snapshot;
mod status;

pub use self::events::{
    Airblast, Announcer, BallCarrierKilled, Banner, BlastJump, BuildingDestroyed, BuildingRepair, CaptureBlocked,
    CaptureBroken, CaptureStarted, Chat, ChatColor, ChatPart, ClassChange, Damage, Explosion, Flag, GameOver, Heal,
    Hint, HudText, Impact, Kill, KilledCapper, Mark, MedicCharge, MedicDeath, MedicDefended, Moment, NpcDamage,
    ObjectChange, PassBlocked, PassCaught, PassFree, PassGet, PassScore, PassStolen, Pause, Pickup, PlayerLeave,
    PlayerMoment, PointCaptured, PointLocked, PointsScored, Reflect, RobotKilled, RocketOpen, RoundSelected,
    RoundStart, RoundWin, Sapped, ScoreMethod, Scores, SentryShot, ServerText, Shot, Spawn, Stun, TeamChange,
    TeamLeaderKilled, Teleported, TimeAdded, TimeRemaining, Tournament, Uber, Upgrade, Voice, VoiceCommand, VoteCast,
    VoteFailed, VoteOptions, VotePassed, VoteStarted,
};
pub use self::kinds::{
    BannerKind, BuildingKind, ChatKind, Class, Condition, FlagAction, FlagStatus, HealSource, ImpactKind, JumpKind,
    JumpPhase, MedigunKind, PickupItem, PickupSize, PlayerId, ProjectileKind, Resist, Round, Team, Ubered,
};
pub use self::line::{Line, Stamped};
pub use self::round::{Position, round1, round2, round3};
pub use self::snapshot::{
    Building, Flames, Medigun, Mvm, MvmClass, Objective, OnPoint, Player, PlayerScore, Points, Projectile, Score,
    Snapshot, Stopwatch, StopwatchState, Timer, Weapon,
};
pub use self::status::{Server, Status, World};

#[cfg(test)]
mod tests;
