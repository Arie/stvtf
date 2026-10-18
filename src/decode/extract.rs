//! Turns parser state into the JSON the overview consumes: periodic snapshots
//! plus discrete events (kills, damage, shots, explosions, ubers, rounds, chat...).

use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;
use tf_demo_parser::ParserState;
use tf_demo_parser::demo::data::DemoTick;
use tf_demo_parser::demo::data::game_state::{
    Building, BuildingClass, GameState, PlayerCondition, PlayerState, Projectile, ProjectileType,
};
use tf_demo_parser::demo::gameevent_gen::GameEvent;
use tf_demo_parser::demo::header::Header;
use tf_demo_parser::demo::message::usermessage::{ChatMessageKind, UserMessage};

use tf_demo_parser::demo::message::packetentities::UpdateType;
use tf_demo_parser::demo::message::{Message, MessageType};
use tf_demo_parser::demo::packet::datatable::{ParseSendTable, ServerClass};
use tf_demo_parser::demo::packet::message::MessagePacketMeta;
use tf_demo_parser::demo::packet::stringtable::StringTableEntry;
use tf_demo_parser::demo::parser::MessageHandler;
use tf_demo_parser::demo::parser::analyser::{Class, Team};
use tf_demo_parser::demo::parser::gamestateanalyser::GameStateAnalyser;
use tf_demo_parser::demo::parser::handler::BorrowMessageHandler;
use tf_demo_parser::demo::sendprop::{SendProp, SendPropValue};
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
/// TF_WEAPON_ROCKETLAUNCHER: its explosions are rockets, every other weapon's grenades.
const WEAPON_ROCKET_LAUNCHER: i64 = 22;
/// How far from an explosion the projectile that caused it may be.
const EXPLOSION_RADIUS: f32 = 150.0;
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

/// `STVMAP_DEBUG`, read once: a server class, temp entity class, `ent:N` or `gameevents`.
static DEBUG: LazyLock<Option<String>> = LazyLock::new(|| std::env::var("STVMAP_DEBUG").ok());

/// Whether `STVMAP_DEBUG` asks for this topic.
pub(crate) fn debug(topic: &str) -> bool {
    DEBUG.as_deref() == Some(topic)
}

/// The entity `STVMAP_DEBUG=ent:N` asks to trace.
fn debug_entity() -> Option<u32> {
    DEBUG.as_deref()?.strip_prefix("ent:")?.parse().ok()
}

#[derive(Default)]
pub struct Extractor {
    pub gs: GameStateAnalyser,
    /// Latest server tick seen in a net_Tick.
    pub server_tick: u32,
    events: RefCell<Vec<Value>>,
    world_sent: Cell<bool>,
    control_points: Vec<ControlPoint>,
    num_control_points: usize,
    /// Condition words per player entity: m_nPlayerCond, _condition_bits, m_nPlayerCondEx..Ex4.
    /// (tf-demo-parser 0.6.4's own `conditions()` decodes these wrongly.)
    conds: HashMap<u32, [u32; 6]>,
    mediguns: HashMap<u32, Medigun>,
    class_names: Vec<String>,
    /// Seconds per tick from ServerInfo.
    tick_interval: Option<f32>,
    /// Team entity -> (team number, score).
    teams: BTreeMap<u32, (u8, i64)>,
    timers: BTreeMap<u32, RoundTimer>,
    round_state: RoundState,
    /// Server time the match started (m_flMapResetTime) and the current round went live.
    match_start: f32,
    round_live_since: Option<f32>,
    /// When the live round ended (win, stalemate): its time stops there.
    round_ended_at: Option<f32>,
    state_transition: f32,
    /// Where each player entity last was while alive: a dead player's own position is
    /// their spectator camera, which follows teammates.
    last_alive: HashMap<u32, (Vector, f32, f32)>,
    /// mp_timelimit (minutes) and mp_winlimit, replicated to clients as convars.
    time_limit: Option<f32>,
    win_limit: Option<i64>,
    /// Per medic entity: charge and whether it was being released, to report changes.
    medic_charge: HashMap<u32, (u8, bool)>,
    /// Payload: tf-demo-parser takes every func_tracktrain for a cart, but only a payload
    /// map has a team_train_watcher (KotH and other maps use trains for lifts and props).
    train_watchers: BTreeMap<u32, TrainWatcher>,
    /// Entity -> the entity it is attached to (`moveparent`), to find a watcher's cart.
    parents: HashMap<u32, u32>,
    /// World positions of prop entities: a payload cart can be a free physics prop.
    prop_origins: HashMap<u32, Vector>,
    sentries: HashMap<u32, Sentry>,
    /// player_spawn arrives before the player's new position: held until the next entity update.
    pending_spawns: RefCell<Vec<(u16, Value)>>,
}

impl Sentry {
    /// World yaw of the turret: the yaw pose parameter covers a full turn, centred on the
    /// direction the sentry was placed in (checked against its targets while firing).
    fn aim(&self) -> f32 {
        (self.yaw + (0.5 - self.pose[1]) * 360.0).rem_euclid(360.0)
    }
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

#[derive(Default, Clone)]
struct Medigun {
    owner: Option<u32>,
    item: i64,
    releasing: bool,
    resist: i64,
    target: Option<u32>,
}

const COND_PROPS: [&str; 6] =
    ["m_nPlayerCond", "_condition_bits", "m_nPlayerCondEx", "m_nPlayerCondEx2", "m_nPlayerCondEx3", "m_nPlayerCondEx4"];

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
pub struct ControlPoint {
    pub pos: Option<Vector>,
    pub owner: u8,
    pub capping_team: u8,
    /// Progress 0..1 of the capping team as of `progress_tick`. The server sends it
    /// (as m_flLazyCapPerc, counting down) only now and then; in between it is advanced
    /// at `cap_rate`, rolled forward whenever an input of the rate changes.
    pub progress: f32,
    pub progress_tick: u32,
    pub locked: bool,
    pub blocked: bool,
    pub team_in_zone: u8,
    /// Per team: seconds to cap alone, and team members on the point.
    pub cap_time: [f32; 4],
    pub members: [u8; 4],
}

impl ControlPoint {
    /// Progress per second towards capping (negative while it decays back), the way
    /// TF2's client extrapolates between lazy updates: each extra capper adds 1/n.
    pub fn cap_rate(&self) -> f32 {
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

    pub fn progress_at(&self, tick: u32, interval: f32) -> f32 {
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
pub fn handle_entity(handle: i64) -> u32 {
    (handle & 0x7ff) as u32
}

/// The entity a handle prop points at; none for the invalid handle (and 0, the world).
fn opt_handle(handle: i64) -> Option<u32> {
    (handle != INVALID_EHANDLE && handle != 0).then(|| handle_entity(handle))
}

/// A team number prop; out of range values are no team.
fn team_byte(v: i64) -> u8 {
    u8::try_from(v).unwrap_or(0)
}

pub fn team_name(t: Team) -> &'static str {
    match t {
        Team::Red => "red",
        Team::Blue => "blue",
        Team::Spectator => "spectator",
        Team::Other => "none",
    }
}

fn team_num<N>(n: N) -> &'static str
where
    u8: TryFrom<N>,
{
    team_name(Team::new(n))
}

pub fn class_name(c: Class) -> &'static str {
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

fn vec3(v: Vector) -> Value {
    json!([round1(v.x), round1(v.y), round1(v.z)])
}

fn round2(f: f32) -> f64 {
    (f64::from(f) * 100.0).round() / 100.0
}

fn round1(f: f32) -> f64 {
    (f64::from(f) * 10.0).round() / 10.0
}

fn prop<'p>(props: &'p [SendProp], name: &str) -> Option<&'p SendPropValue> {
    props.iter().find(|p| p.identifier.names().is_some_and(|(_, n)| n.as_str() == name)).map(|p| &p.value)
}

fn prop_f(props: &[SendProp], name: &str) -> Option<f32> {
    match prop(props, name)? {
        SendPropValue::Float(f) => Some(*f),
        SendPropValue::Integer(i) => Some(*i as f32),
        _ => None,
    }
}

fn prop_i(props: &[SendProp], name: &str) -> Option<i64> {
    match prop(props, name)? {
        SendPropValue::Integer(i) => Some(*i),
        SendPropValue::Float(f) => Some(*f as i64),
        _ => None,
    }
}

fn prop_vec(props: &[SendProp], name: &str) -> Option<Vector> {
    match prop(props, name)? {
        SendPropValue::Vector(v) => Some(*v),
        _ => None,
    }
}

impl Extractor {
    pub fn state(&self) -> &GameState {
        &self.gs.state
    }

    /// Seconds per server tick.
    fn interval(&self) -> f32 {
        self.tick_interval.unwrap_or(DEFAULT_TICK_INTERVAL)
    }

    fn projectile_kind(&self, pr: &Projectile) -> &'static str {
        projectile_kind(pr.ty, self.class_names.get(usize::from(pr.class)).map_or("", String::as_str))
    }

    /// SourceTV keeps an exploded projectile's entity around, unchanging, for seconds
    /// before deleting it; a game client hides it at the explosion, so do the same: drop
    /// the nearest matching projectile: rockets for rocket launchers, grenades otherwise.
    fn exploded(&mut self, props: &[SendProp]) {
        let axis = |i: usize| prop_f(props, &format!("m_vecOrigin[{i}]")).unwrap_or(0.0);
        let origin = Vector { x: axis(0), y: axis(1), z: axis(2) };
        let rocket = prop_i(props, "m_iWeaponID").unwrap_or(0) == WEAPON_ROCKET_LAUNCHER;
        let nearest = self
            .gs
            .state
            .projectiles
            .values()
            .filter(|p| (p.ty == ProjectileType::Rocket) == rocket)
            .map(|p| {
                let d = p.position - origin;
                (p.id, (d.x * d.x + d.y * d.y + d.z * d.z).sqrt())
            })
            .filter(|(_, d)| *d < EXPLOSION_RADIUS)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((id, _)) = nearest {
            self.gs.state.projectiles.remove(&id);
        }
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
            let now = (p.charge, gun.is_some_and(|m| m.releasing));
            if let Some(&(charge, releasing)) = self.medic_charge.get(&ent) {
                let medigun = gun.map(|m| medigun_kind(m.item));
                if charge < 100 && now.0 >= 100 && !now.1 {
                    events.push(json!({"type": "uber_ready", "medic": u16::from(info.user_id), "medigun": medigun}));
                }
                if releasing && !now.1 {
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
        self.teams.remove(&idx);
        self.timers.remove(&idx);
        self.train_watchers.remove(&idx);
        self.parents.remove(&idx);
        self.prop_origins.remove(&idx);
        self.sentries.remove(&idx);
        self.medic_charge.remove(&idx);
    }

    fn entity_of_user(&self, user_id: u16) -> Option<u32> {
        self.state()
            .players
            .iter()
            .find(|p| p.info.as_ref().is_some_and(|i| u16::from(i.user_id) == user_id))
            .map(|p| u32::from(p.entity))
    }

    fn medigun_of(&self, player_ent: u32) -> Option<Value> {
        let m = self.mediguns.values().find(|m| m.owner == Some(player_ent))?;
        let kind = medigun_kind(m.item);
        Some(json!({
            "kind": kind,
            "releasing": m.releasing,
            "resist": (kind == "vaccinator").then(|| resist_name(m.resist)),
            "target": m.target.and_then(|t| self.user_of_entity(t)),
        }))
    }

    pub fn push(&self, mut v: Value) {
        if let Some(obj) = v.as_object_mut() {
            obj.insert("tick".into(), json!(self.server_tick));
        }
        self.events.borrow_mut().push(v);
    }

    pub fn drain(&self) -> Vec<Value> {
        std::mem::take(&mut self.events.borrow_mut())
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
        let int = match &p.value {
            SendPropValue::Integer(i) => *i,
            SendPropValue::Float(f) => *f as i64,
            _ => 0,
        };
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

    fn game_event(&self, event: &GameEvent) {
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
                let mut pending = self.pending_spawns.borrow_mut();
                if pending.len() < MAX_PENDING_SPAWNS {
                    pending.push((
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
            GameEvent::PlayerDisconnect(d) => self.push(json!({
                "type": "player_leave",
                "player": d.user_id,
                "name": d.name.to_string(),
                "reason": d.reason.to_string(),
            })),
            GameEvent::PlayerBuiltObject(b) => self.push(json!({
                "type": "build",
                "player": b.user_id,
                "object": building_kind(b.object),
                "id": b.index,
            })),
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
            _ => {}
        }
    }

    fn temp_entity(&self, class: &str, props: &[SendProp]) {
        if debug(class) {
            for p in props {
                eprintln!("{class} {:?} = {:?}", p.identifier.names(), p.value);
            }
        }
        match class {
            "CTEFireBullets" => {
                // m_iPlayer is the player's entity index minus one.
                let player = prop_i(props, "m_iPlayer")
                    .and_then(|i| u32::try_from(i).ok()?.checked_add(1))
                    .and_then(|ent| self.user_of_entity(ent));
                self.push(json!({
                    "type": "shot",
                    "player": player,
                    "origin": vec3(prop_vec(props, "m_vecOrigin").unwrap_or_default()),
                    "pitch": round1(prop_f(props, "m_vecAngles[0]").unwrap_or(0.0)),
                    "yaw": round1(prop_f(props, "m_vecAngles[1]").unwrap_or(0.0)),
                    "weapon_id": prop_i(props, "m_iWeaponID").unwrap_or(0),
                    "crit": prop_i(props, "m_bCritical").unwrap_or(0) != 0,
                }));
            }
            "CTETFExplosion" => {
                let axis = |i: usize| round1(prop_f(props, &format!("m_vecOrigin[{i}]")).unwrap_or(0.0));
                self.push(json!({
                    "type": "explosion",
                    "origin": [axis(0), axis(1), axis(2)],
                    "weapon_id": prop_i(props, "m_iWeaponID").unwrap_or(0),
                }));
            }
            _ => {}
        }
    }

    /// Emits a `world` event once the world bounds are known.
    pub fn announce(&self) {
        if !self.world_sent.get()
            && let Some(w) = &self.state().world
        {
            let v = json!({"type": "world", "min": vec3(w.boundary_min), "max": vec3(w.boundary_max)});
            self.world_sent.set(true);
            self.push(v);
        }
    }

    pub fn snapshot(&self) -> Value {
        let gs = self.state();
        let players: Vec<Value> = gs
            .players
            .iter()
            .filter_map(|p| {
                let info = p.info.as_ref()?;
                // The SourceTV bot itself is a BOT on no playing team.
                if info.steam_id == "BOT" && !matches!(p.team, Team::Red | Team::Blue) && p.class == Class::Other {
                    return None;
                }
                let alive = p.state == PlayerState::Alive;
                // Dead: where they died, or nothing if that happened before we connected.
                let last = if alive {
                    Some((p.position, p.view_angle, p.pitch_angle))
                } else {
                    self.last_alive.get(&u32::from(p.entity)).copied()
                };
                Some(json!({
                    "id": u16::from(info.user_id),
                    "ent": u32::from(p.entity),
                    "name": info.name,
                    "steamid": info.steam_id,
                    "team": team_name(p.team),
                    "class": class_name(p.class),
                    "alive": alive,
                    "hp": p.health,
                    "max_hp": p.max_health,
                    "pos": last.map(|l| vec3(l.0)),
                    "yaw": last.map(|l| round1(l.1)),
                    "pitch": last.map(|l| round1(l.2)),
                    "uber": p.charge,
                    "ping": p.ping,
                    "conds": self.conds.get(&u32::from(p.entity)).map(cond_names).unwrap_or_default(),
                    "ubered": self.conds.get(&u32::from(p.entity)).and_then(ubered),
                    "medigun": (p.class == Class::Medic).then(|| self.medigun_of(u32::from(p.entity))).flatten(),
                }))
            })
            .collect();
        let projectiles: Vec<Value> = gs
            .projectiles
            .values()
            .map(|pr| {
                let owner = gs
                    .players
                    .iter()
                    .find(|p| pr.launcher.0 != 0 && p.weapons.contains(&pr.launcher))
                    .and_then(|p| p.info.as_ref())
                    .map(|i| u16::from(i.user_id));
                json!({
                    "id": u32::from(pr.id),
                    "kind": self.projectile_kind(pr),
                    "yaw": round1(pr.rotation.y),
                    "team": team_name(pr.team),
                    "pos": vec3(pr.position),
                    "owner": owner,
                    "crit": pr.critical,
                })
            })
            .collect();
        let buildings: Vec<Value> = gs
            .buildings
            .values()
            .map(|b| {
                let mut v = json!({
                    "id": u32::from(b.entity_id()),
                    "kind": building_class(&b.class()),
                    "team": team_name(b.team()),
                    "level": b.level(),
                    "hp": b.health(),
                    "max_hp": b.max_health(),
                    "pos": vec3(b.position()),
                    "yaw": round1(b.angle()),
                    "builder": self.user_of_entity(handle_entity(i64::from(u16::from(b.builder())))),
                    "sapped": b.sapped(),
                });
                if let Building::Teleporter(t) = b {
                    v["entrance"] = json!(t.is_entrance);
                }
                if let Building::Sentry(s) = b {
                    v["mini"] = json!(s.is_mini);
                    v["building"] = json!(s.building);
                    if let Some(t) = self.sentries.get(&u32::from(b.entity_id())) {
                        v["yaw"] = json!(round1(t.yaw));
                        v["aim"] = json!(round1(t.aim()));
                        v["target"] = json!(t.enemy.and_then(|e| self.user_of_entity(e)));
                        v["attacking"] = json!(t.state == SENTRY_STATE_ATTACKING);
                    }
                }
                v
            })
            .collect();
        let mut objectives: Vec<Value> = self
            .control_points
            .iter()
            .take(self.num_control_points)
            .enumerate()
            .map(|(i, cp)| {
                json!({
                    "kind": "control_point",
                    "index": i,
                    "pos": cp.pos.map(vec3),
                    "owner": team_num(cp.owner),
                    "capping_team": team_num(cp.capping_team),
                    // m_flLazyCapPerc counts down from 1.0 while a team caps; it is stale otherwise.
                    "cap_progress": round2(cp.progress_at(self.server_tick, self.interval())),
                    "cap_rate": (f64::from(cp.cap_rate()) * 1000.0).round() / 1000.0,
                    "locked": cp.locked,
                    "blocked": cp.blocked,
                })
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
            objectives.push(json!({
                "kind": "cart",
                "id": id,
                "pos": vec3(pos),
                "team": team_num(w.team),
                "progress": round2(w.progress),
                "cappers": w.cappers,
                "speed": w.speed,
            }));
        }
        json!({
            "type": "snapshot",
            "tick": self.server_tick,
            "players": players,
            "projectiles": projectiles,
            "buildings": buildings,
            "objectives": objectives,
            "round": self.round_state.name(),
            // Seconds since the round went live and since the match started (null if unknown).
            "round_time": self.round_time().map(round1),
            "time_left": self.time_left().map(round1),
            "win_limit": self.win_limit,
            "match_time": (self.match_start > 0.0).then(|| round1((self.server_tick as f32 * self.interval() - self.match_start).max(0.0))),
            "score": {
                "red": self.teams.values().find(|t| t.0 == 2).map_or(0, |t| t.1),
                "blue": self.teams.values().find(|t| t.0 == 3).map_or(0, |t| t.1),
            },
            "timers": self
                .timers
                .values()
                .filter(|t| t.show && !t.disabled)
                .map(|t| json!({
                    "team": team_num(t.team),
                    "remaining": round1(t.remaining_at(self.server_tick, self.interval())),
                    "paused": t.paused,
                    "setup": t.setup,
                }))
                .collect::<Vec<_>>(),
        })
    }
}

/// Removes TF2 chat colour codes: \x01-\x06 select a colour, \x07 takes RRGGBB, \x08 RRGGBBAA.
pub fn strip_colors(text: &str) -> String {
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

fn building_class(c: &BuildingClass) -> &'static str {
    match c {
        BuildingClass::Sentry => "sentry",
        BuildingClass::Dispenser => "dispenser",
        BuildingClass::Teleporter => "teleporter",
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
        if let Message::PacketEntities(pe) = message {
            if let Some(ent) = debug_entity() {
                for e in pe.entities.iter().filter(|e| u32::from(e.entity_index) == ent) {
                    let class = state.server_classes.get(usize::from(e.server_class)).map_or("?", |c| c.name.as_str());
                    eprintln!(
                        "tick {} ent {ent} {:?} {class} props {} delta {:?}",
                        self.server_tick,
                        e.update_type,
                        e.props.len(),
                        pe.delta
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
                let idx = u32::from(e.entity_index);
                if e.update_type == UpdateType::Delete {
                    self.forget_entity(idx);
                    continue;
                }
                // An entity leaving SourceTV's view is not updated (or deleted) any more;
                // tf-demo-parser keeps projectiles frozen where they were.
                if e.update_type == UpdateType::Leave {
                    self.gs.state.projectiles.remove(&e.entity_index);
                    continue;
                }
                if let Some(parent) =
                    e.props.iter().find(|p| p.identifier.names().is_some_and(|(_, n)| n.as_str() == "moveparent"))
                {
                    match opt_handle(i64::try_from(&parent.value).unwrap_or(INVALID_EHANDLE)) {
                        None => self.parents.remove(&idx),
                        Some(p) => self.parents.insert(idx, p),
                    };
                }
                if matches!(
                    state.server_classes.get(usize::from(e.server_class)).map(|c| c.name.as_str()),
                    Some("CPhysicsProp" | "CDynamicProp" | "CPhysicsPropMultiplayer")
                ) && let Some(SendPropValue::Vector(v)) = prop(&e.props, "m_vecOrigin")
                {
                    self.prop_origins.insert(idx, *v);
                }
                // A slot taken over by an entity of another class (an implicit delete).
                if self.gs.state.projectiles.get(&e.entity_index).is_some_and(|p| p.class != e.server_class) {
                    self.gs.state.projectiles.remove(&e.entity_index);
                }
                let Some(class) = state.server_classes.get(usize::from(e.server_class)) else { continue };
                if debug(class.name.as_str()) {
                    for p in e.props(state) {
                        eprintln!(
                            "{}#{} {:?} = {:?}",
                            class.name.as_str(),
                            u32::from(e.entity_index),
                            p.identifier.names(),
                            p.value
                        );
                    }
                }
                match class.name.as_str() {
                    "CTFPlayer" => {
                        let words = self.conds.entry(idx).or_default();
                        for p in e.props(state) {
                            let Some((_, name)) = p.identifier.names() else { continue };
                            if let Some(i) = COND_PROPS.iter().position(|n| *n == name.as_str()) {
                                words[i] = i64::try_from(&p.value).unwrap_or_default() as u32;
                            }
                        }
                    }
                    "CTFTeam" => {
                        let t = self.teams.entry(idx).or_default();
                        for p in e.props(state) {
                            let Some((_, name)) = p.identifier.names() else { continue };
                            let v = i64::try_from(&p.value).unwrap_or_default();
                            match name.as_str() {
                                "m_iTeamNum" => t.0 = team_byte(v),
                                "m_iScore" => t.1 = v,
                                _ => {}
                            }
                        }
                    }
                    "CTeamRoundTimer" => {
                        let t = self.timers.entry(idx).or_default();
                        for p in e.props(state) {
                            let Some((_, name)) = p.identifier.names() else { continue };
                            let f = f32::try_from(&p.value)
                                .unwrap_or_else(|_| i64::try_from(&p.value).unwrap_or_default() as f32);
                            match name.as_str() {
                                "m_iTeamNum" => t.team = team_byte(f as i64),
                                "m_flTimerEndTime" => t.end_time = f,
                                "m_flTimeRemaining" => t.remaining = f,
                                "m_bTimerPaused" => t.paused = f != 0.0,
                                "m_bShowInHUD" => t.show = f != 0.0,
                                "m_bIsDisabled" => t.disabled = f != 0.0,
                                "m_nState" => t.setup = f == 0.0,
                                _ => {}
                            }
                        }
                    }
                    "CObjectSentrygun" => {
                        let sentry = self.sentries.entry(idx).or_default();
                        let mut fired = 0;
                        for p in e.props(state) {
                            let Some((table, name)) = p.identifier.names() else { continue };
                            let v = i64::try_from(&p.value).unwrap_or_default();
                            match (table.as_str(), name.as_str()) {
                                ("m_flPoseParameter", "000") => {
                                    sentry.pose[0] = f32::try_from(&p.value).unwrap_or_default();
                                }
                                ("m_flPoseParameter", "001") => {
                                    sentry.pose[1] = f32::try_from(&p.value).unwrap_or_default();
                                }
                                (_, "m_angRotation") => {
                                    if let SendPropValue::Vector(a) = p.value {
                                        sentry.yaw = a.y;
                                    }
                                }
                                (_, "m_hEnemy") => sentry.enemy = opt_handle(v),
                                (_, "m_iState") => sentry.state = v,
                                (_, "m_iAmmoShells") => {
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
                            let (target, builder) =
                                (sentry.enemy, self.gs.state.buildings.get(&e.entity_index).map(Building::builder));
                            self.push(json!({
                                "type": "sentry_shot",
                                "sentry": idx,
                                "builder": builder.and_then(|b| self.user_of_entity(handle_entity(i64::from(u16::from(b))))),
                                "target": target.and_then(|t| self.user_of_entity(t)),
                                "shells": fired,
                            }));
                        }
                    }
                    "CTeamTrainWatcher" => {
                        let w = self.train_watchers.entry(idx).or_default();
                        for p in e.props(state) {
                            let Some((_, name)) = p.identifier.names() else { continue };
                            let v = i64::try_from(&p.value).unwrap_or_default();
                            match name.as_str() {
                                "m_iTeamNum" => w.team = team_byte(v),
                                "m_flTotalProgress" => w.progress = f32::try_from(&p.value).unwrap_or_default(),
                                "m_nNumCappers" => w.cappers = v,
                                "m_iTrainSpeedLevel" => w.speed = v,
                                "m_hGlowEnt" => w.glow = opt_handle(v),
                                _ => {}
                            }
                        }
                    }
                    "CTFGameRulesProxy" => {
                        for p in e.props(state) {
                            let Some((_, name)) = p.identifier.names() else { continue };
                            match name.as_str() {
                                "m_iRoundState" => {
                                    self.round_state =
                                        RoundState::from_raw(i64::try_from(&p.value).unwrap_or_default());
                                }
                                "m_flMapResetTime" => self.match_start = f32::try_from(&p.value).unwrap_or_default(),
                                "m_flStateTransitionTime" => {
                                    self.state_transition = f32::try_from(&p.value).unwrap_or_default();
                                }
                                _ => {}
                            }
                        }
                        self.track_round_time();
                    }
                    "CWeaponMedigun" => {
                        let m = self.mediguns.entry(idx).or_default();
                        for p in e.props(state) {
                            let Some((_, name)) = p.identifier.names() else { continue };
                            let v = i64::try_from(&p.value).unwrap_or_default();
                            match name.as_str() {
                                "m_hOwner" => m.owner = opt_handle(v),
                                "m_iItemDefinitionIndex" => m.item = v,
                                "m_bChargeRelease" => m.releasing = v != 0,
                                "m_nChargeResistType" => m.resist = v,
                                "m_hHealingTarget" => m.target = opt_handle(v),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                if class.name.as_str() == "CTFObjectiveResource" {
                    // Capping-team changes reset progress, so they go before the cap percentages.
                    let mut props: Vec<SendProp> = e.props(state).collect();
                    props.sort_by_key(|p| p.identifier.names().is_none_or(|(t, _)| t.as_str() != "m_iCappingTeam"));
                    for p in &props {
                        self.objective_prop(p);
                    }
                }
            }
        }
        if let Message::PacketEntities(_) = message {
            for p in &self.gs.state.players {
                if p.state == PlayerState::Alive {
                    self.last_alive.insert(u32::from(p.entity), (p.position, p.view_angle, p.pitch_angle));
                }
            }
            self.medic_changes();
            for (user, mut ev) in std::mem::take(&mut *self.pending_spawns.borrow_mut()) {
                let tick = ev["tick"].clone();
                ev["pos"] = self.user_pos(user);
                self.push(ev);
                if let Some(last) = self.events.borrow_mut().last_mut() {
                    last["tick"] = tick;
                }
            }
        }
        match message {
            Message::NetTick(t) => self.server_tick = u32::from(t.tick),
            Message::SetConVar(c) => {
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
            Message::ServerInfo(info) => {
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
            Message::GameEvent(e) => {
                if debug("gameevents") {
                    eprintln!("gameevent {:?}", e.event.event_type());
                }
                self.game_event(&e.event);
            }
            Message::TempEntities(te) => {
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
                        if class.name.as_str() == "CTETFExplosion" {
                            self.exploded(&props);
                        }
                    }
                    prev = Some((class_id, props));
                }
            }
            Message::UserMessage(UserMessage::Unknown(u)) if u.raw_type == USER_MESSAGE_SAY_TEXT => {
                // SayText: console `say` and server notices. u8 client, string text.
                let mut d = u.data.clone();
                let client: u8 = d.read().unwrap_or(0);
                let text: String = d.read().unwrap_or_default();
                self.push(json!({
                    "type": "chat",
                    "player": if client == 0 { None } else { self.user_of_entity(u32::from(client)) },
                    "name": Value::Null,
                    "text": strip_colors(&text),
                    "kind": "SayText",
                }));
            }
            Message::UserMessage(UserMessage::SayText2(m)) => {
                let player = self.user_of_entity(u32::from(m.client));
                self.push(json!({
                    "type": "chat",
                    "player": player,
                    "name": m.from.as_ref().map(ToString::to_string),
                    "text": strip_colors(m.text.as_ref()),
                    "kind": chat_kind(m.kind),
                }));
            }
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
    fn entity_handles() {
        assert_eq!(handle_entity(40967), 7);
        assert_eq!(handle_entity(7), 7);
        assert_eq!(opt_handle(40967), Some(7));
        assert_eq!(opt_handle(INVALID_EHANDLE), None);
        assert_eq!(opt_handle(0), None);
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
        x.medic_charge.insert(5, (100, true));
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
mod explosion_tests {
    use super::*;
    use tf_demo_parser::demo::message::packetentities::EntityId;
    use tf_demo_parser::demo::packet::datatable::{ClassId, ServerClassName};
    use tf_demo_parser::demo::sendprop::SendPropIdentifier;

    fn projectile(id: u32, class: &str, ty: ProjectileType, pos: [f32; 3]) -> Projectile {
        let mut p = Projectile::new(EntityId::from(id), ClassId::from(1u16), &ServerClassName::from(class));
        p.ty = ty;
        p.position = Vector { x: pos[0], y: pos[1], z: pos[2] };
        p
    }

    fn explosion(weapon: i64, at: [f32; 3]) -> Vec<SendProp> {
        let prop = |name: &str, value| SendProp {
            index: 0,
            identifier: SendPropIdentifier::new("DT_TETFExplosion", name),
            value,
        };
        vec![
            prop("m_vecOrigin[0]", SendPropValue::Float(at[0])),
            prop("m_vecOrigin[1]", SendPropValue::Float(at[1])),
            prop("m_vecOrigin[2]", SendPropValue::Float(at[2])),
            prop("m_iWeaponID", SendPropValue::Integer(weapon)),
        ]
    }

    fn extractor() -> Extractor {
        let mut x = Extractor::default();
        for p in [
            projectile(10, "CTFProjectile_Rocket", ProjectileType::Rocket, [100.0, 0.0, 0.0]),
            projectile(11, "CTFGrenadePipebombProjectile", ProjectileType::Sticky, [40.0, 0.0, 0.0]),
            projectile(12, "CTFProjectile_Rocket", ProjectileType::Rocket, [900.0, 0.0, 0.0]),
        ] {
            x.gs.state.projectiles.insert(p.id, p);
        }
        x
    }

    fn left(x: &Extractor) -> Vec<u32> {
        x.gs.state.projectiles.keys().map(|k| u32::from(*k)).collect()
    }

    #[test]
    fn rocket_explosion_removes_the_rocket_not_the_closer_sticky() {
        assert!(explosion(22, [0.0; 3]).iter().all(|p| p.identifier.names().is_some()), "prop names resolve");
        let mut x = extractor();
        x.exploded(&explosion(22, [30.0, 0.0, 0.0]));
        assert_eq!(left(&x), [11, 12]);
    }

    #[test]
    fn grenade_explosion_removes_the_sticky() {
        let mut x = extractor();
        x.exploded(&explosion(35, [45.0, 0.0, 0.0]));
        assert_eq!(left(&x), [10, 12]);
    }

    #[test]
    fn far_explosions_remove_nothing() {
        let mut x = extractor();
        x.exploded(&explosion(22, [500.0, 0.0, 0.0]));
        assert_eq!(left(&x), [10, 11, 12]);
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
