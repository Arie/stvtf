//! A line of output: one event or snapshot, and when it came.

use serde::{Deserialize, Serialize};

use super::events::{
    Airblast, Announcer, BallCarrierKilled, Banner, BlastJump, BuildingDestroyed, BuildingRepair, CaptureBlocked,
    CaptureBroken, CaptureStarted, Chat, ClassChange, Damage, Explosion, Flag, GameOver, Heal, Hint, HudText, Impact,
    Kill, KilledCapper, Mark, MedicCharge, MedicDeath, MedicDefended, Moment, NpcDamage, ObjectChange, PassBlocked,
    PassCaught, PassFree, PassGet, PassScore, PassStolen, Pause, Pickup, PlayerLeave, PlayerMoment, PointCaptured,
    PointLocked, PointsScored, Reflect, RobotKilled, RocketOpen, RoundSelected, RoundStart, RoundWin, Sapped, Scores,
    SentryShot, ServerText, Shot, Spawn, Stun, TeamChange, TeamLeaderKilled, Teleported, TimeAdded, TimeRemaining,
    Tournament, Uber, Upgrade, Voice, VoiceCommand, VoteCast, VoteFailed, VoteOptions, VotePassed, VoteStarted,
};
use super::kinds::PlayerId;
use super::snapshot::Snapshot;
use super::{Server, Status, World};

/// The `Line` enum and its `tick()`: every line but a status carries the tick it happened at, so
/// listing a variant once gives both.
macro_rules! lines {
    ($($variant:ident($payload:ty),)*) => {
        /// One line of output.
        #[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
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
    Voice(Voice),
    Pause(Pause),
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
            | Line::VoteFailed(_)
            | Line::Pause(_) => ([None; 3], &[]),
            Line::Voice(v) => ([v.player, None, None], &[]),
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
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Stamped {
    /// The line itself.
    #[serde(flatten)]
    pub line: Line,
    /// Receive time in Unix milliseconds; none offline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<u64>,
}
