//! Turns parser state into the JSON the overview consumes: periodic snapshots
//! plus discrete events (kills, damage, shots, explosions, ubers, rounds, chat...).

use super::detail;
use serde::Serialize;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::LazyLock;
use tf_demo_parser::ParserState;
use tf_demo_parser::demo::data::DemoTick;
use tf_demo_parser::demo::data::game_state::{
    Building, GameState, PlayerCondition, PlayerState, Projectile, ProjectileType,
};
use tf_demo_parser::demo::gameevent_gen::GameEvent;
use tf_demo_parser::demo::header::Header;
use tf_demo_parser::demo::message::usermessage::{ChatMessageKind, UserMessage};

use tf_demo_parser::demo::message::packetentities::{EntityId, UpdateType};
use tf_demo_parser::demo::message::{
    Message, MessageType, PacketEntitiesMessage, PacketEntity, ServerInfoMessage, SetConVarMessage, TempEntitiesMessage,
};
use tf_demo_parser::demo::packet::datatable::{ParseSendTable, ServerClass};
use tf_demo_parser::demo::packet::message::MessagePacketMeta;
use tf_demo_parser::demo::packet::stringtable::StringTableEntry;
use tf_demo_parser::demo::parser::MessageHandler;
use tf_demo_parser::demo::parser::analyser::{Class, Team, UserId};
use tf_demo_parser::demo::parser::gamestateanalyser::GameStateAnalyser;
use tf_demo_parser::demo::parser::handler::BorrowMessageHandler;
use tf_demo_parser::demo::sendprop::{SendProp, SendPropIdentifier, SendPropValue};
use tf_demo_parser::demo::vector::Vector;

/// TF2 sets DMG_ACID on critical hits.
const DMG_CRIT: u32 = 1 << 20;
/// player_death death_flags.
const DEATH_DOMINATION: u16 = 0x1;
const DEATH_REVENGE: u16 = 0x4;
const DEATH_FEIGN: u16 = 0x20;
/// player_death customkill values.
const CUSTOM_KILL_HEADSHOT: u16 = 1;
const CUSTOM_KILL_BACKSTAB: u16 = 2;
/// Recent frames kept to resolve deltas against.
const MAX_FRAMES: usize = 128;
/// CObjectSentrygun m_iState while shooting at a target.
const SENTRY_STATE_ATTACKING: i64 = 2;
/// User message id of SayText (console `say` and server notices).
const USER_MESSAGE_SAY_TEXT: u8 = 3;
/// Unattended cap progress decays over mp_capdeteriorate_time (default 90 s).
const CAP_DECAY_SECONDS: f32 = 90.0;
/// The handle value of "no entity".
const INVALID_EHANDLE: i64 = 0x1f_ffff;
/// player_spawn events waiting for a position; more than this means entity updates stopped.
const MAX_PENDING_SPAWNS: usize = 256;
/// Seconds per server tick until the server's own value (ServerInfo) arrives.
pub(crate) const DEFAULT_TICK_INTERVAL: f32 = 0.015;

/// `STVTF_DEBUG`, read once: a server class, temp entity class, `ent:N` or `gameevents`.
static DEBUG: LazyLock<Option<String>> = LazyLock::new(|| std::env::var("STVTF_DEBUG").ok());

/// Whether `STVTF_DEBUG` asks for this topic.
pub(crate) fn debug(topic: &str) -> bool {
    DEBUG.as_deref() == Some(topic)
}

/// The entity `STVTF_DEBUG=ent:N` asks to trace.
fn debug_entity() -> Option<u32> {
    DEBUG.as_deref()?.strip_prefix("ent:")?.parse().ok()
}

/// A send prop by send table and name. Elements of an array prop have the array as
/// table and their index as name.
const fn prop_id(table: &str, name: &str) -> SendPropIdentifier {
    SendPropIdentifier::new(table, name)
}

const PROP_TEAM_NUM: SendPropIdentifier = prop_id("DT_BaseEntity", "m_iTeamNum");
const PROP_ORIGIN: SendPropIdentifier = prop_id("DT_BaseEntity", "m_vecOrigin");
const PROP_ROTATION: SendPropIdentifier = prop_id("DT_BaseEntity", "m_angRotation");
/// The condition words of a player: m_nPlayerCond, _condition_bits, m_nPlayerCondEx..Ex4.
const PROP_CONDS: [SendPropIdentifier; 6] = [
    prop_id("DT_TFPlayerShared", "m_nPlayerCond"),
    prop_id("DT_TFPlayerConditionListExclusive", "_condition_bits"),
    prop_id("DT_TFPlayerShared", "m_nPlayerCondEx"),
    prop_id("DT_TFPlayerShared", "m_nPlayerCondEx2"),
    prop_id("DT_TFPlayerShared", "m_nPlayerCondEx3"),
    prop_id("DT_TFPlayerShared", "m_nPlayerCondEx4"),
];
const PROP_TEAM_TEAM_NUM: SendPropIdentifier = prop_id("DT_Team", "m_iTeamNum");
const PROP_TEAM_SCORE: SendPropIdentifier = prop_id("DT_Team", "m_iScore");
const PROP_FLAG_CAPTURES: SendPropIdentifier = prop_id("DT_TFTeam", "m_nFlagCaptures");
const PROP_TIMER_END_TIME: SendPropIdentifier = prop_id("DT_TeamRoundTimer", "m_flTimerEndTime");
const PROP_TIMER_REMAINING: SendPropIdentifier = prop_id("DT_TeamRoundTimer", "m_flTimeRemaining");
const PROP_TIMER_PAUSED: SendPropIdentifier = prop_id("DT_TeamRoundTimer", "m_bTimerPaused");
const PROP_TIMER_SHOW_IN_HUD: SendPropIdentifier = prop_id("DT_TeamRoundTimer", "m_bShowInHUD");
const PROP_TIMER_DISABLED: SendPropIdentifier = prop_id("DT_TeamRoundTimer", "m_bIsDisabled");
const PROP_TIMER_STATE: SendPropIdentifier = prop_id("DT_TeamRoundTimer", "m_nState");
const PROP_SENTRY_POSE_PITCH: SendPropIdentifier = prop_id("m_flPoseParameter", "000");
const PROP_SENTRY_POSE_YAW: SendPropIdentifier = prop_id("m_flPoseParameter", "001");
const PROP_SENTRY_ENEMY: SendPropIdentifier = prop_id("DT_ObjectSentrygun", "m_hEnemy");
const PROP_SENTRY_STATE: SendPropIdentifier = prop_id("DT_ObjectSentrygun", "m_iState");
const PROP_SENTRY_SHELLS: SendPropIdentifier = prop_id("DT_ObjectSentrygun", "m_iAmmoShells");
const PROP_WATCHER_PROGRESS: SendPropIdentifier = prop_id("DT_TeamTrainWatcher", "m_flTotalProgress");
const PROP_WATCHER_CAPPERS: SendPropIdentifier = prop_id("DT_TeamTrainWatcher", "m_nNumCappers");
const PROP_WATCHER_SPEED: SendPropIdentifier = prop_id("DT_TeamTrainWatcher", "m_iTrainSpeedLevel");
const PROP_WATCHER_GLOW: SendPropIdentifier = prop_id("DT_TeamTrainWatcher", "m_hGlowEnt");
const PROP_OWNER_ENTITY: SendPropIdentifier = prop_id("DT_BaseEntity", "m_hOwnerEntity");
const PROP_FLAG_STATUS: SendPropIdentifier = prop_id("DT_CaptureFlag", "m_nFlagStatus");
const PROP_FLAG_DISABLED: SendPropIdentifier = prop_id("DT_CaptureFlag", "m_bDisabled");
const PROP_FLAG_RESET_TIME: SendPropIdentifier = prop_id("DT_CaptureFlag", "m_flResetTime");
const PROP_ROUND_STATE: SendPropIdentifier = prop_id("DT_TeamplayRoundBasedRules", "m_iRoundState");
const PROP_CARRYING: SendPropIdentifier = prop_id("DT_TFPlayerShared", "m_bCarryingObject");
const PROP_CARRIED: SendPropIdentifier = prop_id("DT_TFPlayerShared", "m_hCarriedObject");
const PROP_RED_KOTH_TIMER: SendPropIdentifier = prop_id("DT_TFGameRules", "m_hRedKothTimer");
const PROP_BLUE_KOTH_TIMER: SendPropIdentifier = prop_id("DT_TFGameRules", "m_hBlueKothTimer");
const PROP_MAP_RESET_TIME: SendPropIdentifier = prop_id("DT_TeamplayRoundBasedRules", "m_flMapResetTime");
const PROP_STATE_TRANSITION_TIME: SendPropIdentifier = prop_id("DT_TeamplayRoundBasedRules", "m_flStateTransitionTime");
const PROP_WEAPON_OWNER: SendPropIdentifier = prop_id("DT_BaseCombatWeapon", "m_hOwner");
const PROP_ACTIVE_WEAPON: SendPropIdentifier = prop_id("DT_BaseCombatCharacter", "m_hActiveWeapon");
const PROP_CLIP: SendPropIdentifier = prop_id("DT_LocalWeaponData", "m_iClip1");
const PROP_ITEM_DEFINITION: SendPropIdentifier = prop_id("DT_ScriptCreatedItem", "m_iItemDefinitionIndex");
const PROP_MEDIGUN_RELEASING: SendPropIdentifier = prop_id("DT_WeaponMedigun", "m_bChargeRelease");
const PROP_MEDIGUN_RESIST: SendPropIdentifier = prop_id("DT_WeaponMedigun", "m_nChargeResistType");
const PROP_MEDIGUN_TARGET: SendPropIdentifier = prop_id("DT_WeaponMedigun", "m_hHealingTarget");
const PROP_FLAMETHROWER_STATE: SendPropIdentifier = prop_id("DT_WeaponFlameThrower", "m_iWeaponState");
const PROP_FLAMETHROWER_CRIT: SendPropIdentifier = prop_id("DT_WeaponFlameThrower", "m_bCritFire");
const PROP_FLAMETHROWER_HIT: SendPropIdentifier = prop_id("DT_WeaponFlameThrower", "m_bHitTarget");
/// m_iWeaponState: idle, starting to fire, firing, airblast.
const FLAMETHROWER_AIRBLAST: i64 = 3;
const PROP_BULLETS_PLAYER: SendPropIdentifier = prop_id("DT_TEFireBullets", "m_iPlayer");
const PROP_BULLETS_ORIGIN: SendPropIdentifier = prop_id("DT_TEFireBullets", "m_vecOrigin");
const PROP_BULLETS_PITCH: SendPropIdentifier = prop_id("DT_TEFireBullets", "m_vecAngles[0]");
const PROP_BULLETS_YAW: SendPropIdentifier = prop_id("DT_TEFireBullets", "m_vecAngles[1]");
const PROP_BULLETS_WEAPON: SendPropIdentifier = prop_id("DT_TEFireBullets", "m_iWeaponID");
const PROP_BULLETS_CRIT: SendPropIdentifier = prop_id("DT_TEFireBullets", "m_bCritical");
const PROP_EXPLOSION_ORIGIN: [SendPropIdentifier; 3] = [
    prop_id("DT_TETFExplosion", "m_vecOrigin[0]"),
    prop_id("DT_TETFExplosion", "m_vecOrigin[1]"),
    prop_id("DT_TETFExplosion", "m_vecOrigin[2]"),
];
const PROP_EXPLOSION_WEAPON: SendPropIdentifier = prop_id("DT_TETFExplosion", "m_iWeaponID");

#[derive(Default)]
pub(crate) struct Extractor {
    gs: GameStateAnalyser,
    /// Latest server tick seen in a net_Tick.
    pub(crate) server_tick: u32,
    events: RefCell<Vec<Value>>,
    world_sent: Cell<bool>,
    control_points: Vec<ControlPoint>,
    num_control_points: usize,
    /// Condition words per player entity: m_nPlayerCond, _condition_bits, m_nPlayerCondEx..Ex4.
    /// (tf-demo-parser 0.6.4's own `conditions()` decodes these wrongly.)
    conds: HashMap<u32, [u32; 6]>,
    mediguns: HashMap<u32, Medigun>,
    flamethrowers: HashMap<u32, Flamethrower>,
    weapons: HashMap<u32, Weapon>,
    /// Per server class id: whether its send table has an item definition index.
    weapon_classes: Vec<Option<bool>>,
    active_weapons: HashMap<u32, u32>,
    class_names: Vec<String>,
    /// Seconds per tick from ServerInfo.
    tick_interval: Option<f32>,
    teams: BTreeMap<u32, TeamScore>,
    timers: BTreeMap<u32, RoundTimer>,
    /// King of the hill: each team's own timer, which runs while that team holds the point.
    koth_timers: [Option<u32>; 2],
    round_state: RoundState,
    /// Server time the match started (m_flMapResetTime) and the current round went live.
    match_start: f32,
    round_live_since: Option<f32>,
    /// When the live round ended (win, stalemate): its time stops there.
    round_ended_at: Option<f32>,
    state_transition: f32,
    /// Where each player entity last was while alive: a dead player's own position is
    /// their spectator camera, which follows teammates.
    last_alive: HashMap<u32, Pose>,
    /// mp_timelimit (minutes) and mp_winlimit, replicated to clients as convars.
    time_limit: Option<f32>,
    win_limit: Option<i64>,
    /// Per medic entity, to report changes.
    medic_charge: HashMap<u32, Charge>,
    /// Payload: tf-demo-parser takes every func_tracktrain for a cart, but only a payload
    /// map has a team_train_watcher (KotH and other maps use trains for lifts and props).
    train_watchers: BTreeMap<u32, TrainWatcher>,
    /// Entity -> the entity it is attached to (`moveparent`), to find a watcher's cart.
    parents: HashMap<u32, u32>,
    /// World positions of prop entities: a payload cart can be a free physics prop.
    prop_origins: HashMap<u32, Vector>,
    sentries: HashMap<u32, Sentry>,
    /// CTF flags and the bball ball (item_teamflag), by entity.
    flags: BTreeMap<u32, Flag>,
    /// player_spawn arrives before the player's new position: held until the next entity update.
    pending_spawns: Vec<(u16, Value)>,
    /// The entities in each recent frame, by server tick, to find the ones a delta drops silently.
    frames: VecDeque<(u32, HashSet<u32>)>,
    /// Players who disconnected: the server keeps their slot, and so their entity and info, for
    /// a while after. A user id is not reused while the server runs.
    left: HashSet<u16>,
    /// Scoreboard lines from the player resource, by player entity.
    stats: HashMap<u32, Stats>,
    /// Engineers carrying a building: player entity -> the building's entity.
    carried: HashMap<u32, u32>,
    /// What each building entity is: a carried building is not sent while it is carried.
    building_kinds: HashMap<u32, &'static str>,
}

/// A player's line on the scoreboard. Points, kills and deaths are the server's match totals;
/// damage, healing and assists are counted from events, as logs.tf does.
#[derive(Default, Clone)]
struct Stats {
    points: i64,
    kills: i64,
    deaths: i64,
    assists: i64,
    damage: i64,
    healing: i64,
    dominations: i64,
    streak: i64,
    respawn_at: f32,
    carrying: bool,
}

/// A team entity's team number and score.
#[derive(Default, Clone, Copy)]
struct TeamScore {
    team: u8,
    score: i64,
    /// Flags captured this round, the score of a CTF round.
    captures: i64,
}

/// Where a player was and where they looked.
#[derive(Clone, Copy)]
struct Pose {
    pos: Vector,
    yaw: f32,
    pitch: f32,
}

/// A medic's charge and whether it was being released.
#[derive(Clone, Copy, PartialEq)]
struct Charge {
    charge: u8,
    releasing: bool,
}

#[derive(Default, Clone)]
struct Sentry {
    /// Turret pose parameters (0..1 over the model's range): [pitch, yaw].
    pose: [f32; 2],
    /// The direction the sentry was placed in (tf-demo-parser leaves this 0).
    yaw: f32,
    enemy: Option<u32>,
    shells: Option<i64>,
    state: i64,
}

impl Sentry {
    /// World yaw of the turret: the yaw pose parameter covers a full turn, centred on the
    /// direction the sentry was placed in (checked against its targets while firing).
    fn aim(&self) -> f32 {
        (self.yaw + (0.5 - self.pose[1]) * 360.0).rem_euclid(360.0)
    }
}

/// An intelligence briefcase, or a neutral flag such as the bball ball.
#[derive(Default, Clone)]
struct Flag {
    team: u8,
    pos: Vector,
    /// 0 at home, 1 carried, 2 dropped.
    status: i64,
    disabled: bool,
    carrier: Option<u32>,
    /// Server time a dropped flag goes home.
    reset_time: f32,
}

#[derive(Default, Clone)]
struct TrainWatcher {
    team: u8,
    /// The cart model (m_hGlowEnt), which is attached to the cart's func_tracktrain.
    glow: Option<u32>,
    progress: f32,
    cappers: i64,
    speed: i64,
}

#[derive(Default, Clone)]
struct RoundTimer {
    team: u8,
    end_time: f32,
    paused: bool,
    remaining: f32,
    show: bool,
    disabled: bool,
    setup: bool,
}

impl RoundTimer {
    /// Seconds left at a server tick; the end time is in server time (tick * interval).
    fn remaining_at(&self, tick: u32, interval: f32) -> f32 {
        if self.paused { self.remaining } else { (self.end_time - tick as f32 * interval).max(0.0) }
    }
}

/// CTFGameRulesProxy m_iRoundState (gamerules_roundstate_t).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum RoundState {
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

impl RoundState {
    fn from_raw(v: i64) -> Self {
        match v {
            0 => Self::Init,
            1 => Self::Pregame,
            2 => Self::StartGame,
            3 => Self::Preround,
            4 => Self::Running,
            5 => Self::TeamWin,
            6 => Self::Restart,
            7 => Self::Stalemate,
            8 => Self::GameOver,
            9 => Self::Bonus,
            _ => Self::BetweenRounds,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Pregame => "pregame",
            Self::StartGame => "start_game",
            Self::Preround => "preround",
            Self::Running => "running",
            Self::TeamWin => "team_win",
            Self::Restart => "restart",
            Self::Stalemate => "stalemate",
            Self::GameOver => "game_over",
            Self::Bonus => "bonus",
            Self::BetweenRounds => "between_rounds",
        }
    }
}

#[derive(Default)]
struct Flamethrower {
    owner: Option<u32>,
    state: i64,
    crit: bool,
    hit: bool,
}

#[derive(Default, Clone)]
struct Weapon {
    item: i64,
    /// -1 and below is TF2's "no clip".
    clip: i64,
}

#[derive(Default, Clone)]
struct Medigun {
    owner: Option<u32>,
    item: i64,
    releasing: bool,
    resist: i64,
    target: Option<u32>,
}

/// tf-demo-parser names the common projectiles; the rest go by their server class.
fn projectile_kind(ty: ProjectileType, class: &str) -> &'static str {
    match ty {
        ProjectileType::Rocket => return "Rocket",
        ProjectileType::HealingArrow => return "HealingArrow",
        ProjectileType::Sticky => return "Sticky",
        ProjectileType::Pipe => return "Pipe",
        ProjectileType::Flare => return "Flare",
        ProjectileType::LooseCannon => return "LooseCannon",
        ProjectileType::Unknown => {}
    }
    match class {
        "CTFProjectile_Arrow" => "Arrow",
        "CTFProjectile_EnergyBall" => "EnergyBall",
        "CTFProjectile_EnergyRing" => "EnergyRing",
        "CTFProjectile_BallOfFire" => "Fireball",
        "CTFProjectile_Cleaver" => "Cleaver",
        "CTFProjectile_Jar" => "Jarate",
        "CTFProjectile_JarMilk" => "MadMilk",
        "CTFProjectile_JarGas" => "GasPasser",
        "CTFProjectile_MechanicalArmOrb" => "ShortCircuitOrb",
        _ => "Unknown",
    }
}

fn medigun_kind(item: i64) -> &'static str {
    match item {
        35 => "kritzkrieg",
        411 => "quickfix",
        998 => "vaccinator",
        _ => "medigun",
    }
}

fn resist_name(r: i64) -> &'static str {
    match r {
        0 => "bullet",
        1 => "blast",
        _ => "fire",
    }
}

/// Output names of the TF2 player conditions (ETFCond), by number; "" for unused numbers.
/// The names follow tf-demo-parser's `PlayerCondition`, but are spelled out here so the
/// output does not change with that crate's Debug formatting.
#[rustfmt::skip]
const COND_NAMES: [&str; 131] = [
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

/// Whether condition number `n` is set.
fn has_cond_bit(words: &[u32; 6], n: usize) -> bool {
    let word = if n < 32 { words[0] | words[1] } else { words.get(n / 32 + 1).copied().unwrap_or(0) };
    word >> (n % 32) & 1 == 1
}

fn has_cond(words: &[u32; 6], c: PlayerCondition) -> bool {
    has_cond_bit(words, c as usize)
}

fn cond_names(words: &[u32; 6]) -> Vec<&'static str> {
    COND_NAMES
        .iter()
        .enumerate()
        .filter(|&(n, name)| !name.is_empty() && has_cond_bit(words, n))
        .map(|(_, name)| *name)
        .collect()
}

fn chat_kind(kind: ChatMessageKind) -> &'static str {
    match kind {
        ChatMessageKind::ChatAll => "ChatAll",
        ChatMessageKind::ChatTeam => "ChatTeam",
        ChatMessageKind::ChatAllDead => "ChatAllDead",
        ChatMessageKind::ChatTeamDead => "ChatTeamDead",
        ChatMessageKind::ChatAllSpec => "ChatAllSpec",
        ChatMessageKind::NameChange => "NameChange",
        ChatMessageKind::Empty => "Empty",
    }
}

/// The medigun charge effect a player is under, from their conditions.
fn ubered(words: &[u32; 6]) -> Option<&'static str> {
    use PlayerCondition::*;
    let on = |c| has_cond(words, c);
    // InvulnerableWearingOff only marks the fade-out next to Invulnerable; servers have been
    // seen leaving it set on a medic for good, so it is not an über on its own.
    if on(Invulnerable) {
        Some("uber")
    } else if on(MegaHeal) {
        Some("quickfix")
    } else if on(MedigunUberBulletResist) {
        Some("vaccinator_bullet")
    } else if on(MedigunUberBlastResist) {
        Some("vaccinator_blast")
    } else if on(MedigunUberFireResist) {
        Some("vaccinator_fire")
    } else if on(CritBoosted) {
        Some("kritz")
    } else {
        None
    }
}

#[derive(Default, Clone)]
struct ControlPoint {
    pos: Option<Vector>,
    owner: u8,
    capping_team: u8,
    /// Progress 0..1 of the capping team as of `progress_tick`. The server sends it
    /// (as m_flLazyCapPerc, counting down) only now and then; in between it is advanced
    /// at `cap_rate`, rolled forward whenever an input of the rate changes.
    progress: f32,
    progress_tick: u32,
    locked: bool,
    blocked: bool,
    team_in_zone: u8,
    /// Per team: seconds to cap alone, and team members on the point.
    cap_time: [f32; 4],
    members: [u8; 4],
}

impl ControlPoint {
    /// Progress per second towards capping (negative while it decays back), the way
    /// TF2's client extrapolates between lazy updates: each extra capper adds 1/n.
    fn cap_rate(&self) -> f32 {
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

    fn progress_at(&self, tick: u32, interval: f32) -> f32 {
        if self.capping_team == 0 {
            return 0.0;
        }
        let elapsed = tick.saturating_sub(self.progress_tick) as f32 * interval;
        (self.progress + self.cap_rate() * elapsed).clamp(0.0, 1.0)
    }

    fn advance(&mut self, tick: u32, interval: f32) {
        self.progress = self.progress_at(tick, interval);
        self.progress_tick = tick;
    }
}

/// Entity handles carry the entity index in their low 11 bits.
fn handle_entity(handle: i64) -> u32 {
    (handle & 0x7ff) as u32
}

/// The entity of a building's builder. tf-demo-parser stores the builder's entity
/// handle (m_hBuilder) in a `UserId`, so despite its type it is not a user id.
fn builder_entity(builder: UserId) -> u32 {
    handle_entity(i64::from(u16::from(builder)))
}

/// The entity a handle prop points at; none for the invalid handle (and 0, the world).
fn opt_handle(handle: i64) -> Option<u32> {
    (handle != INVALID_EHANDLE && handle != 0).then(|| handle_entity(handle))
}

/// A team number prop; out of range values are no team.
fn team_byte(v: i64) -> u8 {
    u8::try_from(v).unwrap_or(0)
}

fn team_name(t: Team) -> &'static str {
    match t {
        Team::Red => "red",
        Team::Blue => "blue",
        Team::Spectator => "spectator",
        Team::Other => "none",
    }
}

pub(crate) fn team_num<N>(n: N) -> &'static str
where
    u8: TryFrom<N>,
{
    team_name(Team::new(n))
}

fn class_name(c: Class) -> &'static str {
    match c {
        Class::Scout => "scout",
        Class::Sniper => "sniper",
        Class::Soldier => "soldier",
        Class::Demoman => "demoman",
        Class::Medic => "medic",
        Class::Heavy => "heavy",
        Class::Pyro => "pyro",
        Class::Spy => "spy",
        Class::Engineer => "engineer",
        Class::Other => "unknown",
    }
}

fn class_num(n: u16) -> &'static str {
    class_name(Class::new(i64::from(n)))
}

/// A position as written out, to a tenth of a unit.
fn xyz(v: Vector) -> [f64; 3] {
    [round1(v.x), round1(v.y), round1(v.z)]
}

fn vec3(v: Vector) -> Value {
    json!(xyz(v))
}

fn round2(f: f32) -> f64 {
    (f64::from(f) * 100.0).round() / 100.0
}

fn round1(f: f32) -> f64 {
    (f64::from(f) * 10.0).round() / 10.0
}

/// A prop value as a number, whichever number type the table gave it; 0 otherwise.
fn as_int(v: &SendPropValue) -> i64 {
    match v {
        SendPropValue::Integer(i) => *i,
        SendPropValue::Float(f) => *f as i64,
        _ => 0,
    }
}

fn as_float(v: &SendPropValue) -> f32 {
    match v {
        SendPropValue::Float(f) => *f,
        SendPropValue::Integer(i) => *i as f32,
        _ => 0.0,
    }
}

fn prop(props: &[SendProp], id: SendPropIdentifier) -> Option<&SendPropValue> {
    props.iter().find(|p| p.identifier == id).map(|p| &p.value)
}

fn prop_f(props: &[SendProp], id: SendPropIdentifier) -> f32 {
    prop(props, id).map_or(0.0, as_float)
}

fn prop_i(props: &[SendProp], id: SendPropIdentifier) -> i64 {
    prop(props, id).map_or(0, as_int)
}

fn prop_vec(props: &[SendProp], id: SendPropIdentifier) -> Option<Vector> {
    match prop(props, id)? {
        SendPropValue::Vector(v) => Some(*v),
        _ => None,
    }
}

impl Extractor {
    fn state(&self) -> &GameState {
        &self.gs.state
    }

    /// Seconds per server tick.
    pub(crate) fn interval(&self) -> f32 {
        self.tick_interval.unwrap_or(DEFAULT_TICK_INTERVAL)
    }

    fn projectile_kind(&self, pr: &Projectile) -> &'static str {
        projectile_kind(pr.ty, self.class_names.get(usize::from(pr.class)).map_or("", String::as_str))
    }

    /// While running, the last state transition is when the round went live; a win or
    /// stalemate stops its clock and the next pre-round clears it.
    fn track_round_time(&mut self) {
        let now = self.server_tick as f32 * self.interval();
        match self.round_state {
            RoundState::Running => (self.round_live_since, self.round_ended_at) = (Some(self.state_transition), None),
            RoundState::Preround => (self.round_live_since, self.round_ended_at) = (None, None),
            _ if self.round_live_since.is_some() && self.round_ended_at.is_none() => self.round_ended_at = Some(now),
            _ => {}
        }
    }

    /// Seconds left of mp_timelimit, which counts from the start of the match.
    fn time_left(&self) -> Option<f32> {
        let limit = self.time_limit?;
        (self.match_start > 0.0)
            .then(|| (limit * 60.0 - (self.server_tick as f32 * self.interval() - self.match_start)).max(0.0))
    }

    fn round_time(&self) -> Option<f32> {
        let now = self.server_tick as f32 * self.interval();
        self.round_live_since.map(|t| (self.round_ended_at.unwrap_or(now) - t).max(0.0))
    }

    /// `uber_ready` when a medic's charge fills up, `uber_ended` when a released charge runs out.
    fn medic_changes(&mut self) {
        let mut events = Vec::new();
        for p in &self.gs.state.players {
            let ent = u32::from(p.entity);
            let Some(info) = p.info.as_ref() else { continue };
            if p.class != Class::Medic || p.state != PlayerState::Alive {
                self.medic_charge.remove(&ent);
                continue;
            }
            let gun = self.mediguns.values().find(|m| m.owner == Some(ent));
            let now = Charge { charge: p.charge, releasing: gun.is_some_and(|m| m.releasing) };
            if let Some(&before) = self.medic_charge.get(&ent) {
                let medigun = gun.map(|m| medigun_kind(m.item));
                if before.charge < 100 && now.charge >= 100 && !now.releasing {
                    events.push(json!({"type": "uber_ready", "medic": u16::from(info.user_id), "medigun": medigun}));
                }
                if before.releasing && !now.releasing {
                    events.push(json!({"type": "uber_ended", "medic": u16::from(info.user_id), "medigun": medigun}));
                }
            }
            self.medic_charge.insert(ent, now);
        }
        for e in events {
            self.push(e);
        }
    }

    fn forget_entity(&mut self, idx: u32) {
        self.last_alive.remove(&idx);
        self.conds.remove(&idx);
        self.mediguns.remove(&idx);
        self.flamethrowers.remove(&idx);
        self.weapons.remove(&idx);
        self.active_weapons.remove(&idx);
        self.teams.remove(&idx);
        self.timers.remove(&idx);
        self.train_watchers.remove(&idx);
        self.parents.remove(&idx);
        self.prop_origins.remove(&idx);
        self.sentries.remove(&idx);
        self.medic_charge.remove(&idx);
        self.flags.remove(&idx);
    }

    fn entity_of_user(&self, user_id: u16) -> Option<u32> {
        self.state()
            .players
            .iter()
            .find(|p| p.info.as_ref().is_some_and(|i| u16::from(i.user_id) == user_id))
            .map(|p| u32::from(p.entity))
    }

    fn weapon_of(&self, player_ent: u32) -> Option<WeaponOut> {
        let w = self.weapons.get(self.active_weapons.get(&player_ent)?)?;
        Some(WeaponOut { item: w.item, clip: (w.clip >= 0).then_some(w.clip) })
    }

    fn medigun_of(&self, player_ent: u32) -> Option<MedigunOut> {
        let m = self.mediguns.values().find(|m| m.owner == Some(player_ent))?;
        let kind = medigun_kind(m.item);
        Some(MedigunOut {
            kind,
            releasing: m.releasing,
            resist: (kind == "vaccinator").then(|| resist_name(m.resist)),
            target: m.target.and_then(|t| self.user_of_entity(t)),
        })
    }

    /// A pyro's flamethrower while it is firing.
    fn flames_of(&self, player_ent: u32) -> Option<FlamesOut> {
        let f = self.flamethrowers.values().find(|f| f.owner == Some(player_ent) && matches!(f.state, 1 | 2))?;
        Some(FlamesOut { crit: f.crit, hit: f.hit })
    }

    /// Queues an event, stamped with the current tick unless it has its own.
    fn push(&self, mut v: Value) {
        if let Some(obj) = v.as_object_mut() {
            obj.entry("tick").or_insert_with(|| json!(self.server_tick));
        }
        self.events.borrow_mut().push(v);
    }

    /// Takes the queued events.
    pub(crate) fn drain(&self) -> Vec<Value> {
        self.events.take()
    }

    fn user_pos(&self, user_id: u16) -> Value {
        self.state()
            .players
            .iter()
            .find(|p| p.info.as_ref().is_some_and(|i| u16::from(i.user_id) == user_id))
            .map_or(Value::Null, |p| vec3(p.position))
    }

    fn user_of_entity(&self, ent: u32) -> Option<u16> {
        self.state()
            .players
            .iter()
            .find(|p| u32::from(p.entity) == ent)
            .and_then(|p| p.info.as_ref())
            .map(|i| u16::from(i.user_id))
    }

    /// One prop of CTFObjectiveResource. Its per point arrays come as elements named
    /// after the array with the index as name, so they go by name rather than identifier.
    fn objective_prop(&mut self, p: &SendProp) {
        let Some((table, name)) = p.identifier.names() else { return };
        if name.as_str() == "m_vCPPositions" {
            if let SendPropValue::Array(items) = &p.value {
                if self.control_points.len() < items.len() {
                    self.control_points.resize(items.len().min(8), ControlPoint::default());
                }
                for (cp, item) in self.control_points.iter_mut().zip(items) {
                    if let SendPropValue::Vector(v) = item {
                        cp.pos = Some(*v);
                    }
                }
            }
            return;
        }
        if name.as_str() == "m_iNumControlPoints" {
            if let SendPropValue::Integer(n) = p.value {
                self.num_control_points = n.clamp(0, 8) as usize;
            }
            return;
        }
        let Ok(idx) = name.as_str().parse::<usize>() else { return };
        // Per team and point arrays: index = team * 8 + point (TEAM_ARRAY).
        if matches!(table.as_str(), "m_flTeamCapTime" | "m_iNumTeamMembers") {
            let (team, point) = (idx / 8, idx % 8);
            if team >= 4 {
                return;
            }
            if self.control_points.len() <= point {
                self.control_points.resize(point + 1, ControlPoint::default());
            }
            let (tick, interval) = (self.server_tick, self.interval());
            let cp = &mut self.control_points[point];
            cp.advance(tick, interval);
            match &p.value {
                SendPropValue::Float(f) if table.as_str() == "m_flTeamCapTime" => cp.cap_time[team] = *f,
                SendPropValue::Integer(i) => cp.members[team] = (*i).clamp(0, 255) as u8,
                _ => {}
            }
            return;
        }
        if idx >= 8 {
            return;
        }
        let (tick, interval) = (self.server_tick, self.interval());
        if self.control_points.len() <= idx {
            self.control_points.resize(idx + 1, ControlPoint::default());
        }
        let cp = &mut self.control_points[idx];
        cp.advance(tick, interval);
        let int = as_int(&p.value);
        match table.as_str() {
            "m_vCPPositions" => {
                if let SendPropValue::Vector(v) = p.value {
                    cp.pos = Some(v);
                }
            }
            "m_iOwner" => cp.owner = team_byte(int),
            "m_iCappingTeam" => {
                if cp.capping_team != team_byte(int) {
                    cp.progress = 0.0;
                }
                cp.capping_team = team_byte(int);
            }
            "m_flLazyCapPerc" => {
                if let SendPropValue::Float(f) = p.value {
                    cp.progress = (1.0 - f).clamp(0.0, 1.0);
                }
            }
            "m_iTeamInZone" => cp.team_in_zone = team_byte(int),
            "m_bCPLocked" => cp.locked = int != 0,
            "m_bBlocked" => cp.blocked = int != 0,
            _ => {}
        }
    }

    fn opt_user(id: u16) -> Value {
        if id == 0 { Value::Null } else { json!(id) }
    }

    fn game_event(&mut self, event: &GameEvent) {
        self.count_score(event);
        match event {
            GameEvent::PlayerDeath(d) => {
                // Building kills (sentries) report the building as inflictor; attacker stays the engineer.
                self.push(json!({
                    "type": "kill",
                    "victim": d.user_id,
                    "attacker": Self::opt_user(if d.attacker == d.user_id { 0 } else { d.attacker }),
                    "assister": if d.assister == u16::MAX { Value::Null } else { Self::opt_user(d.assister) },
                    "weapon": d.weapon.to_string(),
                    "crit": d.damage_bits & DMG_CRIT != 0,
                    "headshot": d.custom_kill == CUSTOM_KILL_HEADSHOT,
                    "backstab": d.custom_kill == CUSTOM_KILL_BACKSTAB,
                    "suicide": d.attacker == 0 || d.attacker == d.user_id,
                    "dominated": d.death_flags & DEATH_DOMINATION != 0,
                    "revenge": d.death_flags & DEATH_REVENGE != 0,
                    // A spy's Dead Ringer: the victim is not actually dead.
                    "feign": d.death_flags & DEATH_FEIGN != 0,
                    // The victim was blast jumping: an airshot for projectiles.
                    "airborne": d.rocket_jump,
                    "victim_pos": self.user_pos(d.user_id),
                    "attacker_pos": self.user_pos(d.attacker),
                }));
            }
            GameEvent::PlayerHurt(h) => self.push(json!({
                "type": "damage",
                "victim": h.user_id,
                "attacker": Self::opt_user(h.attacker),
                "amount": h.damage_amount,
                "health": h.health,
                "crit": h.crit,
                "minicrit": h.mini_crit,
                "weapon_id": h.weapon_id,
            })),
            GameEvent::PlayerSpawn(s) => {
                if self.pending_spawns.len() < MAX_PENDING_SPAWNS {
                    self.pending_spawns.push((
                        s.user_id,
                        json!({
                            "type": "spawn",
                            "player": s.user_id,
                            "team": team_num(s.team),
                            "class": class_num(s.class),
                            "tick": self.server_tick,
                        }),
                    ));
                }
            }
            GameEvent::PlayerChargeDeployed(c) => self.push(json!({
                "type": "uber",
                "medic": c.user_id,
                "target": Self::opt_user(c.target_id),
                "medigun": self
                    .entity_of_user(c.user_id)
                    .and_then(|e| self.mediguns.values().find(|m| m.owner == Some(e)))
                    .map(|m| medigun_kind(m.item)),
            })),
            GameEvent::PlayerTeam(t) => self.push(json!({
                "type": "team_change",
                "player": t.user_id,
                "team": team_num(t.team),
                "old_team": team_num(t.old_team),
                "disconnect": t.disconnect,
            })),
            GameEvent::PlayerChangeClass(c) => self.push(json!({
                "type": "class_change",
                "player": c.user_id,
                "class": class_num(c.class),
            })),
            GameEvent::PlayerDisconnect(d) => {
                self.left.insert(d.user_id);
                self.push(json!({
                    "type": "player_leave",
                    "player": d.user_id,
                    "name": d.name.to_string(),
                    "reason": d.reason.to_string(),
                }));
            }
            GameEvent::PlayerBuiltObject(b) => self.push(json!({
                "type": "build",
                "player": b.user_id,
                "object": building_kind(b.object),
                "id": b.index,
            })),
            GameEvent::PlayerUpgradedObject(u) => self.push(json!({
                "type": "building_upgrade",
                "player": u.user_id,
                "object": building_kind(u.object),
                "id": u.index,
            })),
            GameEvent::BuildingHealed(b) => self.push(json!({
                "type": "building_repair",
                "id": b.building,
                "amount": b.amount,
            })),
            GameEvent::TeamPlayTimerTimeAdded(t) => {
                let idx = u32::from(t.timer);
                let team = match self.koth_timers.iter().position(|k| *k == Some(idx)) {
                    Some(0) => "red",
                    Some(_) => "blue",
                    None => self.timers.get(&idx).map_or("none", |t| team_num(t.team)),
                };
                self.push(json!({"type": "time_added", "team": team, "seconds": t.seconds_added}));
            }
            GameEvent::ObjectDestroyed(o) => self.push(json!({
                "type": "building_destroyed",
                "owner": o.user_id,
                "attacker": Self::opt_user(o.attacker),
                "object": building_kind(o.object_type),
                "id": o.index,
                "weapon": o.weapon.to_string(),
                "was_building": o.was_building,
            })),
            GameEvent::TeamPlayRoundStart(r) => self.push(json!({"type": "round_start", "full_reset": r.full_reset})),
            GameEvent::TeamPlayRoundActive(_) => self.push(json!({"type": "round_active"})),
            GameEvent::TeamPlaySetupFinished(_) => self.push(json!({"type": "setup_finished"})),
            GameEvent::TeamPlayRoundWin(w) => self.push(json!({
                "type": "round_win",
                "team": team_num(w.team),
                "reason": w.win_reason,
                "round_time": w.round_time,
            })),
            GameEvent::TeamPlayPointCaptured(c) => self.push(json!({
                "type": "point_captured",
                "point": c.cp,
                "name": c.cp_name.to_string(),
                "team": team_num(c.team),
                "cappers": c.cappers.as_bytes().iter().map(|&e| self.user_of_entity(u32::from(e))).collect::<Vec<_>>(),
            })),
            // `player` and `carrier` are entity indexes here, not user ids.
            GameEvent::TeamPlayFlagEvent(f) => {
                let event = match f.event_type {
                    1 => "pickup",
                    2 => "captured",
                    3 => "defended",
                    4 => "dropped",
                    5 => "returned",
                    _ => return,
                };
                self.push(json!({
                    "type": "flag",
                    "event": event,
                    "player": self.user_of_entity(u32::from(f.player)),
                    "carrier": (f.carrier != 0).then(|| self.user_of_entity(u32::from(f.carrier))).flatten(),
                    "team": team_num(f.team),
                }));
            }
            GameEvent::MedicDeath(m) => self.push(json!({
                "type": "medic_death",
                "medic": m.user_id,
                "attacker": Self::opt_user(m.attacker),
                "healing": m.healing,
                // Died with a full charge: a dropped über.
                "drop": m.charged,
            })),
            GameEvent::TeamPlayCaptureBlocked(b) => self.push(json!({
                "type": "capture_blocked",
                "point": b.cp,
                "name": b.cp_name.to_string(),
                "blocker": self.user_of_entity(u32::from(b.blocker)),
                "victim": self.user_of_entity(u32::from(b.victim)),
            })),
            GameEvent::ObjectDeflected(o) => self.push(json!({
                "type": "reflect",
                "player": o.user_id,
                "owner": Self::opt_user(o.owner_id),
                "weapon_id": o.weapon_id,
            })),
            GameEvent::DeployBuffBanner(b) => self.push(json!({
                "type": "banner",
                "player": b.buff_owner,
                "kind": match b.buff_type { 1 => "buff_banner", 2 => "battalions_backup", 3 => "concheror", _ => "other" },
            })),
            GameEvent::TeamPlayRoundStalemate(_) => self.push(json!({"type": "stalemate"})),
            GameEvent::TeamPlayOvertimeBegin(_) => self.push(json!({"type": "overtime"})),
            GameEvent::TeamPlayWinPanel(w) => self.push(json!({
                "type": "scores",
                "red": w.red_score,
                "blue": w.blue_score,
                "game_over": w.game_over != 0,
            })),
            GameEvent::TeamPlayGameOver(g) => self.push(json!({"type": "game_over", "reason": g.reason.to_string()})),
            GameEvent::TfGameOver(g) => self.push(json!({"type": "game_over", "reason": g.reason.to_string()})),
            _ => {
                let class_of_user = |id: u16| {
                    let state = self.state();
                    let p = state.players.iter().find(|p| p.info.as_ref().is_some_and(|i| u16::from(i.user_id) == id));
                    p.map(|p| p.class)
                };
                let user_of_entity = |ent: u32| self.user_of_entity(ent);
                let look = detail::Lookup { user_of_entity: &user_of_entity, class_of_user: &class_of_user };
                if let Some(v) = detail::game_event(event, &look) {
                    self.push(v);
                }
            }
        }
    }

    fn temp_entity(&mut self, class: &str, props: &[SendProp]) {
        if debug(class) {
            for p in props {
                eprintln!("{class} {:?} = {:?}", p.identifier.names(), p.value);
            }
        }
        match class {
            "CTEFireBullets" => {
                // m_iPlayer is the player's entity index minus one.
                let player = u32::try_from(prop_i(props, PROP_BULLETS_PLAYER))
                    .ok()
                    .and_then(|i| i.checked_add(1))
                    .and_then(|ent| self.user_of_entity(ent));
                self.push(json!({
                    "type": "shot",
                    "player": player,
                    "origin": vec3(prop_vec(props, PROP_BULLETS_ORIGIN).unwrap_or_default()),
                    "pitch": round1(prop_f(props, PROP_BULLETS_PITCH)),
                    "yaw": round1(prop_f(props, PROP_BULLETS_YAW)),
                    "weapon_id": prop_i(props, PROP_BULLETS_WEAPON),
                    "crit": prop_i(props, PROP_BULLETS_CRIT) != 0,
                }));
            }
            "CTETFExplosion" => {
                self.push(json!({
                    "type": "explosion",
                    "origin": PROP_EXPLOSION_ORIGIN.map(|id| round1(prop_f(props, id))),
                    "weapon_id": prop_i(props, PROP_EXPLOSION_WEAPON),
                }));
            }
            _ => {
                if let Some(v) = detail::temp_entity(class, props) {
                    self.push(v);
                }
            }
        }
    }

    /// Emits a `world` event once the world bounds are known.
    pub(crate) fn announce(&self) {
        if !self.world_sent.get()
            && let Some(w) = &self.state().world
        {
            let v = json!({"type": "world", "min": vec3(w.boundary_min), "max": vec3(w.boundary_max)});
            self.world_sent.set(true);
            self.push(v);
        }
    }

    pub(crate) fn snapshot(&self) -> Value {
        let gs = self.state();
        let interval = self.interval();
        let players = gs
            .players
            .iter()
            .filter_map(|p| {
                let info = p.info.as_ref()?;
                if self.left.contains(&u16::from(info.user_id)) {
                    return None;
                }
                // The SourceTV bot itself is a BOT on no playing team.
                if info.steam_id == "BOT" && !matches!(p.team, Team::Red | Team::Blue) && p.class == Class::Other {
                    return None;
                }
                let ent = u32::from(p.entity);
                let alive = p.state == PlayerState::Alive;
                // Dead: where they died, or nothing if that happened before we connected.
                let last = if alive {
                    Some(Pose { pos: p.position, yaw: p.view_angle, pitch: p.pitch_angle })
                } else {
                    self.last_alive.get(&ent).copied()
                };
                let conds = self.conds.get(&ent);
                Some(PlayerOut {
                    id: u16::from(info.user_id),
                    ent,
                    name: &info.name,
                    steamid: &info.steam_id,
                    team: team_name(p.team),
                    class: class_name(p.class),
                    alive,
                    hp: p.health,
                    max_hp: p.max_health,
                    pos: last.map(|l| xyz(l.pos)),
                    yaw: last.map(|l| round1(l.yaw)),
                    pitch: last.map(|l| round1(l.pitch)),
                    uber: p.charge,
                    ping: p.ping,
                    conds: conds.map(cond_names).unwrap_or_default(),
                    ubered: conds.and_then(ubered),
                    medigun: (p.class == Class::Medic).then(|| self.medigun_of(ent)).flatten(),
                    weapon: self.weapon_of(ent),
                    flames: (p.class == Class::Pyro && alive).then(|| self.flames_of(ent)).flatten(),
                    respawn: self
                        .stats
                        .get(&ent)
                        .filter(|_| !alive)
                        .map(|st| st.respawn_at - self.server_tick as f32 * interval)
                        .map(round1)
                        .filter(|left| *left > 0.0),
                    score: self.stats.get(&ent).map(|st| PlayerScoreOut {
                        points: st.points,
                        kills: st.kills,
                        deaths: st.deaths,
                        assists: st.assists,
                        damage: st.damage,
                        healing: st.healing,
                        dominations: st.dominations,
                        streak: st.streak,
                    }),
                    carrying: self
                        .carried
                        .get(&ent)
                        .filter(|_| alive && self.stats.get(&ent).is_some_and(|st| st.carrying))
                        .and_then(|b| self.building_kinds.get(b).copied()),
                })
            })
            .collect();
        let projectiles = gs
            .projectiles
            .values()
            .map(|pr| ProjectileOut {
                id: u32::from(pr.id),
                kind: self.projectile_kind(pr),
                yaw: round1(pr.rotation.y),
                team: team_name(pr.team),
                pos: xyz(pr.position),
                owner: gs
                    .players
                    .iter()
                    .find(|p| pr.launcher.0 != 0 && p.weapons.contains(&pr.launcher))
                    .and_then(|p| p.info.as_ref())
                    .map(|i| u16::from(i.user_id)),
                crit: pr.critical,
            })
            .collect();
        let carried: HashSet<u32> = self.carried.values().copied().collect();
        let buildings = gs
            .buildings
            .values()
            .filter(|b| !carried.contains(&u32::from(b.entity_id())))
            .map(|b| {
                let mut out = BuildingOut {
                    id: u32::from(b.entity_id()),
                    kind: building_kind_of(b),
                    team: team_name(b.team()),
                    level: b.level(),
                    hp: b.health(),
                    max_hp: b.max_health(),
                    pos: xyz(b.position()),
                    yaw: round1(b.angle()),
                    builder: self.user_of_entity(builder_entity(b.builder())),
                    sapped: b.sapped(),
                    entrance: None,
                    mini: None,
                    building: None,
                    aim: None,
                    target: None,
                    attacking: None,
                };
                if let Building::Teleporter(t) = b {
                    out.entrance = Some(t.is_entrance);
                }
                if let Building::Sentry(s) = b {
                    out.mini = Some(s.is_mini);
                    out.building = Some(s.building);
                    if let Some(t) = self.sentries.get(&u32::from(b.entity_id())) {
                        out.yaw = round1(t.yaw);
                        out.aim = Some(round1(t.aim()));
                        out.target = Some(t.enemy.and_then(|e| self.user_of_entity(e)));
                        out.attacking = Some(t.state == SENTRY_STATE_ATTACKING);
                    }
                }
                out
            })
            .collect();
        let mut objectives: Vec<ObjectiveOut> = self
            .control_points
            .iter()
            .take(self.num_control_points)
            .enumerate()
            .map(|(index, cp)| ObjectiveOut::ControlPoint {
                kind: "control_point",
                index,
                pos: cp.pos.map(xyz),
                owner: team_num(cp.owner),
                capping_team: team_num(cp.capping_team),
                // m_flLazyCapPerc counts down from 1.0 while a team caps; it is stale otherwise.
                cap_progress: round2(cp.progress_at(self.server_tick, interval)),
                cap_rate: (f64::from(cp.cap_rate()) * 1000.0).round() / 1000.0,
                locked: cp.locked,
                blocked: cp.blocked,
                on_point: OnPoint { red: cp.members[2], blue: cp.members[3] },
            })
            .collect();
        let carts: Vec<_> =
            gs.objectives.iter().filter_map(|(id, o)| o.as_cart().map(|c| (u32::from(*id), c))).collect();
        let mut watchers: Vec<&TrainWatcher> = self.train_watchers.values().collect();
        watchers.sort_by_key(|w| w.team);
        for (i, w) in watchers.iter().enumerate() {
            // The cart model either rides on a func_tracktrain (its moveparent) or is a free
            // physics prop with its own position; without either, pair trains by order.
            let on_train = w.glow.and_then(|g| self.parents.get(&g)).and_then(|t| carts.iter().find(|(id, _)| id == t));
            let (id, pos) = match (on_train, w.glow.and_then(|g| self.prop_origins.get(&g).map(|p| (g, *p)))) {
                (Some((id, c)), _) => (*id, c.position),
                (None, Some(free)) => free,
                (None, None) if carts.len() == watchers.len() => (carts[i].0, carts[i].1.position),
                _ => continue,
            };
            objectives.push(ObjectiveOut::Cart {
                kind: "cart",
                id,
                pos: xyz(pos),
                team: team_num(w.team),
                progress: round2(w.progress),
                cappers: w.cappers,
                speed: w.speed,
            });
        }
        let now = self.server_tick as f32 * interval;
        for (&id, f) in self.flags.iter().filter(|(_, f)| !f.disabled) {
            // A carried flag is attached to its carrier, its own origin relative to them.
            let carrier = gs.players.iter().find(|p| f.status == 1 && Some(u32::from(p.entity)) == f.carrier);
            objectives.push(ObjectiveOut::Flag {
                kind: "flag",
                id,
                pos: xyz(carrier.map_or(f.pos, |p| p.position)),
                team: team_num(f.team),
                status: match f.status {
                    1 => "carried",
                    2 => "dropped",
                    _ => "home",
                },
                carrier: carrier.and_then(|p| p.info.as_ref()).map(|i| u16::from(i.user_id)),
                returns_in: (f.status == 2 && f.reset_time > now).then(|| round1(f.reset_time - now)),
            });
        }
        let score = |team: Team| self.teams.values().find(|t| t.team == team as u8).map_or(0, |t| t.score);
        let captures = |team: Team| self.teams.values().find(|t| t.team == team as u8).map_or(0, |t| t.captures);
        let snapshot = SnapshotOut {
            kind: "snapshot",
            tick: self.server_tick,
            players,
            projectiles,
            buildings,
            objectives,
            round: self.round_state.name(),
            round_time: self.round_time().map(round1),
            time_left: self.time_left().map(round1),
            win_limit: self.win_limit,
            match_time: (self.match_start > 0.0)
                .then(|| round1((self.server_tick as f32 * interval - self.match_start).max(0.0))),
            score: ScoreOut { red: score(Team::Red), blue: score(Team::Blue) },
            captures: (!self.flags.is_empty())
                .then(|| ScoreOut { red: captures(Team::Red), blue: captures(Team::Blue) }),
            timers: self
                .timers
                .iter()
                // On king of the hill the HUD shows only the teams' timers; others are left from earlier rounds.
                .filter(|(idx, t)| {
                    t.show && !t.disabled && (self.koth_timers == [None; 2] || self.koth_timers.contains(&Some(**idx)))
                })
                .map(|(idx, t)| TimerOut {
                    team: match self.koth_timers.iter().position(|k| *k == Some(*idx)) {
                        Some(0) => "red",
                        Some(_) => "blue",
                        None => team_num(t.team),
                    },
                    remaining: round1(t.remaining_at(self.server_tick, interval)),
                    paused: t.paused,
                    setup: t.setup,
                })
                .collect(),
        };
        serde_json::to_value(snapshot).expect("a snapshot has only string map keys")
    }
}

// The snapshot as it is written out (see the README's Snapshot section). Floats are
// f64, as serde_json would otherwise print an f32 with its shortest f32 digits.

#[derive(Serialize)]
struct SnapshotOut<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    tick: u32,
    players: Vec<PlayerOut<'a>>,
    projectiles: Vec<ProjectileOut>,
    buildings: Vec<BuildingOut>,
    objectives: Vec<ObjectiveOut>,
    round: &'static str,
    /// Seconds since the round went live and since the match started (null if unknown).
    round_time: Option<f64>,
    time_left: Option<f64>,
    win_limit: Option<i64>,
    match_time: Option<f64>,
    score: ScoreOut,
    #[serde(skip_serializing_if = "Option::is_none")]
    captures: Option<ScoreOut>,
    timers: Vec<TimerOut>,
}

#[derive(Serialize)]
struct PlayerOut<'a> {
    id: u16,
    ent: u32,
    name: &'a str,
    steamid: &'a str,
    team: &'static str,
    class: &'static str,
    alive: bool,
    hp: u16,
    max_hp: u16,
    pos: Option<[f64; 3]>,
    yaw: Option<f64>,
    pitch: Option<f64>,
    uber: u8,
    ping: u16,
    conds: Vec<&'static str>,
    ubered: Option<&'static str>,
    medigun: Option<MedigunOut>,
    weapon: Option<WeaponOut>,
    flames: Option<FlamesOut>,
    /// Seconds until a dead player respawns, when the server has set it.
    #[serde(skip_serializing_if = "Option::is_none")]
    respawn: Option<f64>,
    /// The scoreboard line (unchanged ones repeat, which compresses better than leaving them out).
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<PlayerScoreOut>,
    /// An engineer carrying a building: its kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    carrying: Option<&'static str>,
}

#[derive(Serialize)]
struct PlayerScoreOut {
    points: i64,
    kills: i64,
    deaths: i64,
    assists: i64,
    damage: i64,
    healing: i64,
    dominations: i64,
    streak: i64,
}

#[derive(Serialize)]
struct WeaponOut {
    item: i64,
    clip: Option<i64>,
}

#[derive(Serialize)]
struct FlamesOut {
    crit: bool,
    hit: bool,
}

#[derive(Serialize)]
struct MedigunOut {
    kind: &'static str,
    releasing: bool,
    resist: Option<&'static str>,
    target: Option<u16>,
}

#[derive(Serialize)]
struct ProjectileOut {
    id: u32,
    kind: &'static str,
    yaw: f64,
    team: &'static str,
    pos: [f64; 3],
    owner: Option<u16>,
    crit: bool,
}

#[derive(Serialize)]
struct BuildingOut {
    id: u32,
    kind: &'static str,
    team: &'static str,
    level: u8,
    hp: u16,
    max_hp: u16,
    pos: [f64; 3],
    yaw: f64,
    builder: Option<u16>,
    sapped: bool,
    /// Teleporters only.
    #[serde(skip_serializing_if = "Option::is_none")]
    entrance: Option<bool>,
    /// Sentries only.
    #[serde(skip_serializing_if = "Option::is_none")]
    mini: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    building: Option<bool>,
    /// Sentries whose turret has been seen.
    #[serde(skip_serializing_if = "Option::is_none")]
    aim: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<Option<u16>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attacking: Option<bool>,
}

#[derive(Serialize)]
struct OnPoint {
    red: u8,
    blue: u8,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ObjectiveOut {
    ControlPoint {
        kind: &'static str,
        index: usize,
        pos: Option<[f64; 3]>,
        owner: &'static str,
        capping_team: &'static str,
        cap_progress: f64,
        cap_rate: f64,
        locked: bool,
        blocked: bool,
        on_point: OnPoint,
    },
    Cart {
        kind: &'static str,
        id: u32,
        pos: [f64; 3],
        team: &'static str,
        progress: f64,
        cappers: i64,
        speed: i64,
    },
    Flag {
        kind: &'static str,
        id: u32,
        pos: [f64; 3],
        team: &'static str,
        status: &'static str,
        carrier: Option<u16>,
        returns_in: Option<f64>,
    },
}

#[derive(Serialize)]
struct ScoreOut {
    red: i64,
    blue: i64,
}

#[derive(Serialize)]
struct TimerOut {
    team: &'static str,
    remaining: f64,
    paused: bool,
    setup: bool,
}

/// Removes TF2 chat colour codes: \x01-\x06 select a colour, \x07 takes RRGGBB, \x08 RRGGBBAA.
pub(crate) fn strip_colors(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\u{07}' => {
                chars.by_ref().take(6).for_each(drop);
            }
            '\u{08}' => {
                chars.by_ref().take(8).for_each(drop);
            }
            '\u{01}'..='\u{06}' => {}
            _ => out.push(c),
        }
    }
    out.trim_end_matches('\n').to_string()
}

/// The coloured spans of a chat line with TF2 colour codes, or None without any: `{"c": colour,
/// "t": text}`, where the colour is `RRGGBB`, `team` (the sender's team colour) or null (the
/// default). \x03 is the team colour, \x04 and \x05 Valve's green ones.
pub(crate) fn color_parts(text: &str) -> Option<Vec<Value>> {
    if !text.chars().any(|c| ('\u{01}'..='\u{08}').contains(&c)) {
        return None;
    }
    let mut parts: Vec<Value> = Vec::new();
    let (mut color, mut run) = (Value::Null, String::new());
    let mut chars = text.trim_end_matches('\n').chars();
    while let Some(c) = chars.next() {
        let next = match c {
            '\u{07}' | '\u{08}' => {
                let hex: String = chars.by_ref().take(if c == '\u{07}' { 6 } else { 8 }).collect();
                Some(
                    hex.get(..6)
                        .filter(|h| h.chars().all(|d| d.is_ascii_hexdigit()))
                        .map_or(Value::Null, |h| json!(h.to_ascii_uppercase())),
                )
            }
            '\u{03}' => Some(json!("team")),
            '\u{04}' => Some(json!("40FF40")),
            '\u{05}' => Some(json!("99FF99")),
            '\u{01}' | '\u{02}' | '\u{06}' => Some(Value::Null),
            _ => {
                run.push(c);
                None
            }
        };
        if let Some(next) = next {
            if !run.is_empty() {
                parts.push(json!({"c": color, "t": std::mem::take(&mut run)}));
            }
            color = next;
        }
    }
    if !run.is_empty() {
        parts.push(json!({"c": color, "t": run}));
    }
    Some(parts)
}

// tf-demo-parser's `Building::class()` reports dispensers as sentries; go by the variant.
fn building_kind_of(b: &Building) -> &'static str {
    match b {
        Building::Sentry(_) => "sentry",
        Building::Dispenser(_) => "dispenser",
        Building::Teleporter(_) => "teleporter",
        _ => "unknown",
    }
}

fn building_kind(object: u16) -> &'static str {
    match object {
        0 => "dispenser",
        1 => "teleporter",
        2 => "sentry",
        3 => "sapper",
        _ => "unknown",
    }
}

/// Per-class entity handlers. Each reads the props of one entity update.
impl Extractor {
    /// A delta names only what changed since its base frame, so an entity that came and went in
    /// between (a rocket fired and exploded since then) is in neither, and no delete for it is
    /// ever sent. SourceTV relays often send deltas from a frame a few ticks back; demo files
    /// from the previous tick, where every delete arrives. Like the game client, this frame's
    /// entities are the base frame's plus those that entered, less those that left: projectiles
    /// and buildings outside it are gone. Without the base frame (just connected), nothing is dropped.
    fn track_frame(&mut self, pe: &PacketEntitiesMessage) {
        let base = match pe.delta {
            None => Some(HashSet::new()),
            Some(t) => self.frames.iter().rev().find(|(tick, _)| *tick == u32::from(t)).map(|(_, f)| f.clone()),
        };
        let Some(mut frame) = base else { return };
        for e in &pe.entities {
            let id = u32::from(e.entity_index);
            match e.update_type {
                UpdateType::Enter | UpdateType::Preserve => frame.insert(id),
                UpdateType::Leave | UpdateType::Delete => frame.remove(&id),
            };
        }
        for id in &pe.removed_entities {
            frame.remove(&u32::from(*id));
        }
        let gone: Vec<EntityId> = self
            .gs
            .state
            .projectiles
            .keys()
            .chain(self.gs.state.buildings.keys())
            .filter(|id| !frame.contains(&u32::from(**id)))
            .copied()
            .collect();
        for id in gone {
            self.gs.state.projectile_destroy(id);
            self.gs.state.remove_building(id);
            self.forget_entity(u32::from(id));
        }
        // A later delta never comes from a frame older than this one's base.
        if let Some(t) = pe.delta {
            self.frames.retain(|(tick, _)| *tick >= u32::from(t));
        }
        self.frames.push_back((self.server_tick, frame));
        if self.frames.len() > MAX_FRAMES {
            self.frames.pop_front();
        }
    }

    fn packet_entities(&mut self, pe: &PacketEntitiesMessage, state: &ParserState) {
        if let Some(ent) = debug_entity() {
            for e in pe.entities.iter().filter(|e| u32::from(e.entity_index) == ent) {
                let class = state.server_classes.get(usize::from(e.server_class)).map_or("?", |c| c.name.as_str());
                let props: Vec<String> = e
                    .props
                    .iter()
                    .map(|p| match p.identifier.names() {
                        Some((table, name)) => format!("{table}.{name}={:?}", p.value),
                        None => format!("{:?}={:?}", p.identifier, p.value),
                    })
                    .collect();
                eprintln!(
                    "tick {} ent {ent} {:?} {class} props {} delta {:?} {}",
                    self.server_tick,
                    e.update_type,
                    e.props.len(),
                    pe.delta,
                    props.join(" ")
                );
            }
            if pe.removed_entities.iter().any(|id| u32::from(*id) == ent) {
                eprintln!("tick {} ent {ent} removed", self.server_tick);
            }
        }
        for id in &pe.removed_entities {
            self.forget_entity(u32::from(*id));
        }
        for e in &pe.entities {
            self.entity(e, state);
        }
        self.track_frame(pe);
        for p in &self.gs.state.players {
            if p.state == PlayerState::Alive {
                self.last_alive
                    .insert(u32::from(p.entity), Pose { pos: p.position, yaw: p.view_angle, pitch: p.pitch_angle });
            }
        }
        self.medic_changes();
        for (user, mut ev) in std::mem::take(&mut self.pending_spawns) {
            ev["pos"] = self.user_pos(user);
            self.push(ev);
        }
    }

    fn entity(&mut self, e: &PacketEntity, state: &ParserState) {
        let idx = u32::from(e.entity_index);
        if e.update_type == UpdateType::Delete {
            self.forget_entity(idx);
            return;
        }
        // An entity leaving SourceTV's view is not updated (or deleted) any more;
        // tf-demo-parser keeps projectiles frozen where they were.
        if e.update_type == UpdateType::Leave {
            self.gs.state.projectiles.remove(&e.entity_index);
            return;
        }
        // Every entity class has its own `moveparent` prop, so this one goes by name.
        if let Some(parent) =
            e.props.iter().find(|p| p.identifier.names().is_some_and(|(_, n)| n.as_str() == "moveparent"))
        {
            match opt_handle(as_int(&parent.value)) {
                None => self.parents.remove(&idx),
                Some(p) => self.parents.insert(idx, p),
            };
        }
        let class = state.server_classes.get(usize::from(e.server_class));
        if matches!(class.map(|c| c.name.as_str()), Some("CPhysicsProp" | "CDynamicProp" | "CPhysicsPropMultiplayer"))
            && let Some(v) = prop_vec(&e.props, PROP_ORIGIN)
        {
            self.prop_origins.insert(idx, v);
        }
        // A slot taken over by an entity of another class (an implicit delete); tf-demo-parser
        // keeps the old projectile or building, and applied this update to the old projectile:
        // a new projectile is built again from it, so it keeps its team and owner.
        if self.gs.state.projectiles.get(&e.entity_index).is_some_and(|p| p.class != e.server_class) {
            self.gs.state.projectiles.remove(&e.entity_index);
            if class.is_some_and(|c| {
                c.name.starts_with("CTFProjectile_") || c.name.as_str() == "CTFGrenadePipebombProjectile"
            }) {
                self.gs.handle_projectile_entity(e, state);
            }
        }
        let Some(class) = class else { return };
        if e.update_type == UpdateType::Enter {
            self.sentries.remove(&idx);
            let same = |b: &Building| {
                matches!(
                    (b, class.name.as_str()),
                    (Building::Sentry(_), "CObjectSentrygun")
                        | (Building::Dispenser(_), "CObjectDispenser")
                        | (Building::Teleporter(_), "CObjectTeleporter")
                )
            };
            if self.gs.state.buildings.get(&e.entity_index).is_some_and(|b| !same(b)) {
                self.gs.state.buildings.remove(&e.entity_index);
            }
        }
        if debug(class.name.as_str()) {
            for p in e.props(state) {
                eprintln!("{}#{idx} {:?} = {:?}", class.name.as_str(), p.identifier.names(), p.value);
            }
        }
        if self.is_weapon_class(usize::from(e.server_class), state) {
            // The baseline only matters on Enter; later updates are deltas.
            if e.update_type == UpdateType::Enter {
                // A reused slot may have had no Delete.
                self.weapons.remove(&idx);
                self.on_weapon(idx, e.props(state).map(|p| (p.identifier, as_int(&p.value))));
            } else {
                self.on_weapon(idx, e.props.iter().map(|p| (p.identifier, as_int(&p.value))));
            }
        }
        let props = e.props(state);
        match class.name.as_str() {
            "CTFPlayer" => self.on_player(idx, props),
            "CTFPlayerResource" => self.on_player_resource(props),
            "CTFTeam" => self.on_team(idx, props),
            "CTeamRoundTimer" => self.on_round_timer(idx, props),
            "CObjectSentrygun" => {
                self.building_kinds.insert(idx, "sentry");
                self.on_sentry(idx, props);
            }
            "CObjectDispenser" => {
                self.building_kinds.insert(idx, "dispenser");
            }
            "CObjectTeleporter" => {
                self.building_kinds.insert(idx, "teleporter");
            }
            "CTeamTrainWatcher" => self.on_train_watcher(idx, props),
            "CCaptureFlag" => self.on_flag(idx, props),
            "CTFGameRulesProxy" => self.on_game_rules(props),
            "CWeaponMedigun" => self.on_medigun(idx, props),
            "CTFFlameThrower" => self.on_flamethrower(idx, props),
            "CTFObjectiveResource" => self.on_objective_resource(props),
            _ => {}
        }
    }

    /// Any weapon class carries these props.
    fn on_weapon(&mut self, idx: u32, props: impl Iterator<Item = (SendPropIdentifier, i64)>) {
        for (id, v) in props {
            match id {
                PROP_ITEM_DEFINITION => self.weapon(idx).item = v,
                PROP_CLIP => {
                    let w = self.weapon(idx);
                    // networked as value + 1 so -1 ("no clip") fits unsigned
                    w.clip = v - 1;
                }
                _ => {}
            }
        }
    }

    fn is_weapon_class(&mut self, class: usize, state: &ParserState) -> bool {
        if self.weapon_classes.len() <= class {
            self.weapon_classes.resize(class + 1, None);
        }
        *self.weapon_classes[class].get_or_insert_with(|| {
            state
                .send_tables
                .get(class)
                .is_some_and(|t| t.flattened_props.iter().any(|d| d.identifier == PROP_ITEM_DEFINITION))
        })
    }

    fn weapon(&mut self, idx: u32) -> &mut Weapon {
        self.weapons.entry(idx).or_insert(Weapon { item: 0, clip: -1 })
    }

    fn on_player(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let words = self.conds.entry(idx).or_default();
        for p in props {
            if let Some(i) = PROP_CONDS.iter().position(|id| *id == p.identifier) {
                // A bit field: the bits are wanted, not the number.
                words[i] = as_int(&p.value) as u32;
            } else if p.identifier == PROP_ACTIVE_WEAPON {
                match opt_handle(as_int(&p.value)) {
                    Some(w) => self.active_weapons.insert(idx, w),
                    None => self.active_weapons.remove(&idx),
                };
            } else if p.identifier == PROP_CARRYING {
                self.stats.entry(idx).or_default().carrying = as_int(&p.value) != 0;
            } else if p.identifier == PROP_CARRIED {
                match opt_handle(as_int(&p.value)) {
                    Some(b) => self.carried.insert(idx, b),
                    None => self.carried.remove(&idx),
                };
            }
        }
    }

    /// The scoreboard's counted columns, like logs.tf: damage dealt to others, healing given and
    /// assists, from the events since the match started (or since connecting).
    fn count_score(&mut self, event: &GameEvent) {
        let (user, add): (u16, fn(&mut Stats, i64)) = match event {
            GameEvent::PlayerHurt(h) if h.attacker != 0 && h.attacker != h.user_id => {
                (h.attacker, |s, n| s.damage += n)
            }
            GameEvent::PlayerHealed(h) if h.healer != 0 && h.healer != h.patient => (h.healer, |s, n| s.healing += n),
            GameEvent::PlayerDeath(d)
                if d.death_flags & DEATH_FEIGN == 0 && d.assister != u16::MAX && d.assister != 0 =>
            {
                (d.assister, |s, _| s.assists += 1)
            }
            _ => return,
        };
        let amount = match event {
            GameEvent::PlayerHurt(h) => i64::from(h.damage_amount),
            GameEvent::PlayerHealed(h) => i64::from(h.amount),
            _ => 1,
        };
        if let Some(ent) = self.entity_of_user(user) {
            add(self.stats.entry(ent).or_default(), amount);
        }
    }

    /// Arrays indexed by player entity, one table per value.
    fn on_player_resource(&mut self, props: impl Iterator<Item = SendProp>) {
        let mut restarted = HashSet::new();
        for p in props {
            let Some((table, name)) = p.identifier.names() else { continue };
            let Ok(ent) = name.as_str().parse::<u32>() else { continue };
            let v = as_int(&p.value);
            let st = self.stats.entry(ent).or_default();
            // The match totals going down is the match restarting (after the warm-up, say).
            let total = |old: i64, restarted: &mut HashSet<u32>| {
                if v < old {
                    restarted.insert(ent);
                }
                v
            };
            match table.as_str() {
                "m_iTotalScore" => st.points = total(st.points, &mut restarted),
                "m_iScore" => st.kills = total(st.kills, &mut restarted),
                "m_iDeaths" => st.deaths = total(st.deaths, &mut restarted),
                "m_iActiveDominations" => st.dominations = v,
                "m_iStreaks" => st.streak = v,
                "m_flNextRespawnTime" => st.respawn_at = as_float(&p.value),
                _ => {}
            }
        }
        for ent in restarted {
            if let Some(st) = self.stats.get_mut(&ent) {
                (st.damage, st.healing, st.assists) = (0, 0, 0);
            }
        }
    }

    fn on_team(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let t = self.teams.entry(idx).or_default();
        for p in props {
            match p.identifier {
                PROP_TEAM_NUM | PROP_TEAM_TEAM_NUM => t.team = team_byte(as_int(&p.value)),
                PROP_TEAM_SCORE => t.score = as_int(&p.value),
                PROP_FLAG_CAPTURES => t.captures = as_int(&p.value),
                _ => {}
            }
        }
    }

    fn on_round_timer(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let t = self.timers.entry(idx).or_default();
        for p in props {
            let f = as_float(&p.value);
            match p.identifier {
                PROP_TEAM_NUM => t.team = team_byte(as_int(&p.value)),
                PROP_TIMER_END_TIME => t.end_time = f,
                PROP_TIMER_REMAINING => t.remaining = f,
                PROP_TIMER_PAUSED => t.paused = f != 0.0,
                PROP_TIMER_SHOW_IN_HUD => t.show = f != 0.0,
                PROP_TIMER_DISABLED => t.disabled = f != 0.0,
                PROP_TIMER_STATE => t.setup = f == 0.0,
                _ => {}
            }
        }
    }

    fn on_sentry(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let sentry = self.sentries.entry(idx).or_default();
        let mut fired = 0;
        for p in props {
            let v = as_int(&p.value);
            match p.identifier {
                PROP_SENTRY_POSE_PITCH => sentry.pose[0] = as_float(&p.value),
                PROP_SENTRY_POSE_YAW => sentry.pose[1] = as_float(&p.value),
                PROP_ROTATION => {
                    if let SendPropValue::Vector(a) = p.value {
                        sentry.yaw = a.y;
                    }
                }
                PROP_SENTRY_ENEMY => sentry.enemy = opt_handle(v),
                PROP_SENTRY_STATE => sentry.state = v,
                PROP_SENTRY_SHELLS => {
                    if let Some(before) = sentry.shells
                        && v < before
                    {
                        fired = before - v;
                    }
                    sentry.shells = Some(v);
                }
                _ => {}
            }
        }
        if fired > 0 {
            let target = sentry.enemy;
            let builder = self.gs.state.buildings.get(&EntityId::from(idx)).map(Building::builder);
            self.push(json!({
                "type": "sentry_shot",
                "sentry": idx,
                "builder": builder.and_then(|b| self.user_of_entity(builder_entity(b))),
                "target": target.and_then(|t| self.user_of_entity(t)),
                "shells": fired,
            }));
        }
    }

    fn on_train_watcher(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let w = self.train_watchers.entry(idx).or_default();
        for p in props {
            let v = as_int(&p.value);
            match p.identifier {
                PROP_TEAM_NUM => w.team = team_byte(v),
                PROP_WATCHER_PROGRESS => w.progress = as_float(&p.value),
                PROP_WATCHER_CAPPERS => w.cappers = v,
                PROP_WATCHER_SPEED => w.speed = v,
                PROP_WATCHER_GLOW => w.glow = opt_handle(v),
                _ => {}
            }
        }
    }

    fn on_flag(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let f = self.flags.entry(idx).or_default();
        for p in props {
            let v = as_int(&p.value);
            match p.identifier {
                PROP_TEAM_NUM => f.team = team_byte(v),
                PROP_ORIGIN => {
                    if let SendPropValue::Vector(o) = p.value {
                        f.pos = o;
                    }
                }
                PROP_FLAG_STATUS => f.status = v,
                PROP_FLAG_DISABLED => f.disabled = v != 0,
                PROP_OWNER_ENTITY => f.carrier = opt_handle(v),
                PROP_FLAG_RESET_TIME => f.reset_time = as_float(&p.value),
                _ => {}
            }
        }
    }

    fn on_game_rules(&mut self, props: impl Iterator<Item = SendProp>) {
        for p in props {
            match p.identifier {
                PROP_ROUND_STATE => self.round_state = RoundState::from_raw(as_int(&p.value)),
                PROP_RED_KOTH_TIMER => self.koth_timers[0] = opt_handle(as_int(&p.value)),
                PROP_BLUE_KOTH_TIMER => self.koth_timers[1] = opt_handle(as_int(&p.value)),
                PROP_MAP_RESET_TIME => self.match_start = as_float(&p.value),
                PROP_STATE_TRANSITION_TIME => self.state_transition = as_float(&p.value),
                _ => {}
            }
        }
        self.track_round_time();
    }

    fn on_medigun(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let m = self.mediguns.entry(idx).or_default();
        for p in props {
            let v = as_int(&p.value);
            match p.identifier {
                PROP_WEAPON_OWNER => m.owner = opt_handle(v),
                PROP_ITEM_DEFINITION => m.item = v,
                PROP_MEDIGUN_RELEASING => m.releasing = v != 0,
                PROP_MEDIGUN_RESIST => m.resist = v,
                PROP_MEDIGUN_TARGET => m.target = opt_handle(v),
                _ => {}
            }
        }
    }

    fn on_flamethrower(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let f = self.flamethrowers.entry(idx).or_default();
        let before = f.state;
        for p in props {
            let v = as_int(&p.value);
            match p.identifier {
                PROP_WEAPON_OWNER => f.owner = opt_handle(v),
                PROP_FLAMETHROWER_STATE => f.state = v,
                PROP_FLAMETHROWER_CRIT => f.crit = v != 0,
                PROP_FLAMETHROWER_HIT => f.hit = v != 0,
                _ => {}
            }
        }
        if f.state == FLAMETHROWER_AIRBLAST
            && before != FLAMETHROWER_AIRBLAST
            && let Some(player) = f.owner.and_then(|o| self.user_of_entity(o))
        {
            self.push(json!({"type": "airblast", "player": player}));
        }
    }

    fn on_objective_resource(&mut self, props: impl Iterator<Item = SendProp>) {
        // Capping-team changes reset progress, so they go before the cap percentages.
        let mut props: Vec<SendProp> = props.collect();
        props.sort_by_key(|p| p.identifier.names().is_none_or(|(t, _)| t.as_str() != "m_iCappingTeam"));
        for p in &props {
            self.objective_prop(p);
        }
    }

    fn temp_entities(&mut self, te: &TempEntitiesMessage, state: &ParserState) {
        // An event of the same class as the one before it in the message is
        // delta-encoded against it; any other against zeros, so a missing prop is 0.
        let mut prev: Option<(u16, Vec<SendProp>)> = None;
        for ev in &te.events {
            let class_id = u16::from(ev.class_id);
            let mut props = match prev.take() {
                Some((c, p)) if c == class_id => p,
                _ => Vec::new(),
            };
            for p in &ev.props {
                match props.iter_mut().find(|q| q.identifier == p.identifier) {
                    Some(q) => q.value = p.value.clone(),
                    None => props.push(p.clone()),
                }
            }
            if let Some(class) = state.server_classes.get(usize::from(class_id)) {
                self.temp_entity(class.name.as_str(), &props);
            }
            prev = Some((class_id, props));
        }
    }

    fn convars(&mut self, c: &SetConVarMessage) {
        let dbg = debug("convars");
        for v in &c.vars {
            if dbg {
                // Debug formatting escapes control characters from the server.
                eprintln!("convar {:?} = {:?}", v.key, v.value);
            }
            match v.key.as_str() {
                "mp_timelimit" => self.time_limit = v.value.parse().ok().filter(|m: &f32| *m > 0.0),
                "mp_winlimit" => self.win_limit = v.value.parse().ok().filter(|w: &i64| *w > 0),
                _ => {}
            }
        }
    }

    fn server_info(&mut self, info: &ServerInfoMessage) {
        if info.interval_per_tick > 0.0 {
            self.tick_interval = Some(info.interval_per_tick);
        }
        self.push(json!({
            "type": "server",
            "map": info.map,
            "hostname": info.server_name,
            "tick_interval": info.interval_per_tick,
            "max_players": info.max_player_count,
            "spawn_count": info.server_count,
        }));
    }

    fn user_message(&mut self, message: &UserMessage<'_>) {
        match message {
            UserMessage::Unknown(u) if u.raw_type == USER_MESSAGE_SAY_TEXT => {
                // SayText: console `say` and server notices. u8 client, string text.
                let mut d = u.data.clone();
                let client: u8 = d.read().unwrap_or(0);
                let text: String = d.read().unwrap_or_default();
                self.push(json!({
                    "type": "chat",
                    "player": if client == 0 { None } else { self.user_of_entity(u32::from(client)) },
                    "name": Value::Null,
                    "text": strip_colors(&text),
                    "parts": color_parts(&text),
                    "kind": "SayText",
                }));
            }
            UserMessage::SayText2(m) => {
                let player = self.user_of_entity(u32::from(m.client));
                self.push(json!({
                    "type": "chat",
                    "player": player,
                    "name": m.from.as_ref().map(ToString::to_string),
                    "text": strip_colors(m.text.as_ref()),
                    "parts": color_parts(m.text.as_ref()),
                    "kind": chat_kind(m.kind),
                }));
            }
            _ => {
                let class_of_user = |_| None;
                let user_of_entity = |ent: u32| self.user_of_entity(ent);
                let look = detail::Lookup { user_of_entity: &user_of_entity, class_of_user: &class_of_user };
                if let Some(v) = detail::user_message(message, &look) {
                    self.push(v);
                }
            }
        }
    }
}

impl MessageHandler for Extractor {
    type Output = Self;

    fn does_handle(t: MessageType) -> bool {
        GameStateAnalyser::does_handle(t)
            || matches!(
                t,
                MessageType::NetTick
                    | MessageType::SignOnState
                    | MessageType::TempEntities
                    | MessageType::UserMessage
                    | MessageType::ServerInfo
                    | MessageType::SetConVar
            )
    }

    fn handle_header(&mut self, header: &Header) {
        self.gs.handle_header(header);
    }

    fn handle_message(&mut self, message: &Message<'_>, tick: DemoTick, state: &ParserState) {
        if GameStateAnalyser::does_handle(message.get_message_type()) {
            self.gs.handle_message(message, tick, state);
        }
        match message {
            Message::PacketEntities(pe) => self.packet_entities(pe, state),
            Message::NetTick(t) => self.server_tick = u32::from(t.tick),
            Message::SetConVar(c) => self.convars(c),
            Message::ServerInfo(info) => self.server_info(info),
            Message::GameEvent(e) => {
                if debug("gameevents") {
                    eprintln!("gameevent {:?}", e.event.event_type());
                }
                self.game_event(&e.event);
            }
            Message::TempEntities(te) => self.temp_entities(te, state),
            Message::UserMessage(m) => self.user_message(m),
            _ => {}
        }
    }

    fn handle_string_entry(&mut self, table: &str, index: usize, entry: &StringTableEntry<'_>, state: &ParserState) {
        self.gs.handle_string_entry(table, index, entry, state);
    }

    fn handle_data_tables(&mut self, tables: &[ParseSendTable], classes: &[ServerClass], state: &ParserState) {
        self.class_names = classes.iter().map(|c| c.name.as_str().to_string()).collect();
        self.gs.handle_data_tables(tables, classes, state);
    }

    fn handle_packet_meta(&mut self, tick: DemoTick, meta: &MessagePacketMeta, state: &ParserState) {
        self.gs.handle_packet_meta(tick, meta, state);
    }

    fn into_output(self, _state: &ParserState) -> Self::Output {
        self
    }
}

impl BorrowMessageHandler for Extractor {
    fn borrow_output(&self, _state: &ParserState) -> &Self::Output {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_chat_colors() {
        assert_eq!(
            strip_colors("\u{1}\u{1}[STV Stats] \u{7}99CCFFBLU\u{1}'s medic DROPPED!\n"),
            "[STV Stats] BLU's medic DROPPED!"
        );
        assert_eq!(strip_colors("\u{8}FF0000FFred"), "red");
        assert_eq!(strip_colors("plain"), "plain");
    }

    #[test]
    fn chat_colour_codes_become_spans() {
        let parts = color_parts("\u{1}\u{1}[STV Stats] \u{7}99ccffBLU\u{1}'s medic \u{3}DROPPED!\n").unwrap();
        assert_eq!(
            Value::Array(parts),
            json!([{"c": null, "t": "[STV Stats] "}, {"c": "99CCFF", "t": "BLU"}, {"c": null, "t": "'s medic "}, {"c": "team", "t": "DROPPED!"}])
        );
        assert!(color_parts("plain text").is_none());
        assert_eq!(color_parts("\u{8}FF0000AAred").unwrap(), [json!({"c": "FF0000", "t": "red"})]);
    }

    #[test]
    fn entity_handles() {
        assert_eq!(handle_entity(40967), 7);
        assert_eq!(handle_entity(7), 7);
        assert_eq!(opt_handle(40967), Some(7));
        assert_eq!(opt_handle(INVALID_EHANDLE), None);
        assert_eq!(opt_handle(0), None);
    }

    #[test]
    fn prop_identifiers_name_known_props() {
        // An identifier with a typo in its table or name would silently match nothing.
        let ids = [
            PROP_TEAM_NUM,
            PROP_ORIGIN,
            PROP_ROTATION,
            PROP_TEAM_TEAM_NUM,
            PROP_TEAM_SCORE,
            PROP_TIMER_END_TIME,
            PROP_TIMER_REMAINING,
            PROP_TIMER_PAUSED,
            PROP_TIMER_SHOW_IN_HUD,
            PROP_TIMER_DISABLED,
            PROP_TIMER_STATE,
            PROP_SENTRY_POSE_PITCH,
            PROP_SENTRY_POSE_YAW,
            PROP_SENTRY_ENEMY,
            PROP_SENTRY_STATE,
            PROP_SENTRY_SHELLS,
            PROP_WATCHER_PROGRESS,
            PROP_WATCHER_CAPPERS,
            PROP_WATCHER_SPEED,
            PROP_WATCHER_GLOW,
            PROP_ROUND_STATE,
            PROP_MAP_RESET_TIME,
            PROP_STATE_TRANSITION_TIME,
            PROP_WEAPON_OWNER,
            PROP_ACTIVE_WEAPON,
            PROP_CLIP,
            PROP_ITEM_DEFINITION,
            PROP_MEDIGUN_RELEASING,
            PROP_MEDIGUN_RESIST,
            PROP_MEDIGUN_TARGET,
            PROP_FLAMETHROWER_STATE,
            PROP_FLAMETHROWER_CRIT,
            PROP_FLAMETHROWER_HIT,
            PROP_BULLETS_PLAYER,
            PROP_BULLETS_ORIGIN,
            PROP_BULLETS_PITCH,
            PROP_BULLETS_YAW,
            PROP_BULLETS_WEAPON,
            PROP_BULLETS_CRIT,
            PROP_EXPLOSION_WEAPON,
        ];
        for id in ids.iter().chain(&PROP_CONDS).chain(&PROP_EXPLOSION_ORIGIN) {
            assert!(id.names().is_some(), "{id:?}");
        }
    }

    #[test]
    fn team_numbers_out_of_range_are_no_team() {
        assert_eq!(team_num(2u8), "red");
        assert_eq!(team_num(3u16), "blue");
        assert_eq!(team_num(258u16), "none", "not truncated to red");
        assert_eq!(team_byte(-1), 0);
    }

    #[test]
    fn a_reused_entity_slot_forgets_the_medic_charge() {
        let mut x = Extractor::default();
        x.medic_charge.insert(5, Charge { charge: 100, releasing: true });
        x.forget_entity(5);
        assert!(x.medic_charge.is_empty());
    }
}

#[cfg(test)]
mod cond_tests {
    use super::*;

    fn words(conds: &[u8]) -> [u32; 6] {
        let mut w = [0u32; 6];
        for &c in conds {
            let n = c as usize;
            if n < 32 { w[0] |= 1 << n } else { w[n / 32 + 1] |= 1 << (n % 32) }
        }
        w
    }

    #[test]
    fn conditions_decode_by_bit() {
        assert_eq!(
            cond_names(&words(&[1, 4, 32, 64, 130])),
            ["Zoomed", "Stealthed", "SpeedBoost", "StealthedUserBuff", "ImmuneToPushback"]
        );
        // m_nPlayerCond and _condition_bits both carry conditions 0-31.
        let mut w = [0u32; 6];
        w[1] = 1 << 5;
        assert_eq!(cond_names(&w), ["Invulnerable"]);
    }

    #[test]
    fn uber_kinds_from_conditions() {
        assert_eq!(ubered(&words(&[5])), Some("uber"));
        assert_eq!(ubered(&words(&[5, 8])), Some("uber"));
        assert_eq!(ubered(&words(&[8])), None, "a leftover fade-out flag is not an über");
        assert_eq!(ubered(&words(&[11])), Some("kritz"));
        assert_eq!(ubered(&words(&[28])), Some("quickfix"));
        assert_eq!(ubered(&words(&[59])), Some("vaccinator_blast"));
        assert_eq!(ubered(&words(&[23, 21])), None, "overheal is not an uber");
    }

    #[test]
    fn projectile_kinds() {
        assert_eq!(projectile_kind(ProjectileType::Sticky, "CTFGrenadePipebombProjectile"), "Sticky");
        assert_eq!(projectile_kind(ProjectileType::Unknown, "CTFProjectile_Arrow"), "Arrow");
        assert_eq!(projectile_kind(ProjectileType::Unknown, "CTFProjectile_JarMilk"), "MadMilk");
        assert_eq!(projectile_kind(ProjectileType::Unknown, "CTFProjectile_SpellBats"), "Unknown");
    }

    #[test]
    fn condition_names_match_the_parser() {
        for n in 0..=PlayerCondition::MAX as u8 {
            let expected = PlayerCondition::try_from(n).map(|c| format!("{c:?}")).unwrap_or_default();
            assert_eq!(COND_NAMES[usize::from(n)], expected, "condition {n}");
        }
    }

    #[test]
    fn medigun_kinds() {
        assert_eq!(medigun_kind(29), "medigun");
        assert_eq!(medigun_kind(211), "medigun");
        assert_eq!(medigun_kind(35), "kritzkrieg");
        assert_eq!(medigun_kind(411), "quickfix");
        assert_eq!(medigun_kind(998), "vaccinator");
    }
}

#[cfg(test)]
mod koth_tests {
    use super::*;

    #[test]
    fn koth_shows_each_teams_timer_and_not_old_ones() {
        let mut x = Extractor::default();
        let shown = RoundTimer { show: true, remaining: 180.0, paused: true, ..Default::default() };
        for idx in [5, 6, 7] {
            x.timers.insert(idx, shown.clone());
        }
        x.koth_timers = [Some(6), Some(5)];
        let teams: Vec<String> = x.snapshot()["timers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["team"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(teams, ["blue", "red"]);
        x.koth_timers = [None; 2];
        assert_eq!(x.snapshot()["timers"].as_array().unwrap().len(), 3);
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;
    use tf_demo_parser::demo::data::ServerTick;
    use tf_demo_parser::demo::message::packetentities::{BaselineIndex, EntityId};
    use tf_demo_parser::demo::packet::datatable::{ClassId, ServerClassName};

    fn update(id: u32, update_type: UpdateType) -> PacketEntity {
        PacketEntity {
            server_class: ClassId::from(1u16),
            entity_index: EntityId::from(id),
            props: Vec::new(),
            in_pvs: true,
            update_type,
            serial_number: 0,
            delay: None,
            delta: None,
            baseline_index: BaselineIndex::First,
        }
    }

    fn frame(x: &mut Extractor, tick: u32, delta: Option<u32>, entities: Vec<PacketEntity>) {
        x.server_tick = tick;
        for e in &entities {
            if e.update_type == UpdateType::Enter {
                let id = e.entity_index;
                x.gs.state.projectiles.entry(id).or_insert_with(|| {
                    Projectile::new(id, ClassId::from(1u16), &ServerClassName::from("CTFProjectile_Rocket"))
                });
            }
        }
        let pe = PacketEntitiesMessage {
            entities,
            removed_entities: Vec::new(),
            max_entries: 0,
            delta: delta.map(ServerTick::from),
            base_line: BaselineIndex::First,
            updated_base_line: false,
        };
        x.track_frame(&pe);
    }

    fn shown(x: &Extractor) -> Vec<u32> {
        let mut ids: Vec<u32> = x.gs.state.projectiles.keys().map(|k| u32::from(*k)).collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn a_projectile_that_came_and_went_since_the_base_frame_is_gone() {
        let mut x = Extractor::default();
        frame(&mut x, 100, None, vec![update(10, UpdateType::Enter)]);
        frame(&mut x, 101, Some(100), vec![update(11, UpdateType::Enter)]);
        frame(&mut x, 102, Some(100), vec![update(11, UpdateType::Enter)]);
        assert_eq!(shown(&x), [10, 11]);
        // Fired after frame 100 and exploded: no longer in the deltas from 100, and never deleted.
        frame(&mut x, 103, Some(100), vec![]);
        assert_eq!(shown(&x), [10]);
    }

    #[test]
    fn deletes_and_deltas_from_the_previous_frame() {
        let mut x = Extractor::default();
        frame(&mut x, 100, None, vec![update(10, UpdateType::Enter), update(11, UpdateType::Enter)]);
        frame(&mut x, 101, Some(100), vec![update(10, UpdateType::Preserve)]);
        frame(&mut x, 102, Some(101), vec![update(11, UpdateType::Delete)]);
        assert_eq!(shown(&x), [10]);
        frame(&mut x, 103, Some(102), vec![]);
        assert_eq!(shown(&x), [10]);
    }

    #[test]
    fn without_the_base_frame_nothing_is_dropped() {
        let mut x = Extractor::default();
        frame(&mut x, 101, Some(100), vec![update(10, UpdateType::Enter)]);
        frame(&mut x, 102, Some(100), vec![]);
        assert_eq!(shown(&x), [10]);
    }
}

#[cfg(test)]
mod medic_tests {
    use super::*;
    use tf_demo_parser::demo::data::game_state::Player;
    use tf_demo_parser::demo::message::packetentities::EntityId;
    use tf_demo_parser::demo::parser::analyser::UserInfo;

    fn with_medic(charge: u8, releasing: bool) -> Extractor {
        let mut x = Extractor::default();
        let mut p = Player::default();
        p.entity = EntityId::from(5u32);
        p.class = Class::Medic;
        p.state = PlayerState::Alive;
        p.charge = charge;
        p.info = Some(UserInfo {
            classes: Default::default(),
            name: "medic".into(),
            user_id: 7u16.into(),
            steam_id: "BOT".into(),
            entity_id: EntityId::from(5u32),
            team: Team::Red,
        });
        x.gs.state.players.push(p);
        x.mediguns.insert(9, Medigun { owner: Some(5), item: 35, releasing, ..Default::default() });
        x
    }

    fn step(x: &mut Extractor, charge: u8, releasing: bool) -> Vec<String> {
        x.gs.state.players[0].charge = charge;
        x.mediguns.get_mut(&9).unwrap().releasing = releasing;
        x.medic_changes();
        x.drain()
            .iter()
            .map(|e| format!("{} {}", e["type"].as_str().unwrap(), e["medigun"].as_str().unwrap_or("?")))
            .collect()
    }

    #[test]
    fn ready_once_when_full_and_ended_when_the_charge_runs_out() {
        let mut x = with_medic(90, false);
        assert!(step(&mut x, 95, false).is_empty(), "the first sighting only records the state");
        assert_eq!(step(&mut x, 100, false), ["uber_ready kritzkrieg"]);
        assert!(step(&mut x, 100, false).is_empty());
        assert!(step(&mut x, 98, true).is_empty(), "deploying is the game's own uber event");
        assert_eq!(step(&mut x, 0, false), ["uber_ended kritzkrieg"]);
    }
}

#[cfg(test)]
mod sentry_tests {
    use super::*;

    #[test]
    fn aim_is_the_placement_turned_by_the_yaw_pose() {
        let s = |yaw, pose| Sentry { yaw, pose: [0.5, pose], ..Default::default() };
        assert_eq!(s(255.6, 0.5).aim(), 255.6);
        // Sampled while firing on a target 49 degrees left of the placement direction.
        assert!((s(255.6, 0.365).aim() - (255.6 + 48.6)).abs() < 0.1);
        assert!((s(10.0, 0.75).aim() - 280.0).abs() < 1e-3, "wraps around");
    }
}

#[cfg(test)]
mod timer_tests {
    use super::*;

    fn at(x: &mut Extractor, state: i64, transition: f32, seconds: f32) -> Option<f32> {
        x.round_state = RoundState::from_raw(state);
        x.state_transition = transition;
        x.server_tick = (seconds / DEFAULT_TICK_INTERVAL).round() as u32;
        x.track_round_time();
        x.round_time().map(|t| (t * 10.0).round() / 10.0)
    }

    #[test]
    fn time_left_counts_down_from_the_match_start() {
        let mut x = Extractor::default();
        assert_eq!(x.time_left(), None, "no mp_timelimit");
        x.time_limit = Some(30.0);
        x.match_start = 200.0;
        x.server_tick = (500.0 / DEFAULT_TICK_INTERVAL) as u32;
        assert!((x.time_left().unwrap() - 1500.0).abs() < 0.1, "{:?}", x.time_left());
        x.server_tick = (3000.0 / DEFAULT_TICK_INTERVAL) as u32;
        assert_eq!(x.time_left(), Some(0.0));
    }

    #[test]
    fn round_time_runs_while_live_stops_at_the_win_and_resets() {
        let mut x = Extractor::default();
        assert_eq!(at(&mut x, 4, 100.0, 120.0), Some(20.0));
        assert_eq!(at(&mut x, 5, 160.0, 135.0), Some(35.0), "round won at 135 s");
        assert_eq!(at(&mut x, 5, 160.0, 150.0), Some(35.0), "frozen while the winners celebrate");
        assert_eq!(at(&mut x, 3, 170.0, 165.0), None, "pre-round of the next round");
        assert_eq!(at(&mut x, 4, 170.0, 180.0), Some(10.0));
    }

    #[test]
    fn timer_counts_down_in_server_time_unless_paused() {
        let mut t = RoundTimer { end_time: 100.0, remaining: 42.0, ..Default::default() };
        let at = |t: &RoundTimer, tick| t.remaining_at(tick, DEFAULT_TICK_INTERVAL);
        assert!((at(&t, 4000) - 40.0).abs() < 1e-3, "tick 4000 is 60 s of server time");
        assert_eq!(at(&t, 9000), 0.0);
        t.paused = true;
        assert_eq!(at(&t, 4000), 42.0);
    }
}

#[cfg(test)]
mod cap_tests {
    use super::*;

    fn point(capping: u8, in_zone: u8, members: u8) -> ControlPoint {
        let mut cp = ControlPoint { capping_team: capping, team_in_zone: in_zone, ..Default::default() };
        cp.cap_time[capping as usize] = 10.0;
        cp.members[capping as usize] = members;
        cp
    }

    #[test]
    fn cap_rate_has_diminishing_returns_per_capper() {
        assert!((point(2, 2, 1).cap_rate() - 0.1).abs() < 1e-6);
        assert!((point(2, 2, 2).cap_rate() - 0.15).abs() < 1e-6);
        assert!((point(2, 2, 3).cap_rate() - 0.1833).abs() < 1e-3);
    }

    #[test]
    fn contested_holds_and_empty_decays() {
        assert_eq!(point(2, 3, 2).cap_rate(), 0.0);
        let mut blocked = point(2, 2, 2);
        blocked.blocked = true;
        assert_eq!(blocked.cap_rate(), 0.0);
        assert!((point(2, 0, 0).cap_rate() + 1.0 / 90.0).abs() < 1e-6);
    }

    #[test]
    fn progress_rolls_forward_when_the_rate_changes() {
        let mut cp = point(2, 2, 1);
        // 1 s capping alone, then a second capper joins for 1 s.
        cp.advance(0, DEFAULT_TICK_INTERVAL);
        cp.advance(67, DEFAULT_TICK_INTERVAL);
        cp.members[2] = 2;
        let p = cp.progress_at(134, DEFAULT_TICK_INTERVAL);
        assert!((p - (0.1005 + 0.15 * 1.005)).abs() < 0.01, "{p}");
        assert_eq!(point(0, 0, 0).progress_at(1000, DEFAULT_TICK_INTERVAL), 0.0);
    }
}
