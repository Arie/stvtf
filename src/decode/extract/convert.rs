//! Conversions from parser values to output values.

use super::{COND_BITS, COND_WORDS, RESIST_BLAST, RESIST_BULLET};
use crate::decode::props::handle_entity;
use crate::output::{
    self as out, BuildingKind, ChatColor, ChatKind, ChatPart, Condition, MedigunKind, PlayerId, Position,
    ProjectileKind, Resist, Round, Ubered, round1,
};
use tf_demo_parser::demo::data::game_state::{
    Building, Player, PlayerClassData, PlayerCondition, PlayerState, ProjectileType,
};
use tf_demo_parser::demo::message::usermessage::ChatMessageKind;
use tf_demo_parser::demo::parser::analyser::{Class, Team, UserId};
use tf_demo_parser::demo::vector::Vector;

/// Condition numbers: 0-31 and the four `Ex` words.
const COND_NUMBERS: u8 = (COND_BITS * (COND_WORDS - 1)) as u8;

/// TF2 chat colour codes: the default colour, the sender's team colour, Valve's two greens, then
/// a colour given as `RRGGBB` or as `RRGGBBAA` (alpha ignored).
const CHAT_DEFAULT: [char; 3] = ['\u{01}', '\u{02}', '\u{06}'];
const CHAT_TEAM: char = '\u{03}';
const CHAT_GREEN: char = '\u{04}';
const CHAT_LIGHT_GREEN: char = '\u{05}';
const CHAT_RGB: char = '\u{07}';
const CHAT_RGBA: char = '\u{08}';
const CHAT_CODES: std::ops::RangeInclusive<char> = '\u{01}'..='\u{08}';
const RGB_DIGITS: usize = 6;
const RGBA_DIGITS: usize = 8;
/// What the client draws `CHAT_GREEN` and `CHAT_LIGHT_GREEN` in.
const GREEN: [u8; 3] = [0x40, 0xFF, 0x40];
const LIGHT_GREEN: [u8; 3] = [0x99, 0xFF, 0x99];

/// `m_nFlagStatus`: at home, carried, dropped.
const FLAG_CARRIED: i64 = 1;
const FLAG_DROPPED: i64 = 2;

/// `m_iItemDefinitionIndex` of the mediguns that are not the stock one (or a reskin of it).
const ITEM_KRITZKRIEG: i64 = 35;
const ITEM_QUICKFIX: i64 = 411;
const ITEM_VACCINATOR: i64 = 998;

/// A medic's charge percentage; 0 for anyone else.
pub(super) fn uber_charge(p: &Player) -> u8 {
    match p.class_data {
        PlayerClassData::Medic { charge, .. } => charge,
        _ => 0,
    }
}

/// A player's user id, once their info is known.
pub(super) fn player_id(p: &Player) -> Option<PlayerId> {
    p.info.as_ref().map(|i| PlayerId(u16::from(i.user_id)))
}

/// `CTFGameRulesProxy` `m_iRoundState` (`gamerules_roundstate_t`).
pub(super) fn round_from_raw(v: i64) -> Round {
    match v {
        0 => Round::Init,
        1 => Round::Pregame,
        2 => Round::StartGame,
        3 => Round::Preround,
        4 => Round::Running,
        5 => Round::TeamWin,
        6 => Round::Restart,
        7 => Round::Stalemate,
        8 => Round::GameOver,
        9 => Round::Bonus,
        _ => Round::BetweenRounds,
    }
}

/// tf-demo-parser names the common projectiles; the rest go by their server class.
pub(super) fn projectile_kind(ty: ProjectileType, class: &str) -> ProjectileKind {
    match ty {
        ProjectileType::Rocket => return ProjectileKind::Rocket,
        ProjectileType::HealingArrow => return ProjectileKind::HealingArrow,
        ProjectileType::Sticky => return ProjectileKind::Sticky,
        ProjectileType::Pipe => return ProjectileKind::Pipe,
        ProjectileType::Flare => return ProjectileKind::Flare,
        ProjectileType::LooseCannon => return ProjectileKind::LooseCannon,
        ProjectileType::Unknown => {}
    }
    match class {
        "CTFProjectile_Arrow" => ProjectileKind::Arrow,
        "CTFProjectile_EnergyBall" => ProjectileKind::EnergyBall,
        "CTFProjectile_EnergyRing" => ProjectileKind::EnergyRing,
        "CTFProjectile_BallOfFire" => ProjectileKind::Fireball,
        "CTFProjectile_Cleaver" => ProjectileKind::Cleaver,
        "CTFProjectile_Jar" => ProjectileKind::Jarate,
        "CTFProjectile_JarMilk" => ProjectileKind::MadMilk,
        "CTFProjectile_JarGas" => ProjectileKind::GasPasser,
        "CTFProjectile_MechanicalArmOrb" => ProjectileKind::ShortCircuitOrb,
        _ => ProjectileKind::Unknown,
    }
}

pub(super) fn medigun_kind(item: i64) -> MedigunKind {
    match item {
        ITEM_KRITZKRIEG => MedigunKind::Kritzkrieg,
        ITEM_QUICKFIX => MedigunKind::Quickfix,
        ITEM_VACCINATOR => MedigunKind::Vaccinator,
        _ => MedigunKind::Medigun,
    }
}

pub(super) fn resist(r: i64) -> Resist {
    match r {
        RESIST_BULLET => Resist::Bullet,
        RESIST_BLAST => Resist::Blast,
        _ => Resist::Fire,
    }
}

pub(super) fn flag_status(v: i64) -> out::FlagStatus {
    match v {
        FLAG_CARRIED => out::FlagStatus::Carried,
        FLAG_DROPPED => out::FlagStatus::Dropped,
        _ => out::FlagStatus::Home,
    }
}

/// Whether condition number `n` is set.
fn has_cond_bit(words: &[u32; COND_WORDS], n: usize) -> bool {
    let word = if n < COND_BITS { words[0] | words[1] } else { words.get(n / COND_BITS + 1).copied().unwrap_or(0) };
    word >> (n % COND_BITS) & 1 == 1
}

fn has_cond(words: &[u32; COND_WORDS], c: PlayerCondition) -> bool {
    has_cond_bit(words, c as usize)
}

/// The conditions set in a player's condition words (160 numbers: 0-31 twice, then four more
/// words), in number order.
pub(super) fn conditions(words: &[u32; COND_WORDS]) -> Vec<Condition> {
    (0..COND_NUMBERS).filter(|&n| has_cond_bit(words, usize::from(n))).filter_map(Condition::from_number).collect()
}

pub(super) fn chat_kind(kind: ChatMessageKind) -> ChatKind {
    match kind {
        ChatMessageKind::ChatAll => ChatKind::ChatAll,
        ChatMessageKind::ChatTeam => ChatKind::ChatTeam,
        ChatMessageKind::ChatAllDead => ChatKind::ChatAllDead,
        ChatMessageKind::ChatTeamDead => ChatKind::ChatTeamDead,
        ChatMessageKind::ChatAllSpec => ChatKind::ChatAllSpec,
        ChatMessageKind::NameChange => ChatKind::NameChange,
        ChatMessageKind::Empty => ChatKind::Empty,
    }
}

/// The medigun charge effect a player is under, from their conditions.
pub(super) fn ubered(words: &[u32; COND_WORDS]) -> Option<Ubered> {
    use PlayerCondition::{
        CritBoosted, Invulnerable, MedigunUberBlastResist, MedigunUberBulletResist, MedigunUberFireResist, MegaHeal,
    };
    let on = |c| has_cond(words, c);
    // InvulnerableWearingOff only marks the fade-out next to Invulnerable; servers have been
    // seen leaving it set on a medic for good, so it is not an über on its own.
    if on(Invulnerable) {
        Some(Ubered::Uber)
    } else if on(MegaHeal) {
        Some(Ubered::Quickfix)
    } else if on(MedigunUberBulletResist) {
        Some(Ubered::VaccinatorBullet)
    } else if on(MedigunUberBlastResist) {
        Some(Ubered::VaccinatorBlast)
    } else if on(MedigunUberFireResist) {
        Some(Ubered::VaccinatorFire)
    } else if on(CritBoosted) {
        Some(Ubered::Kritz)
    } else {
        None
    }
}

/// Alive and spawned: a player who joined a team but has not picked a class yet is alive to the
/// game, with no health, at the map's origin.
pub(super) fn alive(p: &Player) -> bool {
    p.state == PlayerState::Alive && p.class != Class::Other
}

/// The entity of a building's builder. tf-demo-parser stores the builder's entity
/// handle (`m_hBuilder`) in a `UserId`, so despite its type it is not a user id.
pub(super) fn builder_entity(builder: UserId) -> u32 {
    handle_entity(i64::from(u16::from(builder)))
}

/// A user id field of an event; 0 is nobody.
pub(crate) fn opt_user(id: u16) -> Option<PlayerId> {
    (id != 0).then_some(PlayerId(id))
}

pub(super) fn team(t: Team) -> out::Team {
    match t {
        Team::Red => out::Team::Red,
        Team::Blue => out::Team::Blue,
        Team::Spectator => out::Team::Spectator,
        Team::Other => out::Team::Unassigned,
    }
}

pub(crate) fn team_num<N>(n: N) -> out::Team
where
    u8: TryFrom<N>,
{
    team(Team::new(n))
}

pub(super) fn class(c: Class) -> out::Class {
    match c {
        Class::Scout => out::Class::Scout,
        Class::Sniper => out::Class::Sniper,
        Class::Soldier => out::Class::Soldier,
        Class::Demoman => out::Class::Demoman,
        Class::Medic => out::Class::Medic,
        Class::Heavy => out::Class::Heavy,
        Class::Pyro => out::Class::Pyro,
        Class::Spy => out::Class::Spy,
        Class::Engineer => out::Class::Engineer,
        Class::Other => out::Class::Unknown,
    }
}

pub(super) fn class_num(n: u16) -> out::Class {
    class(Class::new(i64::from(n)))
}

/// A position as written out, to a tenth of a unit.
pub(crate) fn xyz(v: Vector) -> Position {
    [round1(v.x), round1(v.y), round1(v.z)]
}

/// Removes TF2 chat colour codes: \x01-\x06 select a colour, \x07 takes RRGGBB, \x08 RRGGBBAA.
pub(crate) fn strip_colors(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            CHAT_RGB => chars.by_ref().take(RGB_DIGITS).for_each(drop),
            CHAT_RGBA => chars.by_ref().take(RGBA_DIGITS).for_each(drop),
            c if CHAT_CODES.contains(&c) => {}
            _ => out.push(c),
        }
    }
    out.trim_end_matches('\n').to_string()
}

/// The coloured spans of a chat line with TF2 colour codes, or None without any. \x03 is the
/// sender's team colour, \x04 and \x05 Valve's green ones, \x07 and \x08 a hex colour.
pub(crate) fn color_parts(text: &str) -> Option<Vec<ChatPart>> {
    if !text.chars().any(|c| CHAT_CODES.contains(&c)) {
        return None;
    }
    let mut parts = Vec::new();
    let (mut color, mut run): (Option<ChatColor>, String) = (None, String::new());
    let mut chars = text.trim_end_matches('\n').chars();
    while let Some(c) = chars.next() {
        let next = match c {
            CHAT_RGB | CHAT_RGBA => {
                let hex: String = chars.by_ref().take(if c == CHAT_RGB { RGB_DIGITS } else { RGBA_DIGITS }).collect();
                Some(hex.get(..RGB_DIGITS).and_then(ChatColor::from_hex))
            }
            CHAT_TEAM => Some(Some(ChatColor::Team)),
            CHAT_GREEN => Some(Some(ChatColor::Rgb(GREEN))),
            CHAT_LIGHT_GREEN => Some(Some(ChatColor::Rgb(LIGHT_GREEN))),
            c if CHAT_DEFAULT.contains(&c) => Some(None),
            _ => {
                run.push(c);
                None
            }
        };
        if let Some(next) = next {
            if !run.is_empty() {
                parts.push(ChatPart { color, text: std::mem::take(&mut run) });
            }
            color = next;
        }
    }
    if !run.is_empty() {
        parts.push(ChatPart { color, text: run });
    }
    Some(parts)
}

// tf-demo-parser's `Building::class()` reports dispensers as sentries; go by the variant.
pub(super) fn building_kind_of(b: &Building) -> BuildingKind {
    match b {
        Building::Sentry(_) => BuildingKind::Sentry,
        Building::Dispenser(_) => BuildingKind::Dispenser,
        Building::Teleporter(_) => BuildingKind::Teleporter,
        _ => BuildingKind::Unknown,
    }
}

pub(crate) fn building_kind(object: u16) -> BuildingKind {
    match object {
        0 => BuildingKind::Dispenser,
        1 => BuildingKind::Teleporter,
        2 => BuildingKind::Sentry,
        3 => BuildingKind::Sapper,
        _ => BuildingKind::Unknown,
    }
}
