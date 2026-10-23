//! What stvtf produces: one [`Line`] per event, snapshot or status change.
//!
//! Each type serializes to the JSON the README's Output section describes (one object per line,
//! tagged by `type`), and reads back from it, so saved output can be loaded into the same types.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};
use std::fmt;

/// A position in Hammer units, rounded to a tenth.
pub type Position = [f64; 3];

/// A player's user id: what events call `player`, `attacker`, `victim` and so on. Not an entity
/// index; a user id is not reused while the server runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlayerId(pub u16);

impl fmt::Display for PlayerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A team.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Team {
    /// RED.
    Red,
    /// BLU.
    Blue,
    /// Spectators.
    Spectator,
    /// No team (unassigned, or a neutral objective).
    #[serde(rename = "none")]
    Unassigned,
}

/// A player class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[expect(missing_docs, reason = "the nine classes")]
pub enum Class {
    Scout,
    Sniper,
    Soldier,
    Demoman,
    Medic,
    Heavy,
    Pyro,
    Spy,
    Engineer,
    /// Not picked yet.
    Unknown,
}

/// An engineer's building, or a spy's sapper (in events only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[expect(missing_docs, reason = "named after the buildings")]
pub enum BuildingKind {
    Sentry,
    Dispenser,
    Teleporter,
    Sapper,
    Unknown,
}

/// What a projectile is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[expect(missing_docs, reason = "named after the projectiles")]
pub enum ProjectileKind {
    Rocket,
    Pipe,
    Sticky,
    LooseCannon,
    Flare,
    /// The crossbow's.
    HealingArrow,
    /// The huntsman's.
    Arrow,
    /// The Cow Mangler's.
    EnergyBall,
    /// The Righteous Bison's and the Pomson's.
    EnergyRing,
    /// The Dragon's Fury's.
    Fireball,
    Cleaver,
    Jarate,
    MadMilk,
    GasPasser,
    ShortCircuitOrb,
    Unknown,
}

/// Which medigun a medic carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[expect(missing_docs, reason = "named after the mediguns")]
pub enum MedigunKind {
    /// The stock medigun and its reskins.
    Medigun,
    Kritzkrieg,
    Quickfix,
    Vaccinator,
}

/// The damage type a vaccinator resists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[expect(missing_docs, reason = "named after the damage types")]
pub enum Resist {
    Bullet,
    Blast,
    Fire,
}

/// The medigun charge a player is under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[expect(missing_docs, reason = "named after the charges")]
pub enum Ubered {
    Uber,
    Kritz,
    Quickfix,
    VaccinatorBullet,
    VaccinatorBlast,
    VaccinatorFire,
}

/// The round state (`gamerules_roundstate_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[expect(missing_docs, reason = "TF2's round states")]
pub enum Round {
    #[default]
    Init,
    Pregame,
    StartGame,
    Preround,
    Running,
    TeamWin,
    Restart,
    Stalemate,
    GameOver,
    Bonus,
    BetweenRounds,
}

/// Where a flag is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[expect(missing_docs, reason = "self-explanatory")]
pub enum FlagStatus {
    Home,
    Carried,
    Dropped,
}

/// What happened to a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[expect(missing_docs, reason = "self-explanatory")]
pub enum FlagAction {
    Pickup,
    Captured,
    Defended,
    Dropped,
    Returned,
}

/// A soldier's banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[expect(missing_docs, reason = "named after the banners")]
pub enum BannerKind {
    BuffBanner,
    BattalionsBackup,
    Concheror,
    Other,
}

/// What healed a player, as far as the event tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealSource {
    /// A medic's medigun.
    Medigun,
    /// An engineer's dispenser.
    Dispenser,
    /// The crossbow.
    Crossbow,
    /// Health on hit.
    OnHit,
    /// Anything else (a medkit, a regenerating weapon).
    Other,
}

/// What a player blast jumped with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[expect(missing_docs, reason = "self-explanatory")]
pub enum JumpKind {
    Rocket,
    Sticky,
    /// The Thermal Thruster.
    RocketPack,
}

/// Whether a blast jump started or landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[expect(missing_docs, reason = "self-explanatory")]
pub enum JumpPhase {
    Start,
    Land,
}

/// What a player picked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[expect(missing_docs, reason = "self-explanatory")]
pub enum PickupItem {
    Health,
    Ammo,
}

/// The size of a pickup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[expect(missing_docs, reason = "self-explanatory")]
pub enum PickupSize {
    Small,
    Medium,
    Full,
}

/// What a bullet hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImpactKind {
    /// A player.
    Blood,
    /// The world or a building.
    Sparks,
    /// An übered player.
    Ricochet,
}

/// Where a chat line went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[expect(missing_docs, reason = "TF2's chat kinds")]
pub enum ChatKind {
    ChatAll,
    ChatTeam,
    ChatAllDead,
    ChatTeamDead,
    ChatAllSpec,
    NameChange,
    Empty,
    /// Console `say` and server notices.
    SayText,
}

/// A TF2 player condition (`ETFCond`), written by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Condition(u8);

impl Condition {
    /// The condition with this number, if TF2 uses it.
    pub fn from_number(n: u8) -> Option<Condition> {
        CONDITION_NAMES.get(usize::from(n)).filter(|name| !name.is_empty()).map(|_| Condition(n))
    }

    /// TF2's number for it.
    pub fn number(self) -> u8 {
        self.0
    }

    /// Its name, as tf-demo-parser's `PlayerCondition` spells it.
    pub fn name(self) -> &'static str {
        CONDITION_NAMES[usize::from(self.0)]
    }
}

impl fmt::Display for Condition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl Serialize for Condition {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Condition {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        CONDITION_NAMES
            .iter()
            .position(|n| !n.is_empty() && *n == name)
            .and_then(|n| u8::try_from(n).ok())
            .map(Condition)
            .ok_or_else(|| de::Error::custom(format_args!("unknown condition {name:?}")))
    }
}

/// Reads a field that may be null into `Some`, so `Option<Option<T>>` tells absent (None) from
/// null (`Some(None)`); use with `#[serde(default)]`.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// One line of output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[expect(missing_docs, reason = "each variant is documented on the type it holds")]
pub enum Line {
    Status(Status),
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
}

impl Line {
    /// The server tick of an event or snapshot (66.67 per second, restarting at each map load);
    /// none for a status.
    pub fn tick(&self) -> Option<u32> {
        match self {
            Line::Status(_) => None,
            Line::Server(Server { tick, .. })
            | Line::World(World { tick, .. })
            | Line::Snapshot(Snapshot { tick, .. })
            | Line::Kill(Kill { tick, .. })
            | Line::Damage(Damage { tick, .. })
            | Line::Spawn(Spawn { tick, .. })
            | Line::Uber(Uber { tick, .. })
            | Line::UberReady(MedicCharge { tick, .. })
            | Line::UberEnded(MedicCharge { tick, .. })
            | Line::TeamChange(TeamChange { tick, .. })
            | Line::ClassChange(ClassChange { tick, .. })
            | Line::PlayerLeave(PlayerLeave { tick, .. })
            | Line::Build(ObjectChange { tick, .. })
            | Line::BuildingUpgrade(ObjectChange { tick, .. })
            | Line::BuildingRepair(BuildingRepair { tick, .. })
            | Line::BuildingDestroyed(BuildingDestroyed { tick, .. })
            | Line::TimeAdded(TimeAdded { tick, .. })
            | Line::RoundStart(RoundStart { tick, .. })
            | Line::RoundActive(Moment { tick })
            | Line::SetupFinished(Moment { tick })
            | Line::RoundWin(RoundWin { tick, .. })
            | Line::Stalemate(Moment { tick })
            | Line::Overtime(Moment { tick })
            | Line::Scores(Scores { tick, .. })
            | Line::GameOver(GameOver { tick, .. })
            | Line::PointCaptured(PointCaptured { tick, .. })
            | Line::CaptureBlocked(CaptureBlocked { tick, .. })
            | Line::CaptureBroken(CaptureBroken { tick, .. })
            | Line::KilledCapper(KilledCapper { tick, .. })
            | Line::Flag(Flag { tick, .. })
            | Line::MedicDeath(MedicDeath { tick, .. })
            | Line::MedicDefended(MedicDefended { tick, .. })
            | Line::Reflect(Reflect { tick, .. })
            | Line::Banner(Banner { tick, .. })
            | Line::Shot(Shot { tick, .. })
            | Line::Explosion(Explosion { tick, .. })
            | Line::SentryShot(SentryShot { tick, .. })
            | Line::Airblast(Airblast { tick, .. })
            | Line::Chat(Chat { tick, .. })
            | Line::Heal(Heal { tick, .. })
            | Line::BlastJump(BlastJump { tick, .. })
            | Line::Pickup(Pickup { tick, .. })
            | Line::Impact(Impact { tick, .. })
            | Line::Mark(Mark { tick, .. })
            | Line::VoteOptions(VoteOptions { tick, .. })
            | Line::VoteCast(VoteCast { tick, .. })
            | Line::Tournament(Tournament { tick, .. })
            | Line::Announcer(Announcer { tick, .. })
            | Line::TimeRemaining(TimeRemaining { tick, .. })
            | Line::VoiceCommand(VoiceCommand { tick, .. })
            | Line::ServerText(ServerText { tick, .. })
            | Line::PassGet(PassGet { tick, .. })
            | Line::PassFree(PassFree { tick, .. })
            | Line::PassCaught(PassCaught { tick, .. })
            | Line::PassScore(PassScore { tick, .. })
            | Line::PassStolen(PassStolen { tick, .. })
            | Line::PassBlocked(PassBlocked { tick, .. })
            | Line::BallCarrierKilled(BallCarrierKilled { tick, .. })
            | Line::RobotKilled(RobotKilled { tick, .. })
            | Line::PointsScored(PointsScored { tick, .. })
            | Line::SpecialScore(PlayerMoment { tick, .. })
            | Line::TeamLeaderKilled(TeamLeaderKilled { tick, .. })
            | Line::RocketOpen(RocketOpen { tick, .. })
            | Line::RoundSelected(RoundSelected { tick, .. })
            | Line::Teleported(Teleported { tick, .. })
            | Line::Stun(Stun { tick, .. })
            | Line::Sapped(Sapped { tick, .. })
            | Line::BuildingRemoved(ObjectChange { tick, .. })
            | Line::BuildingDetonated(ObjectChange { tick, .. })
            | Line::DetStickies(PlayerMoment { tick, .. })
            | Line::CaptureStarted(CaptureStarted { tick, .. })
            | Line::PointLocked(PointLocked { tick, .. })
            | Line::NpcDamage(NpcDamage { tick, .. })
            | Line::HudText(HudText { tick, .. })
            | Line::Hint(Hint { tick, .. })
            | Line::VoteStarted(VoteStarted { tick, .. })
            | Line::VotePassed(VotePassed { tick, .. })
            | Line::VoteFailed(VoteFailed { tick, .. }) => Some(*tick),
        }
    }

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
            Line::VoteStarted(v) => ([v.caller, None, None], &[]),
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
            | Line::DetStickies(PlayerMoment { player, .. }) => (one(*player), &[]),
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
        /// The build whose tables are used.
        build: String,
        /// The build the server reported when it is not one that can be used.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        server_build: Option<String>,
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
        /// What it is worth, when the mode counts that: the points of a robot destruction core,
        /// the bottles a player destruction pickup holds.
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

/// A kill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(clippy::struct_excessive_bools, reason = "the kill feed's flags, each its own field")]
pub struct Kill {
    /// Server tick.
    pub tick: u32,
    /// Who died.
    pub victim: PlayerId,
    /// The killer; none for a suicide or the world.
    pub attacker: Option<PlayerId>,
    /// Who assisted.
    pub assister: Option<PlayerId>,
    /// The kill icon's weapon name.
    pub weapon: String,
    /// A crit.
    pub crit: bool,
    /// A headshot.
    pub headshot: bool,
    /// A backstab.
    pub backstab: bool,
    /// A suicide.
    pub suicide: bool,
    /// The attacker now dominates the victim.
    pub dominated: bool,
    /// Revenge on a dominating player.
    pub revenge: bool,
    /// A spy's Dead Ringer: the victim is not dead.
    pub feign: bool,
    /// The victim was blast jumping: an airshot for projectiles.
    pub airborne: bool,
    /// Where the victim was.
    pub victim_pos: Option<Position>,
    /// Where the attacker was.
    pub attacker_pos: Option<Position>,
}

/// Damage done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Damage {
    /// Server tick.
    pub tick: u32,
    /// Who was hurt.
    pub victim: PlayerId,
    /// Who hurt them.
    pub attacker: Option<PlayerId>,
    /// Damage dealt.
    pub amount: u16,
    /// The victim's health after.
    pub health: u16,
    /// A crit.
    pub crit: bool,
    /// A mini-crit.
    pub minicrit: bool,
    /// TF2's weapon id.
    pub weapon_id: u16,
}

/// A player spawned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Spawn {
    pub tick: u32,
    pub player: PlayerId,
    pub team: Team,
    pub class: Class,
    pub pos: Option<Position>,
}

/// A medic deployed their charge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Uber {
    /// Server tick.
    pub tick: u32,
    /// The medic.
    pub medic: PlayerId,
    /// Who they charged.
    pub target: Option<PlayerId>,
    /// Their medigun.
    pub medigun: Option<MedigunKind>,
}

/// A medic's charge filled up (`uber_ready`) or ran out (`uber_ended`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MedicCharge {
    /// Server tick.
    pub tick: u32,
    /// The medic.
    pub medic: PlayerId,
    /// Their medigun.
    pub medigun: Option<MedigunKind>,
}

/// A player changed team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct TeamChange {
    pub tick: u32,
    pub player: PlayerId,
    pub team: Team,
    pub old_team: Team,
    /// Because they disconnected.
    pub disconnect: bool,
}

/// A player changed class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct ClassChange {
    pub tick: u32,
    pub player: PlayerId,
    pub class: Class,
}

/// A player left the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct PlayerLeave {
    pub tick: u32,
    pub player: PlayerId,
    pub name: String,
    pub reason: String,
}

/// An engineer built (`build`) or upgraded (`building_upgrade`) a building.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct ObjectChange {
    pub tick: u32,
    pub player: PlayerId,
    pub object: BuildingKind,
    /// The building's entity index.
    pub id: u16,
}

/// A building was repaired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct BuildingRepair {
    pub tick: u32,
    /// The building's entity index.
    pub id: u16,
    pub amount: u16,
}

/// A building was destroyed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct BuildingDestroyed {
    pub tick: u32,
    /// Its engineer.
    pub owner: PlayerId,
    pub attacker: Option<PlayerId>,
    pub object: BuildingKind,
    /// The building's entity index.
    pub id: u16,
    pub weapon: String,
    /// Still being built.
    pub was_building: bool,
}

/// Time was added to a round timer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct TimeAdded {
    pub tick: u32,
    pub team: Team,
    pub seconds: u16,
}

/// A round started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct RoundStart {
    pub tick: u32,
    pub full_reset: bool,
}

/// An event with nothing but its tick: `round_active`, `setup_finished`, `stalemate`, `overtime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moment {
    /// Server tick.
    pub tick: u32,
}

/// A round was won.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct RoundWin {
    pub tick: u32,
    pub team: Team,
    /// TF2's win reason.
    pub reason: u8,
    /// Seconds the round lasted.
    pub round_time: f64,
}

/// The scores on the win panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Scores {
    pub tick: u32,
    pub red: u16,
    pub blue: u16,
    pub game_over: bool,
}

/// The match is over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct GameOver {
    pub tick: u32,
    pub reason: String,
}

/// A control point was captured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct PointCaptured {
    pub tick: u32,
    pub point: u8,
    pub name: String,
    pub team: Team,
    /// Who captured it (none for one no longer known).
    pub cappers: Vec<Option<PlayerId>>,
}

/// A capture was blocked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct CaptureBlocked {
    pub tick: u32,
    pub point: u8,
    pub name: String,
    pub blocker: Option<PlayerId>,
    pub victim: Option<PlayerId>,
}

/// A capture in progress was abandoned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct CaptureBroken {
    pub tick: u32,
    pub point: u8,
    pub name: String,
}

/// A player capping a point was killed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct KilledCapper {
    pub tick: u32,
    pub point: u8,
    pub killer: PlayerId,
    pub victim: PlayerId,
    pub assister: Option<PlayerId>,
}

/// Something happened to a flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Flag {
    pub tick: u32,
    pub event: FlagAction,
    pub player: Option<PlayerId>,
    pub carrier: Option<PlayerId>,
    pub team: Team,
}

/// A medic died.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct MedicDeath {
    pub tick: u32,
    pub medic: PlayerId,
    pub attacker: Option<PlayerId>,
    /// Healing done this life.
    pub healing: u16,
    /// Died with a full charge: a dropped über.
    pub drop: bool,
}

/// A player defended their medic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct MedicDefended {
    pub tick: u32,
    pub player: PlayerId,
    pub medic: PlayerId,
}

/// A projectile was reflected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Reflect {
    pub tick: u32,
    pub player: PlayerId,
    /// Who fired it.
    pub owner: Option<PlayerId>,
    pub weapon_id: u16,
}

/// A soldier's banner went up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Banner {
    pub tick: u32,
    pub player: PlayerId,
    pub kind: BannerKind,
}

/// Bullets were fired.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Shot {
    pub tick: u32,
    pub player: Option<PlayerId>,
    pub origin: Position,
    /// Degrees.
    pub pitch: f64,
    /// Degrees.
    pub yaw: f64,
    pub weapon_id: i64,
    pub crit: bool,
}

/// Something exploded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Explosion {
    pub tick: u32,
    pub origin: Position,
    pub weapon_id: i64,
}

/// A sentry fired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct SentryShot {
    pub tick: u32,
    /// The sentry's entity index.
    pub sentry: u32,
    pub builder: Option<PlayerId>,
    pub target: Option<PlayerId>,
    /// Shells fired.
    pub shells: i64,
}

/// A pyro airblasted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Airblast {
    pub tick: u32,
    pub player: PlayerId,
}

/// A chat line: names and text come from players, any Unicode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chat {
    /// Server tick.
    pub tick: u32,
    /// Who said it.
    pub player: Option<PlayerId>,
    /// The sender's name as the message gives it.
    pub name: Option<String>,
    /// The text without colour codes.
    pub text: String,
    /// The coloured spans, for text with colour codes.
    pub parts: Option<Vec<ChatPart>>,
    /// Where it went.
    pub kind: ChatKind,
}

/// A coloured span of a chat line, written as `{"c": colour, "t": text}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPart {
    /// The colour; none for the default.
    #[serde(rename = "c")]
    pub color: Option<ChatColor>,
    /// The text.
    #[serde(rename = "t")]
    pub text: String,
}

/// A chat colour, written as `RRGGBB` (upper case) or `team`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatColor {
    /// The sender's team colour.
    Team,
    /// Red, green and blue.
    Rgb([u8; 3]),
}

impl ChatColor {
    /// The colour of six hex digits, in either case.
    pub fn from_hex(hex: &str) -> Option<ChatColor> {
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(ChatColor::Rgb([byte(0)?, byte(2)?, byte(4)?]))
    }
}

impl Serialize for ChatColor {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            ChatColor::Team => s.serialize_str("team"),
            ChatColor::Rgb([r, g, b]) => s.collect_str(&format_args!("{r:02X}{g:02X}{b:02X}")),
        }
    }
}

impl<'de> Deserialize<'de> for ChatColor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        match &*text {
            "team" => Some(ChatColor::Team),
            hex if !hex.bytes().any(|b| b.is_ascii_lowercase()) => ChatColor::from_hex(hex),
            _ => None,
        }
        .ok_or_else(|| de::Error::custom(format_args!("unknown chat colour {text:?}")))
    }
}

/// A player was healed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Heal {
    pub tick: u32,
    pub patient: PlayerId,
    pub healer: Option<PlayerId>,
    pub amount: i64,
    pub source: HealSource,
}

/// A blast jump started or landed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct BlastJump {
    pub tick: u32,
    pub player: PlayerId,
    pub kind: JumpKind,
    pub phase: JumpPhase,
}

/// A health or ammo pickup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Pickup {
    pub tick: u32,
    pub player: PlayerId,
    pub item: PickupItem,
    pub size: Option<PickupSize>,
}

/// Where a bullet hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Impact {
    pub tick: u32,
    pub kind: ImpactKind,
    pub pos: Position,
}

/// A decal (a bullet hole or a scorch mark).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Mark {
    pub tick: u32,
    pub pos: Position,
}

/// A vote's options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct VoteOptions {
    pub tick: u32,
    pub options: Vec<String>,
}

/// A vote was cast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct VoteCast {
    pub tick: u32,
    pub player: Option<PlayerId>,
    /// Index into the options.
    pub option: u8,
    pub team: Team,
}

/// A tournament ready-up change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Tournament {
    pub tick: u32,
    pub player: Option<PlayerId>,
    /// None for a team name change.
    pub ready: Option<bool>,
}

/// An announcer line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Announcer {
    pub tick: u32,
    pub sound: String,
    pub team: Team,
}

/// The map time left was announced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct TimeRemaining {
    pub tick: u32,
    pub seconds: u32,
}

/// A voice command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct VoiceCommand {
    pub tick: u32,
    pub player: PlayerId,
    pub menu: u8,
    pub item: u8,
}

/// Text the server shows in the middle of the screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct ServerText {
    pub tick: u32,
    pub text: String,
}

/// Pass time: a player got the ball.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct PassGet {
    pub tick: u32,
    pub player: PlayerId,
}

/// Pass time: the ball came loose from its carrier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct PassFree {
    pub tick: u32,
    /// Who had it.
    pub player: PlayerId,
    /// Who knocked it loose, if anyone.
    pub attacker: Option<PlayerId>,
}

/// Pass time: a pass was caught.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct PassCaught {
    pub tick: u32,
    pub passer: PlayerId,
    pub catcher: PlayerId,
    /// Hammer units the ball flew.
    pub dist: f64,
    /// Seconds in the air.
    pub duration: f64,
}

/// Pass time: a goal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct PassScore {
    pub tick: u32,
    pub scorer: PlayerId,
    /// Who passed it to the scorer.
    pub assister: Option<PlayerId>,
    /// Points the goal is worth.
    pub points: u8,
}

/// Pass time: the ball was taken off its carrier by melee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct PassStolen {
    pub tick: u32,
    pub victim: PlayerId,
    pub attacker: PlayerId,
}

/// Pass time: a pass was blocked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct PassBlocked {
    pub tick: u32,
    /// Who threw it.
    pub owner: PlayerId,
    pub blocker: PlayerId,
}

/// Pass time: the ball carrier was killed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct BallCarrierKilled {
    pub tick: u32,
    pub attacker: Option<PlayerId>,
    pub victim: PlayerId,
    pub assister: Option<PlayerId>,
}

/// Robot destruction: a robot was destroyed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct RobotKilled {
    pub tick: u32,
    pub attacker: Option<PlayerId>,
    /// The kill icon's weapon name.
    pub weapon: String,
}

/// How robot destruction points were scored (TF2's `RDScoreMethod_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreMethod {
    /// Cores brought to the team's reactor.
    ReactorCaptured,
    /// A core picked up.
    CoresCollected,
    /// The reactor's cores returned.
    ReactorReturned,
    /// Cores stolen from the enemy reactor (negative for the team stolen from).
    ReactorStolen,
    /// A method this version does not know.
    Unknown,
}

/// Robot destruction: a player scored points for their team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct PointsScored {
    pub tick: u32,
    pub player: PlayerId,
    pub method: ScoreMethod,
    pub amount: i16,
}

/// Something one player did: delivered player destruction pickups (`special_score`), detonated
/// their stickies (`det_stickies`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct PlayerMoment {
    pub tick: u32,
    pub player: PlayerId,
}

/// Player destruction: a team leader was killed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct TeamLeaderKilled {
    pub tick: u32,
    pub killer: Option<PlayerId>,
    pub victim: PlayerId,
}

/// Special delivery: the rocket opened for the team with the Australium.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct RocketOpen {
    pub tick: u32,
    pub team: Team,
}

/// Territorial control: the round chose its control points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct RoundSelected {
    pub tick: u32,
    /// The map's name for the round (`round_a`).
    pub round: String,
}

/// A player went through a teleporter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Teleported {
    pub tick: u32,
    pub player: PlayerId,
    /// The engineer whose teleporter it was.
    pub builder: Option<PlayerId>,
    /// Hammer units between entrance and exit.
    pub dist: f64,
}

/// A player was stunned (the Sandman's ball, …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Stun {
    pub tick: u32,
    pub stunner: Option<PlayerId>,
    pub victim: PlayerId,
    /// The victim was capping.
    pub capping: bool,
    /// A full stun (a moonshot), not a slow-down.
    pub big: bool,
}

/// A spy sapped a building.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Sapped {
    pub tick: u32,
    /// The spy.
    pub player: PlayerId,
    /// The engineer.
    pub owner: PlayerId,
    pub object: BuildingKind,
    /// The sapper's entity index.
    pub sapper: u16,
}

/// A capture started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct CaptureStarted {
    pub tick: u32,
    pub point: u8,
    pub name: String,
    /// The team capturing.
    pub team: Team,
    /// The team that owns the point.
    pub owner: Team,
    /// Who is capping (none for one no longer known).
    pub cappers: Vec<Option<PlayerId>>,
}

/// A control point was locked or unlocked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct PointLocked {
    pub tick: u32,
    pub point: u8,
    pub name: String,
    pub locked: bool,
}

/// Damage to something that is not a player: a tank, a robot destruction robot, a boss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct NpcDamage {
    pub tick: u32,
    /// Its entity index.
    pub id: u16,
    pub attacker: Option<PlayerId>,
    pub amount: u16,
    /// Health left.
    pub health: u16,
    pub crit: bool,
    pub weapon_id: u16,
    /// TF2's boss type, 0 for none.
    pub boss: u16,
}

/// A map's or plugin's text on the HUD (`game_text`): scores, timers, instructions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct HudText {
    pub tick: u32,
    /// The HUD channel; a new text replaces the one on its channel.
    pub channel: u8,
    /// Where on the screen, 0 to 1 from the top left (-1 centres).
    pub pos: [f64; 2],
    pub color: ChatColor,
    /// Seconds it stays.
    pub hold: f64,
    pub text: String,
}

/// A hint shown on the HUD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct Hint {
    pub tick: u32,
    pub text: String,
}

/// A vote was called.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct VoteStarted {
    pub tick: u32,
    /// The team voting; none for everyone.
    pub team: Team,
    pub caller: Option<PlayerId>,
    /// What the vote is about, as a localisation token (`#TF_vote_nextlevel`).
    pub issue: String,
    /// Its argument (a map, a player's name).
    pub details: String,
    /// A yes/no vote, not a choice between options.
    pub yes_no: bool,
}

/// A vote passed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct VotePassed {
    pub tick: u32,
    pub team: Team,
    pub issue: String,
    pub details: String,
}

/// A vote failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct VoteFailed {
    pub tick: u32,
    pub team: Team,
    /// TF2's reason (`vote_create_failed_t`).
    pub reason: u8,
}

/// Names of the TF2 player conditions (`ETFCond`), by number; "" for unused numbers. They follow
/// tf-demo-parser's `PlayerCondition`, spelled out so the output does not follow that crate's
/// Debug formatting.
#[rustfmt::skip]
const CONDITION_NAMES: [&str; 131] = [
    "Aiming",
    "Zoomed",
    "Disguising",
    "Disguised",
    "Stealthed",
    "Invulnerable",
    "Teleported",
    "Taunting",
    "InvulnerableWearingOff",
    "StealthedBlink",
    "SelectedToTeleport",
    "CritBoosted",
    "TmpDamageBonus",
    "FeignDeath",
    "Phase",
    "Stunned",
    "OffensiveBuff",
    "ShieldCharge",
    "DemoBuff",
    "EnergyBuff",
    "RadiusHeal",
    "HealthBuff",
    "Burning",
    "HealthOverHealed",
    "Urine",
    "Bleeding",
    "DefensiveBuff",
    "MadMilk",
    "MegaHeal",
    "RegenOnDamageBuff",
    "MarkedForDeath",
    "NoHealingDamageBuff",
    "SpeedBoost",
    "CritBoostedPumpkin",
    "CritBoostedUserBuff",
    "CritBoostedDemoCharge",
    "SodaPopperHype",
    "CritBoostedFirstBlood",
    "CritBoostedBonusTime",
    "CritBoostedCtfCapture",
    "CritBoostedOnKill",
    "CannotSwitchFromMelee",
    "DefenseBuffNoCritBlock",
    "Reprogrammed",
    "CritBoostedRageBuff",
    "DefenseBuffHigh",
    "SniperChargeRageBuff",
    "DisguiseWearingOff",
    "MarkedForDeathSilent",
    "DisguisedAsDispenser",
    "Sapped",
    "InvulnerableHideUnlessDamaged",
    "InvulnerableUserBuff",
    "HalloweenBombHead",
    "HalloweenThriller",
    "RadiusHealOnDamage",
    "CritBoostedCardEffect",
    "InvulnerableCardEffect",
    "MedigunUberBulletResist",
    "MedigunUberBlastResist",
    "MedigunUberFireResist",
    "MedigunSmallBulletResist",
    "MedigunSmallBlastResist",
    "MedigunSmallFireResist",
    "StealthedUserBuff",
    "MedigunDebuff",
    "StealthedUserBuffFading",
    "BulletImmune",
    "BlastImmune",
    "FireImmune",
    "PreventDeath",
    "MvmBotStunRadiowave",
    "HalloweenSpeedBoost",
    "HalloweenQuickHeal",
    "HalloweenGiant",
    "HalloweenTiny",
    "HalloweenInHell",
    "HalloweenGhostMode",
    "MiniCritBoostedOnKill",
    "ObscuredSmoke",
    "ParachuteActive",
    "BlastJumping",
    "HalloweenKart",
    "HalloweenKartDash",
    "BalloonHead",
    "MeleeOnly",
    "SwimmingCurse",
    "FreezeInput",
    "HalloweenKartCage",
    "", // 89: unused
    "RuneStrength",
    "RuneHaste",
    "RuneRegen",
    "RuneResist",
    "RuneVampire",
    "RuneReflect",
    "RunePrecision",
    "RuneAgility",
    "GrapplingHook",
    "GrapplingHookSafeFall",
    "GrapplingHookLatched",
    "GrapplingHookBleeding",
    "AfterburnImmune",
    "RuneKnockout",
    "RuneImbalance",
    "CritBoostedRuneTemp",
    "PasstimeInterception",
    "SwimmingNoEffects",
    "PURGATORY",
    "RuneKing",
    "RunePlague",
    "RuneSupernova",
    "PLAGUE",
    "KingBuffed",
    "TeamGlows",
    "KnockedIntoAir",
    "CompetitiveWinner",
    "CompetitiveLoser",
    "HealingDebuff",
    "PasstimePenaltyDebuff",
    "GrappledToPlayer",
    "GrappledByPlayer",
    "ParachuteDeployed",
    "GAS",
    "BurningPyro",
    "RocketPack",
    "LostFooting",
    "AirCurrent",
    "HalloweenHellHeal",
    "PowerUpModeDominant",
    "ImmuneToPushback",
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn value(line: &Line) -> Value {
        serde_json::to_value(line).unwrap()
    }

    fn back(v: &Value) -> Line {
        serde_json::from_value(v.clone()).unwrap()
    }

    #[test]
    fn a_kill_is_written_as_documented() {
        let kill = Line::Kill(Kill {
            tick: 7,
            victim: PlayerId(3),
            attacker: Some(PlayerId(4)),
            assister: None,
            weapon: "tf_projectile_rocket".into(),
            crit: true,
            headshot: false,
            backstab: false,
            suicide: false,
            dominated: false,
            revenge: false,
            feign: false,
            airborne: true,
            victim_pos: Some([1.5, 2.0, -3.0]),
            attacker_pos: None,
        });
        let expected = json!({
            "type": "kill", "tick": 7, "victim": 3, "attacker": 4, "assister": null,
            "weapon": "tf_projectile_rocket", "crit": true, "headshot": false, "backstab": false,
            "suicide": false, "dominated": false, "revenge": false, "feign": false, "airborne": true,
            "victim_pos": [1.5, 2.0, -3.0], "attacker_pos": null,
        });
        assert_eq!(value(&kill), expected);
        assert_eq!(back(&expected), kill);
    }

    #[test]
    fn statuses_are_tagged_by_state() {
        let connecting = Line::Status(Status::Connecting {
            server: "1.2.3.4:27020".into(),
            build: "11138802".into(),
            server_build: None,
        });
        let expected = json!({"type": "status", "state": "connecting", "server": "1.2.3.4:27020", "build": "11138802"});
        assert_eq!(value(&connecting), expected);
        assert_eq!(back(&expected), connecting);
        assert_eq!(value(&Line::Status(Status::MapChange)), json!({"type": "status", "state": "map_change"}));
        assert_eq!(value(&Line::Status(Status::Prespawn)), json!({"type": "status", "state": "prespawn"}));
        assert_eq!(Line::Status(Status::New).tick(), None);
    }

    #[expect(clippy::option_option, reason = "the field's own type")]
    fn sentry(target: Option<Option<PlayerId>>) -> Building {
        Building {
            id: 492,
            kind: BuildingKind::Sentry,
            team: Team::Blue,
            level: 1,
            hp: 150,
            max_hp: 150,
            pos: [0.0, 0.0, 0.0],
            yaw: 90.0,
            builder: Some(PlayerId(5)),
            sapped: false,
            entrance: None,
            mini: Some(false),
            building: Some(false),
            aim: None,
            target,
            attacking: None,
        }
    }

    #[test]
    fn a_sentry_without_a_target_differs_from_one_not_seen_aiming() {
        let idle = serde_json::to_value(sentry(Some(None))).unwrap();
        assert_eq!(idle["target"], Value::Null);
        assert!(idle.as_object().unwrap().contains_key("target"));
        assert_eq!(serde_json::from_value::<Building>(idle).unwrap().target, Some(None));

        let unseen = serde_json::to_value(sentry(None)).unwrap();
        assert!(!unseen.as_object().unwrap().contains_key("target"));
        assert_eq!(serde_json::from_value::<Building>(unseen).unwrap().target, None);

        let aiming = serde_json::to_value(sentry(Some(Some(PlayerId(9))))).unwrap();
        assert_eq!(serde_json::from_value::<Building>(aiming).unwrap().target, Some(Some(PlayerId(9))));
    }

    #[test]
    fn conditions_go_by_name() {
        assert_eq!(Condition::from_number(5).map(Condition::name), Some("Invulnerable"));
        assert_eq!(Condition::from_number(89), None, "unused number");
        assert_eq!(Condition::from_number(200), None);
        let c = Condition::from_number(130).unwrap();
        assert_eq!(c.number(), 130);
        assert_eq!(c.to_string(), "ImmuneToPushback");
        assert_eq!(serde_json::to_value(c).unwrap(), json!("ImmuneToPushback"));
        assert_eq!(serde_json::from_value::<Condition>(json!("ImmuneToPushback")).unwrap(), c);
        assert!(serde_json::from_value::<Condition>(json!("Hovering")).is_err());
    }

    #[test]
    fn stamped_lines_carry_the_receive_time_when_live() {
        let airblast = Line::Airblast(Airblast { tick: 9, player: PlayerId(2) });
        let live = Stamped { line: airblast.clone(), time: Some(1_700_000_000_000) };
        let expected = json!({"type": "airblast", "tick": 9, "player": 2, "time": 1_700_000_000_000u64});
        assert_eq!(serde_json::to_value(&live).unwrap(), expected);
        assert_eq!(serde_json::from_value::<Stamped>(expected).unwrap(), live);

        let offline = Stamped { line: airblast, time: None };
        let expected = json!({"type": "airblast", "tick": 9, "player": 2});
        assert_eq!(serde_json::to_value(&offline).unwrap(), expected);
        assert_eq!(serde_json::from_value::<Stamped>(expected).unwrap(), offline);
    }

    #[test]
    fn lines_name_their_players_and_tick() {
        let cap = Line::PointCaptured(PointCaptured {
            tick: 40,
            point: 1,
            name: "#cp_mid".into(),
            team: Team::Red,
            cappers: vec![Some(PlayerId(3)), None, Some(PlayerId(8))],
        });
        assert_eq!(cap.players().collect::<Vec<_>>(), [PlayerId(3), PlayerId(8)]);
        assert_eq!(cap.tick(), Some(40));
        let medic =
            Line::MedicDeath(MedicDeath { tick: 1, medic: PlayerId(2), attacker: None, healing: 300, drop: true });
        assert_eq!(medic.players().collect::<Vec<_>>(), [PlayerId(2)]);
        let killed = Line::KilledCapper(KilledCapper {
            tick: 1,
            point: 0,
            killer: PlayerId(4),
            victim: PlayerId(5),
            assister: Some(PlayerId(6)),
        });
        assert_eq!(killed.players().collect::<Vec<_>>(), [PlayerId(4), PlayerId(5), PlayerId(6)]);
        assert_eq!(
            Line::Airblast(Airblast { tick: 1, player: PlayerId(7) }).players().collect::<Vec<_>>(),
            [PlayerId(7)]
        );
        assert_eq!(Line::Mark(Mark { tick: 1, pos: [0.0; 3] }).players().count(), 0);
    }

    #[test]
    fn chat_colours_are_written_as_hex_or_team() {
        let parts = [
            ChatPart { color: None, text: "[STV] ".into() },
            ChatPart { color: Some(ChatColor::Rgb([0x99, 0xCC, 0x0F])), text: "BLU".into() },
            ChatPart { color: Some(ChatColor::Team), text: "won".into() },
        ];
        let expected = json!([{"c": null, "t": "[STV] "}, {"c": "99CC0F", "t": "BLU"}, {"c": "team", "t": "won"}]);
        assert_eq!(s(&parts), expected);
        assert_eq!(serde_json::from_value::<Vec<ChatPart>>(expected).unwrap(), parts);
        for bad in ["99cc0f", "99CC0", "GGGGGG", "red"] {
            assert!(serde_json::from_value::<ChatPart>(json!({"c": bad, "t": ""})).is_err(), "{bad}");
        }
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "one case per line type")]
    fn game_mode_lines_are_written_as_documented() {
        let p = PlayerId;
        let cases = [
            (Line::PassGet(PassGet { tick: 1, player: p(3) }), json!({"type": "pass_get", "tick": 1, "player": 3})),
            (
                Line::PassFree(PassFree { tick: 1, player: p(3), attacker: None }),
                json!({"type": "pass_free", "tick": 1, "player": 3, "attacker": null}),
            ),
            (
                Line::PassCaught(PassCaught { tick: 1, passer: p(3), catcher: p(4), dist: 54.3, duration: 1.48 }),
                json!({"type": "pass_caught", "tick": 1, "passer": 3, "catcher": 4, "dist": 54.3, "duration": 1.48}),
            ),
            (
                Line::PassScore(PassScore { tick: 1, scorer: p(3), assister: Some(p(4)), points: 1 }),
                json!({"type": "pass_score", "tick": 1, "scorer": 3, "assister": 4, "points": 1}),
            ),
            (
                Line::PassStolen(PassStolen { tick: 1, victim: p(3), attacker: p(4) }),
                json!({"type": "pass_stolen", "tick": 1, "victim": 3, "attacker": 4}),
            ),
            (
                Line::PassBlocked(PassBlocked { tick: 1, owner: p(3), blocker: p(4) }),
                json!({"type": "pass_blocked", "tick": 1, "owner": 3, "blocker": 4}),
            ),
            (
                Line::BallCarrierKilled(BallCarrierKilled {
                    tick: 1,
                    attacker: Some(p(3)),
                    victim: p(4),
                    assister: None,
                }),
                json!({"type": "ball_carrier_killed", "tick": 1, "attacker": 3, "victim": 4, "assister": null}),
            ),
            (
                Line::RobotKilled(RobotKilled { tick: 1, attacker: Some(p(3)), weapon: "solemn_vow".into() }),
                json!({"type": "robot_killed", "tick": 1, "attacker": 3, "weapon": "solemn_vow"}),
            ),
            (
                Line::PointsScored(PointsScored {
                    tick: 1,
                    player: p(3),
                    method: ScoreMethod::ReactorStolen,
                    amount: -5,
                }),
                json!({"type": "points_scored", "tick": 1, "player": 3, "method": "reactor_stolen", "amount": -5}),
            ),
            (
                Line::SpecialScore(PlayerMoment { tick: 1, player: p(3) }),
                json!({"type": "special_score", "tick": 1, "player": 3}),
            ),
            (
                Line::TeamLeaderKilled(TeamLeaderKilled { tick: 1, killer: Some(p(3)), victim: p(4) }),
                json!({"type": "team_leader_killed", "tick": 1, "killer": 3, "victim": 4}),
            ),
            (
                Line::RocketOpen(RocketOpen { tick: 1, team: Team::Red }),
                json!({"type": "rocket_open", "tick": 1, "team": "red"}),
            ),
            (
                Line::RoundSelected(RoundSelected { tick: 1, round: "round_a".into() }),
                json!({"type": "round_selected", "tick": 1, "round": "round_a"}),
            ),
            (
                Line::Teleported(Teleported { tick: 1, player: p(3), builder: Some(p(4)), dist: 2707.7 }),
                json!({"type": "teleported", "tick": 1, "player": 3, "builder": 4, "dist": 2707.7}),
            ),
            (
                Line::Stun(Stun { tick: 1, stunner: Some(p(3)), victim: p(4), capping: false, big: true }),
                json!({"type": "stun", "tick": 1, "stunner": 3, "victim": 4, "capping": false, "big": true}),
            ),
            (
                Line::Sapped(Sapped { tick: 1, player: p(3), owner: p(4), object: BuildingKind::Sentry, sapper: 1096 }),
                json!({"type": "sapped", "tick": 1, "player": 3, "owner": 4, "object": "sentry", "sapper": 1096}),
            ),
            (
                Line::BuildingRemoved(ObjectChange {
                    tick: 1,
                    player: p(3),
                    object: BuildingKind::Teleporter,
                    id: 165,
                }),
                json!({"type": "building_removed", "tick": 1, "player": 3, "object": "teleporter", "id": 165}),
            ),
            (
                Line::BuildingDetonated(ObjectChange {
                    tick: 1,
                    player: p(3),
                    object: BuildingKind::Dispenser,
                    id: 476,
                }),
                json!({"type": "building_detonated", "tick": 1, "player": 3, "object": "dispenser", "id": 476}),
            ),
            (
                Line::DetStickies(PlayerMoment { tick: 1, player: p(3) }),
                json!({"type": "det_stickies", "tick": 1, "player": 3}),
            ),
            (
                Line::CaptureStarted(CaptureStarted {
                    tick: 1,
                    point: 0,
                    name: "#Arena_cap".into(),
                    team: Team::Red,
                    owner: Team::Unassigned,
                    cappers: vec![Some(p(3)), None],
                }),
                json!({"type": "capture_started", "tick": 1, "point": 0, "name": "#Arena_cap", "team": "red",
                       "owner": "none", "cappers": [3, null]}),
            ),
            (
                Line::PointLocked(PointLocked { tick: 1, point: 0, name: "#cap".into(), locked: true }),
                json!({"type": "point_locked", "tick": 1, "point": 0, "name": "#cap", "locked": true}),
            ),
            (
                Line::NpcDamage(NpcDamage {
                    tick: 1,
                    id: 154,
                    attacker: Some(p(3)),
                    amount: 22,
                    health: 77,
                    crit: false,
                    weapon_id: 58,
                    boss: 0,
                }),
                json!({"type": "npc_damage", "tick": 1, "id": 154, "attacker": 3, "amount": 22, "health": 77,
                       "crit": false, "weapon_id": 58, "boss": 0}),
            ),
            (
                Line::HudText(HudText {
                    tick: 1,
                    channel: 2,
                    pos: [0.65, 0.82],
                    color: ChatColor::Rgb([0xFF, 0x55, 0x42]),
                    hold: 1.0,
                    text: "0%".into(),
                }),
                json!({"type": "hud_text", "tick": 1, "channel": 2, "pos": [0.65, 0.82], "color": "FF5542",
                       "hold": 1.0, "text": "0%"}),
            ),
            (
                Line::Hint(Hint { tick: 1, text: "Votes: 0/8".into() }),
                json!({"type": "hint", "tick": 1, "text": "Votes: 0/8"}),
            ),
            (
                Line::VoteStarted(VoteStarted {
                    tick: 1,
                    team: Team::Unassigned,
                    caller: Some(p(3)),
                    issue: "#TF_vote_nextlevel".into(),
                    details: "pl_goldrush".into(),
                    yes_no: true,
                }),
                json!({"type": "vote_started", "tick": 1, "team": "none", "caller": 3,
                       "issue": "#TF_vote_nextlevel", "details": "pl_goldrush", "yes_no": true}),
            ),
            (
                Line::VotePassed(VotePassed {
                    tick: 1,
                    team: Team::Unassigned,
                    issue: "#TF_vote_passed_nextlevel".into(),
                    details: "pl_goldrush".into(),
                }),
                json!({"type": "vote_passed", "tick": 1, "team": "none", "issue": "#TF_vote_passed_nextlevel",
                       "details": "pl_goldrush"}),
            ),
            (
                Line::VoteFailed(VoteFailed { tick: 1, team: Team::Red, reason: 3 }),
                json!({"type": "vote_failed", "tick": 1, "team": "red", "reason": 3}),
            ),
            (
                Line::BlastJump(BlastJump {
                    tick: 1,
                    player: p(3),
                    kind: JumpKind::RocketPack,
                    phase: JumpPhase::Start,
                }),
                json!({"type": "blast_jump", "tick": 1, "player": 3, "kind": "rocket_pack", "phase": "start"}),
            ),
        ];
        for (line, expected) in cases {
            assert_eq!(value(&line), expected);
            assert_eq!(back(&expected), line);
            assert_eq!(line.tick(), Some(1));
        }
    }

    #[test]
    fn game_mode_objectives_and_scores_are_written_as_documented() {
        let objectives = [
            Objective::Ball { id: 9, pos: [1.0, 2.0, 3.0], team: Team::Blue, carrier: Some(PlayerId(4)) },
            Objective::Goal { id: 10, pos: [0.0, -1758.1, -416.5], team: Team::Blue },
            Objective::Robot { id: 11, pos: [0.0; 3], team: Team::Red, hp: 70, max_hp: 330 },
            Objective::Tank { id: 12, pos: [0.0; 3], health: 0.99 },
        ];
        let expected = json!([
            {"kind": "ball", "id": 9, "pos": [1.0, 2.0, 3.0], "team": "blue", "carrier": 4},
            {"kind": "goal", "id": 10, "pos": [0.0, -1758.1, -416.5], "team": "blue"},
            {"kind": "robot", "id": 11, "pos": [0.0, 0.0, 0.0], "team": "red", "hp": 70, "max_hp": 330},
            {"kind": "tank", "id": 12, "pos": [0.0, 0.0, 0.0], "health": 0.99},
        ]);
        assert_eq!(s(&objectives), expected);
        assert_eq!(serde_json::from_value::<Vec<Objective>>(expected).unwrap(), objectives);
        let core = |points| Objective::Flag {
            id: 1,
            pos: [0.0; 3],
            team: Team::Red,
            status: FlagStatus::Home,
            carrier: None,
            returns_in: None,
            points,
        };
        assert!(s(core(None)).get("points").is_none(), "a CTF flag is written as before");
        assert_eq!(s(core(Some(5)))["points"], 5);
        let points = Points { red: 3, blue: 5, max: Some(45), red_leader: Some(PlayerId(4)), blue_leader: None };
        let expected = json!({"red": 3, "blue": 5, "max": 45, "red_leader": 4, "blue_leader": null});
        assert_eq!(s(&points), expected);
        let mvm = Mvm { wave: 2, waves: 5, enemies: 67, between_waves: false, money: 41 };
        let expected = json!({"wave": 2, "waves": 5, "enemies": 67, "between_waves": false, "money": 41});
        assert_eq!(s(&mvm), expected);
        assert_eq!(serde_json::from_value::<Mvm>(expected).unwrap(), mvm);
    }

    #[test]
    fn game_mode_lines_name_their_players() {
        let line = Line::BallCarrierKilled(BallCarrierKilled {
            tick: 1,
            attacker: Some(PlayerId(3)),
            victim: PlayerId(4),
            assister: Some(PlayerId(5)),
        });
        assert_eq!(line.players().collect::<Vec<_>>(), [PlayerId(3), PlayerId(4), PlayerId(5)]);
        let stun = Line::Stun(Stun { tick: 1, stunner: None, victim: PlayerId(4), capping: true, big: false });
        assert_eq!(stun.players().collect::<Vec<_>>(), [PlayerId(4)]);
        let started = Line::CaptureStarted(CaptureStarted {
            tick: 1,
            point: 1,
            name: String::new(),
            team: Team::Blue,
            owner: Team::Red,
            cappers: vec![Some(PlayerId(7)), Some(PlayerId(8))],
        });
        assert_eq!(started.players().collect::<Vec<_>>(), [PlayerId(7), PlayerId(8)]);
    }

    fn s(v: impl Serialize) -> Value {
        serde_json::to_value(v).unwrap()
    }

    #[test]
    fn teams_and_kinds_use_the_documented_spellings() {
        assert_eq!(s(Team::Unassigned), json!("none"));
        assert_eq!(s(Class::Demoman), json!("demoman"));
        assert_eq!(s(ProjectileKind::ShortCircuitOrb), json!("ShortCircuitOrb"));
        assert_eq!(s(Ubered::VaccinatorBlast), json!("vaccinator_blast"));
        assert_eq!(s(Round::BetweenRounds), json!("between_rounds"));
        assert_eq!(s(BannerKind::BattalionsBackup), json!("battalions_backup"));
        assert_eq!(s(HealSource::OnHit), json!("on_hit"));
        assert_eq!(s(ChatKind::SayText), json!("SayText"));
    }
}
