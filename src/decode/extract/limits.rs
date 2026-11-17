//! The game's sizes and values the extractor reads by.

use std::sync::LazyLock;

use tf_demo_parser::demo::sendprop::SendPropIdentifier;

/// `CTFObjectiveResource` has room for this many control points, and its per team arrays for
/// this many teams: element `team * MAX_CONTROL_POINTS + point` (`TEAM_ARRAY`).
pub(super) const MAX_CONTROL_POINTS: usize = 8;

pub(super) const MAX_TEAMS: usize = 4;

/// A player's condition words: `m_nPlayerCond` and `_condition_bits` (both conditions 0-31),
/// then `m_nPlayerCondEx..Ex4`, 32 conditions each.
pub(super) const COND_WORDS: usize = 6;

pub(super) const COND_BITS: usize = 32;

/// A medigun's charge when full, in percent.
pub(super) const FULL_CHARGE: u8 = 100;

pub(super) const SECONDS_PER_MINUTE: f32 = 60.0;

/// `m_hMyWeapons` has `MAX_WEAPONS` slots.
pub(super) const MY_WEAPON_SLOTS: usize = 48;

pub(super) static PROP_MY_WEAPONS: LazyLock<[SendPropIdentifier; MY_WEAPON_SLOTS]> =
    LazyLock::new(|| std::array::from_fn(|i| SendPropIdentifier::new("m_hMyWeapons", &format!("{i:03}"))));

/// `m_iWeaponState`: idle, starting to fire, firing, airblast.
pub(super) const FLAMETHROWER_STARTING: i64 = 1;

pub(super) const FLAMETHROWER_FIRING: i64 = 2;

pub(super) const FLAMETHROWER_AIRBLAST: i64 = 3;

/// `m_nChargeResistType`: bullet, blast, fire.
pub(super) const RESIST_BULLET: i64 = 0;

pub(super) const RESIST_BLAST: i64 = 1;
