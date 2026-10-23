//! Reading send props: values as numbers, entity handles, and the props every entity has.

use tf_demo_parser::demo::sendprop::{SendProp, SendPropIdentifier, SendPropValue};
use tf_demo_parser::demo::vector::Vector;

/// A send prop by send table and name. Elements of an array prop have the array as
/// table and their index as name.
pub(crate) const fn prop_id(table: &str, name: &str) -> SendPropIdentifier {
    SendPropIdentifier::new(table, name)
}

/// A vector sent as three floats, `m_vecOrigin[0]` to `[2]`.
pub(crate) const fn origin_parts(table: &str) -> [SendPropIdentifier; 3] {
    [prop_id(table, "m_vecOrigin[0]"), prop_id(table, "m_vecOrigin[1]"), prop_id(table, "m_vecOrigin[2]")]
}

pub(crate) const PROP_TEAM_NUM: SendPropIdentifier = prop_id("DT_BaseEntity", "m_iTeamNum");
pub(crate) const PROP_ORIGIN: SendPropIdentifier = prop_id("DT_BaseEntity", "m_vecOrigin");
pub(crate) const PROP_EFFECTS: SendPropIdentifier = prop_id("DT_BaseEntity", "m_fEffects");
/// `m_fEffects`: not drawn.
pub(crate) const EF_NODRAW: i64 = 0x20;

/// Where each team is in this crate's own `[red, blue]` pairs.
pub(crate) const RED_SLOT: usize = 0;
pub(crate) const BLUE_SLOT: usize = 1;

/// The handle value of "no entity".
pub(crate) const INVALID_EHANDLE: i64 = 0x1f_ffff;
/// Entity handles carry the entity index in their low 11 bits.
const HANDLE_INDEX_MASK: i64 = (1 << 11) - 1;

/// The entity index in a handle.
pub(crate) fn handle_entity(handle: i64) -> u32 {
    (handle & HANDLE_INDEX_MASK) as u32
}

/// The entity a handle prop points at; none for the invalid handle (and 0, the world).
pub(crate) fn opt_handle(handle: i64) -> Option<u32> {
    (handle != INVALID_EHANDLE && handle != 0).then(|| handle_entity(handle))
}

/// A team number prop; out of range values are no team.
pub(crate) fn team_byte(v: i64) -> u8 {
    u8::try_from(v).unwrap_or(0)
}

/// A count prop as written out: below 0 is 0, past `u16::MAX` is `u16::MAX`.
pub(crate) fn saturating_u16(v: i64) -> u16 {
    u16::try_from(v.max(0)).unwrap_or(u16::MAX)
}

/// A prop value as a number, whichever number type the table gave it; 0 otherwise.
pub(crate) fn as_int(v: &SendPropValue) -> i64 {
    match v {
        SendPropValue::Integer(i) => *i,
        SendPropValue::Float(f) => *f as i64,
        _ => 0,
    }
}

pub(crate) fn as_float(v: &SendPropValue) -> f32 {
    match v {
        SendPropValue::Float(f) => *f,
        SendPropValue::Integer(i) => *i as f32,
        _ => 0.0,
    }
}

pub(crate) fn prop(props: &[SendProp], id: SendPropIdentifier) -> Option<&SendPropValue> {
    props.iter().find(|p| p.identifier == id).map(|p| &p.value)
}

pub(crate) fn prop_f(props: &[SendProp], id: SendPropIdentifier) -> f32 {
    prop(props, id).map_or(0.0, as_float)
}

pub(crate) fn prop_i(props: &[SendProp], id: SendPropIdentifier) -> i64 {
    prop(props, id).map_or(0, as_int)
}

pub(crate) fn prop_vec(props: &[SendProp], id: SendPropIdentifier) -> Option<Vector> {
    match prop(props, id)? {
        SendPropValue::Vector(v) => Some(*v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_handles() {
        assert_eq!(handle_entity(40967), 7);
        assert_eq!(handle_entity(7), 7);
        assert_eq!(opt_handle(40967), Some(7));
        assert_eq!(opt_handle(INVALID_EHANDLE), None);
        assert_eq!(opt_handle(0), None);
    }

    #[test]
    fn counts_saturate() {
        assert_eq!(saturating_u16(-3), 0);
        assert_eq!(saturating_u16(70_000), u16::MAX);
        assert_eq!(team_byte(-1), 0);
    }
}
