//! What stvtf produces: one [`Line`] per event, snapshot or status change.
//!
//! Each type serializes to the JSON the README's Output section describes (one object per line,
//! tagged by `type`), and reads back from it, so saved output can be loaded into the same types.

use serde::{Deserialize, Serialize};

/// A position in Hammer units, rounded to a tenth.
pub type Position = [f64; 3];

/// To a tenth: positions, angles, seconds.
pub fn round1(f: impl Into<f64>) -> f64 {
    (f.into() * 10.0).round() / 10.0
}

/// To a hundredth: progress, screen positions.
pub fn round2(f: impl Into<f64>) -> f64 {
    (f.into() * 100.0).round() / 100.0
}

/// To a thousandth: rates, fractions.
pub fn round3(f: impl Into<f64>) -> f64 {
    (f.into() * 1000.0).round() / 1000.0
}

mod events;
mod kinds;
mod snapshot;
#[cfg(test)]
mod tests;

pub use events::*;
pub use kinds::*;
pub use snapshot::*;

/// The `Line` enum and its `tick()`: every line but a status carries the tick it happened at, so
/// listing a variant once gives both.
macro_rules! lines {
    ($($variant:ident($payload:ty),)*) => {
        /// One line of output.
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        #[expect(missing_docs, reason = "each variant is documented on the type it holds")]
        pub enum Line {
            Status(Status),
            $($variant($payload),)*
        }

        impl Line {
            /// The server tick of an event or snapshot (66.67 per second, restarting at each map
            /// load); none for a status.
            pub fn tick(&self) -> Option<u32> {
                match self {
                    Line::Status(_) => None,
                    $(Line::$variant(line) => Some(line.tick),)*
                }
            }
        }
    };
}

lines! {
    Server(Server),
    World(World),
    Snapshot(Snapshot),
    Kill(Kill),
    Damage(Damage),
    Spawn(Spawn),
    Uber(Uber),
    UberReady(MedicCharge),
    UberEnded(MedicCharge),
    TeamChange(TeamChange),
    ClassChange(ClassChange),
    PlayerLeave(PlayerLeave),
    Build(ObjectChange),
    BuildingUpgrade(ObjectChange),
    BuildingRepair(BuildingRepair),
    BuildingDestroyed(BuildingDestroyed),
    TimeAdded(TimeAdded),
    RoundStart(RoundStart),
    RoundActive(Moment),
    SetupFinished(Moment),
    RoundWin(RoundWin),
    Stalemate(Moment),
    Overtime(Moment),
    Scores(Scores),
    GameOver(GameOver),
    PointCaptured(PointCaptured),
    CaptureBlocked(CaptureBlocked),
    CaptureBroken(CaptureBroken),
    KilledCapper(KilledCapper),
    Flag(Flag),
    MedicDeath(MedicDeath),
    MedicDefended(MedicDefended),
    Reflect(Reflect),
    Banner(Banner),
    Shot(Shot),
    Explosion(Explosion),
    SentryShot(SentryShot),
    Airblast(Airblast),
    Chat(Chat),
    Heal(Heal),
    BlastJump(BlastJump),
    Pickup(Pickup),
    Impact(Impact),
    Mark(Mark),
    VoteOptions(VoteOptions),
    VoteCast(VoteCast),
    Tournament(Tournament),
    Announcer(Announcer),
    TimeRemaining(TimeRemaining),
    VoiceCommand(VoiceCommand),
    ServerText(ServerText),
    PassGet(PassGet),
    PassFree(PassFree),
    PassCaught(PassCaught),
    PassScore(PassScore),
    PassStolen(PassStolen),
    PassBlocked(PassBlocked),
    BallCarrierKilled(BallCarrierKilled),
    RobotKilled(RobotKilled),
    PointsScored(PointsScored),
    SpecialScore(PlayerMoment),
    TeamLeaderKilled(TeamLeaderKilled),
    RocketOpen(RocketOpen),
    RoundSelected(RoundSelected),
    Teleported(Teleported),
    Stun(Stun),
    Sapped(Sapped),
    BuildingRemoved(ObjectChange),
    BuildingDetonated(ObjectChange),
    DetStickies(PlayerMoment),
    CaptureStarted(CaptureStarted),
    PointLocked(PointLocked),
    NpcDamage(NpcDamage),
    HudText(HudText),
    Hint(Hint),
    VoteStarted(VoteStarted),
    VotePassed(VotePassed),
    VoteFailed(VoteFailed),
    Upgrade(Upgrade),
}

impl Line {
    /// Every player an event names (not those in a snapshot), in field order.
    pub fn players(&self) -> impl Iterator<Item = PlayerId> + '_ {
        let one = |id: PlayerId| [Some(id), None, None];
        let (ids, more): ([Option<PlayerId>; 3], &[Option<PlayerId>]) = match self {
            Line::Status(_)
            | Line::Server(_)
            | Line::World(_)
            | Line::Snapshot(_)
            | Line::BuildingRepair(_)
            | Line::TimeAdded(_)
            | Line::RoundStart(_)
            | Line::RoundActive(_)
            | Line::SetupFinished(_)
            | Line::RoundWin(_)
            | Line::Stalemate(_)
            | Line::Overtime(_)
            | Line::Scores(_)
            | Line::GameOver(_)
            | Line::CaptureBroken(_)
            | Line::Explosion(_)
            | Line::Impact(_)
            | Line::Mark(_)
            | Line::VoteOptions(_)
            | Line::Announcer(_)
            | Line::TimeRemaining(_)
            | Line::ServerText(_)
            | Line::RocketOpen(_)
            | Line::RoundSelected(_)
            | Line::PointLocked(_)
            | Line::HudText(_)
            | Line::Hint(_)
            | Line::VotePassed(_)
            | Line::VoteFailed(_) => ([None; 3], &[]),
            Line::PassFree(f) => ([Some(f.player), f.attacker, None], &[]),
            Line::PassCaught(c) => ([Some(c.passer), Some(c.catcher), None], &[]),
            Line::PassScore(sc) => ([Some(sc.scorer), sc.assister, None], &[]),
            Line::PassStolen(st) => ([Some(st.victim), Some(st.attacker), None], &[]),
            Line::PassBlocked(b) => ([Some(b.owner), Some(b.blocker), None], &[]),
            Line::BallCarrierKilled(k) => ([k.attacker, Some(k.victim), k.assister], &[]),
            Line::RobotKilled(r) => ([r.attacker, None, None], &[]),
            Line::TeamLeaderKilled(k) => ([k.killer, Some(k.victim), None], &[]),
            Line::Teleported(t) => ([Some(t.player), t.builder, None], &[]),
            Line::Stun(st) => ([st.stunner, Some(st.victim), None], &[]),
            Line::Sapped(sa) => ([Some(sa.player), Some(sa.owner), None], &[]),
            Line::CaptureStarted(c) => ([None; 3], &c.cappers),
            Line::NpcDamage(n) => ([n.attacker, None, None], &[]),
            Line::VoteStarted(v) => ([v.caller, v.target, None], &[]),
            Line::Kill(k) => ([Some(k.victim), k.attacker, k.assister], &[]),
            Line::Damage(d) => ([Some(d.victim), d.attacker, None], &[]),
            Line::Uber(u) => ([Some(u.medic), u.target, None], &[]),
            Line::UberReady(c) | Line::UberEnded(c) => (one(c.medic), &[]),
            Line::Spawn(Spawn { player, .. })
            | Line::TeamChange(TeamChange { player, .. })
            | Line::ClassChange(ClassChange { player, .. })
            | Line::PlayerLeave(PlayerLeave { player, .. })
            | Line::Build(ObjectChange { player, .. })
            | Line::BuildingUpgrade(ObjectChange { player, .. })
            | Line::Banner(Banner { player, .. })
            | Line::Airblast(Airblast { player, .. })
            | Line::BlastJump(BlastJump { player, .. })
            | Line::Pickup(Pickup { player, .. })
            | Line::VoiceCommand(VoiceCommand { player, .. })
            | Line::PassGet(PassGet { player, .. })
            | Line::PointsScored(PointsScored { player, .. })
            | Line::SpecialScore(PlayerMoment { player, .. })
            | Line::BuildingRemoved(ObjectChange { player, .. })
            | Line::BuildingDetonated(ObjectChange { player, .. })
            | Line::DetStickies(PlayerMoment { player, .. })
            | Line::Upgrade(Upgrade { player, .. }) => (one(*player), &[]),
            Line::Shot(Shot { player, .. })
            | Line::Chat(Chat { player, .. })
            | Line::VoteCast(VoteCast { player, .. })
            | Line::Tournament(Tournament { player, .. }) => ([*player, None, None], &[]),
            Line::BuildingDestroyed(b) => ([Some(b.owner), b.attacker, None], &[]),
            Line::PointCaptured(p) => ([None; 3], &p.cappers),
            Line::CaptureBlocked(b) => ([b.blocker, b.victim, None], &[]),
            Line::KilledCapper(k) => ([Some(k.killer), Some(k.victim), k.assister], &[]),
            Line::Flag(f) => ([f.player, f.carrier, None], &[]),
            Line::MedicDeath(m) => ([Some(m.medic), m.attacker, None], &[]),
            Line::MedicDefended(m) => ([Some(m.player), Some(m.medic), None], &[]),
            Line::Reflect(r) => ([Some(r.player), r.owner, None], &[]),
            Line::SentryShot(s) => ([s.builder, s.target, None], &[]),
            Line::Heal(h) => ([Some(h.patient), h.healer, None], &[]),
        };
        ids.into_iter().chain(more.iter().copied()).flatten()
    }
}

/// A line as the CLI writes it: live output also has `time`, the receive time in Unix
/// milliseconds (about `tv_delay` after the moment in the game).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stamped {
    /// The line itself.
    #[serde(flatten)]
    pub line: Line,
    /// Receive time in Unix milliseconds; none offline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<u64>,
}

/// The connection's state (`type: status`, tagged by `state`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    /// Connecting to `server`, as a client of `build`.
    Connecting {
        /// The address connected to.
        server: String,
        /// The build offered to the server.
        build: String,
    },
    /// Signon progress.
    New,
    /// Signon progress.
    Prespawn,
    /// Signon progress.
    Spawn,
    /// Data is flowing.
    Full {
        /// The map, once the server has said.
        map: Option<String>,
    },
    /// The server announced a map change.
    Changelevel,
    /// Signon restarts for a new map: reset the view.
    MapChange,
    /// The session ended.
    Disconnected {
        /// Why.
        reason: String,
        /// Messages that could not be decoded.
        decode_errors: u64,
        /// Packets received.
        packets: u64,
        /// Packets lost.
        lost: u64,
    },
    /// A connection attempt failed; another follows.
    ConnectFailed {
        /// Why.
        reason: String,
    },
    /// Watching ended for good. [`crate::watch::watch`] returns an error instead; a host that
    /// reports that error to its own readers sends this.
    GaveUp {
        /// Why.
        reason: String,
    },
    /// The relay is up: clients can watch through `listen` (see [`crate::relay`]).
    Relay {
        /// The address it listens on.
        listen: String,
    },
}

/// The server, at every signon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "the server's own values")]
pub struct Server {
    pub tick: u32,
    pub map: String,
    pub hostname: String,
    /// Seconds per tick.
    pub tick_interval: f64,
    pub max_players: u8,
    pub spawn_count: u32,
}

/// The world's bounds, once per map. They include the 3D skybox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct World {
    pub tick: u32,
    pub min: Position,
    pub max: Position,
}
