//! The ids, teams, classes and other kinds the lines name.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};
use std::fmt;

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[expect(missing_docs, reason = "self-explanatory")]
pub enum FlagStatus {
    #[default]
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
