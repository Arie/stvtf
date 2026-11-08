//! Every entity's props as the frames so far left them, to write a full update from: a client
//! that starts from it has the same entities as one that followed from the start.
//!
//! The full update sends each entity entering with the props that differ from its class's static
//! baseline (the `instancebaseline` string table, which the client has too); every prop instead
//! would not fit the message's 20-bit length. Frames that build entities on baselines of their own
//! (`updated_base_line`) cannot be followed from it, as the client would lack those baselines:
//! STV demos and relays never send them, but if one comes no full update is written.

use crate::net::bits::{BitBuf, BitReader, BitWriter};
use crate::net::messages::{self, TYPE_BITS};
use bitbuffer::{BitWriteStream, LittleEndian};
use std::collections::BTreeMap;
use tf_demo_parser::ParserState;
use tf_demo_parser::demo::message::packetentities::{BaselineIndex, PacketEntitiesMessage, PacketEntity, UpdateType};
use tf_demo_parser::demo::packet::datatable::ClassId;
use tf_demo_parser::demo::parser::Encode;
use tf_demo_parser::demo::sendprop::{SendProp, SendPropValue};

const SVC_PACKETENTITIES: u32 = 26;

struct Entity {
    class: ClassId,
    serial: u32,
    /// By prop index, every prop sent since the entity entered, over its baseline.
    props: BTreeMap<u32, SendProp>,
}

/// An entity's class, serial and props by index, as text.
#[cfg(test)]
pub(crate) type Dumped = (u16, u32, Vec<(u32, String)>);

#[derive(Default)]
pub(crate) struct World {
    entities: BTreeMap<u32, Entity>,
    max_entries: u16,
    /// No full update came yet, or a frame was lost or built on a baseline of its own since.
    broken: bool,
    seen_full: bool,
}

impl World {
    /// Applies a frame's entities; `state` is the parser's once it took the frame.
    pub(crate) fn apply(&mut self, pe: &PacketEntitiesMessage, state: &ParserState) {
        self.max_entries = pe.max_entries;
        if pe.delta.is_none() {
            self.entities.clear();
            self.broken = false;
            self.seen_full = true;
        }
        if pe.updated_base_line {
            self.broken = true;
        }
        for e in &pe.entities {
            let index = u32::from(e.entity_index);
            match e.update_type {
                UpdateType::Enter => {
                    let Some(table) = state.send_tables.get(usize::from(e.server_class)) else {
                        self.broken = true;
                        continue;
                    };
                    let mut props: BTreeMap<u32, SendProp> = state
                        .get_baseline(pe.base_line, e.entity_index, e.server_class, table, pe.delta.is_some())
                        .map(|b| b.iter().map(|p| (p.index, p.clone())).collect())
                        .unwrap_or_default();
                    props.extend(e.props.iter().map(|p| (p.index, p.clone())));
                    self.entities.insert(index, Entity { class: e.server_class, serial: e.serial_number, props });
                }
                UpdateType::Delta => match self.entities.get_mut(&index) {
                    Some(w) => w.props.extend(e.props.iter().map(|p| (p.index, p.clone()))),
                    None => self.broken = true,
                },
                UpdateType::Leave | UpdateType::Delete => {
                    self.entities.remove(&index);
                }
            }
        }
        for r in &pe.removed_entities {
            self.entities.remove(&u32::from(*r));
        }
    }

    /// The entities may be off from here until the next full update.
    pub(crate) fn lost_frame(&mut self) {
        self.broken = true;
    }

    /// Each entity's class, serial and props, to compare two worlds.
    #[cfg(test)]
    pub(crate) fn dump(&self) -> BTreeMap<u32, Dumped> {
        self.entities
            .iter()
            .map(|(&i, e)| {
                (
                    i,
                    (
                        u16::from(e.class),
                        e.serial,
                        e.props.values().map(|p| (p.index, format!("{:?}", p.value))).collect(),
                    ),
                )
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entities.len()
    }

    /// `net_Tick(tick)` and the entities in full, as one message stream; None when they cannot
    /// be trusted (see `broken`) or do not encode.
    pub(crate) fn full_update(&self, state: &ParserState, tick: u32) -> Option<BitBuf> {
        if self.broken || !self.seen_full {
            return None;
        }
        let mut entities = Vec::with_capacity(self.entities.len());
        for (&index, e) in &self.entities {
            let table = state.send_tables.get(usize::from(e.class))?;
            let base: BTreeMap<u32, SendProp> = state
                .get_static_baseline(e.class, table)
                .unwrap_or_default()
                .into_iter()
                .map(|p| (p.index, p))
                .collect();
            let props = e
                .props
                .values()
                .filter(|p| !base.get(&p.index).is_some_and(|b| same(&b.value, &p.value)))
                .cloned()
                .collect();
            entities.push(PacketEntity {
                server_class: e.class,
                entity_index: index.into(),
                props,
                in_pvs: true,
                update_type: UpdateType::Enter,
                serial_number: e.serial,
                delay: None,
                delta: None,
                baseline_index: BaselineIndex::First,
            });
        }
        let msg = PacketEntitiesMessage {
            entities,
            removed_entities: Vec::new(),
            max_entries: self.max_entries,
            delta: None,
            base_line: BaselineIndex::First,
            updated_base_line: false,
        };
        let mut bytes = Vec::new();
        let nbits = {
            let mut s = BitWriteStream::new(&mut bytes, LittleEndian);
            msg.encode(&mut s, state).ok()?;
            s.bit_len()
        };
        let mut w = BitWriter::new();
        w.write_buf(&messages::tick(tick as i32));
        w.write_bits(SVC_PACKETENTITIES, TYPE_BITS);
        w.write_buf(&BitReader::slice(&bytes, 0..nbits));
        Some(w.finish())
    }
}

/// Whether two prop values are exactly the same. The parser's own `==` takes floats as equal when
/// the first is less than the second (plus 0.001), and an integer as equal to a float.
fn same(a: &SendPropValue, b: &SendPropValue) -> bool {
    use SendPropValue as V;
    match (a, b) {
        (V::Integer(x), V::Integer(y)) => x == y,
        (V::Float(x), V::Float(y)) => x.to_bits() == y.to_bits(),
        (V::Vector(x), V::Vector(y)) => [x.x, x.y, x.z].map(f32::to_bits) == [y.x, y.y, y.z].map(f32::to_bits),
        (V::VectorXY(x), V::VectorXY(y)) => [x.x, x.y].map(f32::to_bits) == [y.x, y.y].map(f32::to_bits),
        (V::String(x), V::String(y)) => x == y,
        (V::Array(x), V::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y)),
        _ => false,
    }
}
