//! The `SendTable` CRC a client proves its class tables with at signon, from a demo's tables.
//!
//! The engine (`SendTable_ComputeCRC`) runs one CRC-32 over the send table of each server class,
//! in class id order, without their child tables. Per table: its name, its prop count, then per
//! prop its type, name and flags, then the data table or excluded table it names, an array's
//! element count, or the low value, high value and bit count. Numbers go in as 4 bytes, little
//! endian; strings without their terminator.
//!
//! A demo has all of that but one flag: `SPROP_ENCODED_AGAINST_TICKCOUNT` (bit 16) is server
//! side only, never sent, and set on just `m_flSimulationTime` and `m_flAnimTime`, so it is put
//! back on those.

use anyhow::{Context, Result};
use tf_demo_parser::demo::packet::datatable::DataTablePacket;
use tf_demo_parser::demo::sendprop::{RawSendPropDefinition, SendPropFlag, SendPropFlags, SendPropType};

const ENCODED_AGAINST_TICKCOUNT: u32 = 1 << 16;
const ENCODED_AGAINST_TICKCOUNT_PROPS: [&str; 2] = ["m_flSimulationTime", "m_flAnimTime"];

/// Every flag the wire carries: 16 bits, each one a variant.
const FLAGS: [SendPropFlag; 16] = [
    SendPropFlag::Unsigned,
    SendPropFlag::Coord,
    SendPropFlag::NoScale,
    SendPropFlag::RoundDown,
    SendPropFlag::RoundUp,
    SendPropFlag::NormalVarInt,
    SendPropFlag::Exclude,
    SendPropFlag::XYZE,
    SendPropFlag::InsideArray,
    SendPropFlag::ProxyAlwaysYes,
    SendPropFlag::ChangesOften,
    SendPropFlag::IsVectorElement,
    SendPropFlag::Collapsible,
    SendPropFlag::CoordMP,
    SendPropFlag::CoordMPLowPrecision,
    SendPropFlag::CoordMPIntegral,
];

fn bits(flags: SendPropFlags) -> u32 {
    FLAGS.iter().filter(|&&f| flags.contains(f)).map(|&f| u32::from(f as u16)).sum()
}

pub(super) fn sendtable_crc(dt: &DataTablePacket) -> Result<u32> {
    let mut h = crc32fast::Hasher::new();
    for class in &dt.server_classes {
        let table = dt.tables.iter().find(|t| t.name == class.data_table).with_context(|| {
            format!("no send table {} for class {}", class.data_table.as_str(), class.name.as_str())
        })?;
        h.update(table.name.as_bytes());
        // The parser keeps an array's element prop inside the array; the engine lists it just
        // before.
        let props: Vec<&RawSendPropDefinition> =
            table.props.iter().flat_map(|p| p.array_property.as_deref().into_iter().chain([p])).collect();
        h.update(&(props.len() as u32).to_le_bytes());
        for p in props {
            prop(&mut h, p)?;
        }
    }
    Ok(h.finalize())
}

fn prop(h: &mut crc32fast::Hasher, p: &RawSendPropDefinition) -> Result<()> {
    let name = p.name.as_str();
    let mut flags = bits(p.flags);
    if p.prop_type == SendPropType::Int && !p.is_exclude() && ENCODED_AGAINST_TICKCOUNT_PROPS.contains(&name) {
        flags |= ENCODED_AGAINST_TICKCOUNT;
    }
    h.update(&(p.prop_type as u32).to_le_bytes());
    h.update(name.as_bytes());
    h.update(&flags.to_le_bytes());
    let missing = || format!("send prop {name} lacks a field its type has");
    if p.prop_type == SendPropType::DataTable || p.is_exclude() {
        h.update(p.table_name.as_ref().with_context(missing)?.as_bytes());
    } else if p.prop_type == SendPropType::Array {
        h.update(&u32::from(p.element_count.with_context(missing)?).to_le_bytes());
    } else {
        h.update(&p.low_value.with_context(missing)?.to_le_bytes());
        h.update(&p.high_value.with_context(missing)?.to_le_bytes());
        // The bits as sent: the parser widens those of unscaled floats.
        h.update(&p.original_bit_count.with_context(missing)?.to_le_bytes());
    }
    Ok(())
}
