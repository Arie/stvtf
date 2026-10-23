//! The events: one line each.

#![expect(missing_docs, reason = "the fields are named after what they hold; the README describes each line")]

use super::{
    BannerKind, BuildingKind, ChatKind, Class, FlagAction, HealSource, ImpactKind, JumpKind, JumpPhase, MedigunKind,
    PickupItem, PickupSize, PlayerId, Position, Team,
};
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};

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
pub struct ClassChange {
    pub tick: u32,
    pub player: PlayerId,
    pub class: Class,
}

/// A player left the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerLeave {
    pub tick: u32,
    pub player: PlayerId,
    pub name: String,
    pub reason: String,
}

/// An engineer built (`build`) or upgraded (`building_upgrade`) a building.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectChange {
    pub tick: u32,
    pub player: PlayerId,
    pub object: BuildingKind,
    /// The building's entity index.
    pub id: u16,
}

/// A building was repaired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildingRepair {
    pub tick: u32,
    /// The building's entity index.
    pub id: u16,
    pub amount: u16,
}

/// A building was destroyed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct TimeAdded {
    pub tick: u32,
    pub team: Team,
    pub seconds: u16,
}

/// A round started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct Scores {
    pub tick: u32,
    pub red: u16,
    pub blue: u16,
    pub game_over: bool,
}

/// The match is over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameOver {
    pub tick: u32,
    pub reason: String,
}

/// A control point was captured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct CaptureBlocked {
    pub tick: u32,
    pub point: u8,
    pub name: String,
    pub blocker: Option<PlayerId>,
    pub victim: Option<PlayerId>,
}

/// A capture in progress was abandoned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureBroken {
    pub tick: u32,
    pub point: u8,
    pub name: String,
}

/// A player capping a point was killed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KilledCapper {
    pub tick: u32,
    pub point: u8,
    pub killer: PlayerId,
    pub victim: PlayerId,
    pub assister: Option<PlayerId>,
}

/// Something happened to a flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flag {
    pub tick: u32,
    pub event: FlagAction,
    pub player: Option<PlayerId>,
    pub carrier: Option<PlayerId>,
    pub team: Team,
}

/// A medic died.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct MedicDefended {
    pub tick: u32,
    pub player: PlayerId,
    pub medic: PlayerId,
}

/// A projectile was reflected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reflect {
    pub tick: u32,
    pub player: PlayerId,
    /// Who fired it.
    pub owner: Option<PlayerId>,
    pub weapon_id: u16,
}

/// A soldier's banner went up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Banner {
    pub tick: u32,
    pub player: PlayerId,
    pub kind: BannerKind,
}

/// Bullets were fired.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
pub struct Explosion {
    pub tick: u32,
    pub origin: Position,
    pub weapon_id: i64,
}

/// A sentry fired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct Heal {
    pub tick: u32,
    pub patient: PlayerId,
    pub healer: Option<PlayerId>,
    pub amount: i64,
    pub source: HealSource,
}

/// A blast jump started or landed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlastJump {
    pub tick: u32,
    pub player: PlayerId,
    pub kind: JumpKind,
    pub phase: JumpPhase,
}

/// A health or ammo pickup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pickup {
    pub tick: u32,
    pub player: PlayerId,
    pub item: PickupItem,
    pub size: Option<PickupSize>,
}

/// Where a bullet hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Impact {
    pub tick: u32,
    pub kind: ImpactKind,
    pub pos: Position,
}

/// A decal (a bullet hole or a scorch mark).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    pub tick: u32,
    pub pos: Position,
}

/// A vote's options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoteOptions {
    pub tick: u32,
    pub options: Vec<String>,
}

/// A vote was cast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoteCast {
    pub tick: u32,
    pub player: Option<PlayerId>,
    /// Index into the options.
    pub option: u8,
    pub team: Team,
}

/// A tournament ready-up change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tournament {
    pub tick: u32,
    pub player: Option<PlayerId>,
    /// None for a team name change.
    pub ready: Option<bool>,
}

/// An announcer line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcer {
    pub tick: u32,
    pub sound: String,
    pub team: Team,
}

/// The map time left was announced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRemaining {
    pub tick: u32,
    pub seconds: u32,
}

/// A voice command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceCommand {
    pub tick: u32,
    pub player: PlayerId,
    pub menu: u8,
    pub item: u8,
}

/// Text the server shows in the middle of the screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerText {
    pub tick: u32,
    pub text: String,
}

/// Pass time: a player got the ball.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassGet {
    pub tick: u32,
    pub player: PlayerId,
}

/// Pass time: the ball came loose from its carrier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassFree {
    pub tick: u32,
    /// Who had it.
    pub player: PlayerId,
    /// Who knocked it loose, if anyone.
    pub attacker: Option<PlayerId>,
}

/// Pass time: a pass was caught.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
pub struct PassStolen {
    pub tick: u32,
    pub victim: PlayerId,
    pub attacker: PlayerId,
}

/// Pass time: a pass was blocked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassBlocked {
    pub tick: u32,
    /// Who threw it.
    pub owner: PlayerId,
    pub blocker: PlayerId,
}

/// Pass time: the ball carrier was killed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BallCarrierKilled {
    pub tick: u32,
    pub attacker: Option<PlayerId>,
    pub victim: PlayerId,
    pub assister: Option<PlayerId>,
}

/// Robot destruction: a robot was destroyed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct PointsScored {
    pub tick: u32,
    pub player: PlayerId,
    pub method: ScoreMethod,
    pub amount: i16,
}

/// Something one player did: delivered player destruction pickups (`special_score`), detonated
/// their stickies (`det_stickies`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerMoment {
    pub tick: u32,
    pub player: PlayerId,
}

/// Player destruction: a team leader was killed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamLeaderKilled {
    pub tick: u32,
    pub killer: Option<PlayerId>,
    pub victim: PlayerId,
}

/// Special delivery: the rocket opened for the team with the Australium.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RocketOpen {
    pub tick: u32,
    pub team: Team,
}

/// Territorial control: the round chose its control points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundSelected {
    pub tick: u32,
    /// The map's name for the round (`round_a`).
    pub round: String,
}

/// A player went through a teleporter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
pub struct PointLocked {
    pub tick: u32,
    pub point: u8,
    pub name: String,
    pub locked: bool,
}

/// Damage to something that is not a player: a building, a tank, a robot destruction robot, a boss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NpcDamage {
    pub tick: u32,
    /// Its entity index.
    pub id: u16,
    /// None for damage from no player (a sapper's drain).
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
pub struct Hint {
    pub tick: u32,
    pub text: String,
}

/// A vote was called.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The player a kick vote is about.
    pub target: Option<PlayerId>,
}

/// A vote passed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VotePassed {
    pub tick: u32,
    pub team: Team,
    pub issue: String,
    pub details: String,
}

/// Mann vs. Machine: a player bought an upgrade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upgrade {
    pub tick: u32,
    pub player: PlayerId,
    /// The item definition index of what was upgraded (30015 for the Power Up Canteen).
    pub item: u16,
    /// The attribute definition index bought.
    pub attribute: u16,
    /// Money spent.
    pub cost: u16,
}

/// A vote failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoteFailed {
    pub tick: u32,
    pub team: Team,
    /// TF2's reason (`vote_create_failed_t`).
    pub reason: u8,
}
