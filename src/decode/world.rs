//! Every entity's props as the frames so far left them, to write a full update from: a client
//! that starts from it has the same entities as one that followed from the start.
//!
//! The full update sends each entity entering with the props that differ from its class's static
//! baseline (the `instancebaseline` string table, which the client has too); every prop instead
//! would not fit the message's 20-bit length.
//!
//! A delta builds on the frame it names, which from a live `SourceTV` is the last one acked, often a
//! few ticks back: the frames since are kept (entities shared between them), as a game client keeps
//! them. An entity entering on a baseline of the stream's own (`updated_base_line`) cannot be
//! followed from a full update, as the client lacks that baseline: a server sends such baselines
//! to a viewer that acks them, which stvtf never does, so they stay unused, but if one is used no
//! full update is written.

use crate::net::bits::{BitBuf, BitReader, BitWriter};
use crate::net::messages::{self, TYPE_BITS};
use bitbuffer::{BitWriteStream, LittleEndian};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use tf_demo_parser::ParserState;
use tf_demo_parser::demo::message::packetentities::{BaselineIndex, PacketEntitiesMessage, PacketEntity, UpdateType};
use tf_demo_parser::demo::packet::datatable::ClassId;
use tf_demo_parser::demo::parser::Encode;
use tf_demo_parser::demo::sendprop::{SendProp, SendPropValue};

const SVC_PACKETENTITIES: u32 = 26;

/// Frames kept for later deltas to build on, at most.
const MAX_FRAMES: usize = 128;

#[derive(Clone)]
struct Entity {
    class: ClassId,
    serial: u32,
    /// By prop index, every prop sent since the entity entered, over its baseline.
    props: BTreeMap<u32, SendProp>,
}

/// An entity's class, serial and props by index, as text.
#[cfg(test)]
pub(crate) type Dumped = (u16, u32, Vec<(u32, String)>);

/// A frame's entities by index.
type Frame = BTreeMap<u32, Arc<Entity>>;

#[derive(Default)]
pub(crate) struct World {
    /// The frames by server tick, oldest first: the last is the latest.
    frames: VecDeque<(u32, Frame)>,
    max_entries: u16,
    /// No full update came yet, or a frame was lost or built on a baseline of its own since.
    broken: bool,
    seen_full: bool,
}

impl World {
    /// Applies the entities of the frame at server tick `tick`; `state` is the parser's once it
    /// took the frame.
    pub(crate) fn apply(&mut self, pe: &PacketEntitiesMessage, state: &ParserState, tick: u32) {
        self.max_entries = pe.max_entries;
        let mut frame = match pe.delta {
            None => {
                self.broken = false;
                self.seen_full = true;
                Frame::new()
            }
            Some(base) => {
                if let Some((_, f)) = self.frames.iter().rev().find(|(t, _)| *t == u32::from(base)) {
                    f.clone()
                } else {
                    self.broken = true;
                    self.latest().cloned().unwrap_or_default()
                }
            }
        };
        let dynamic = &state.instance_baselines[usize::from(pe.base_line == BaselineIndex::Second)];
        for e in &pe.entities {
            let index = u32::from(e.entity_index);
            match e.update_type {
                UpdateType::Enter => {
                    let Some(table) = state.send_tables.get(usize::from(e.server_class)) else {
                        self.broken = true;
                        continue;
                    };
                    if pe.delta.is_some()
                        && dynamic.get(e.entity_index).is_some_and(|b| b.server_class == e.server_class)
                    {
                        self.broken = true;
                    }
                    let mut props: BTreeMap<u32, SendProp> = state
                        .get_baseline(pe.base_line, e.entity_index, e.server_class, table, pe.delta.is_some())
                        .map(|b| b.iter().map(|p| (p.index, p.clone())).collect())
                        .unwrap_or_default();
                    props.extend(e.props.iter().map(|p| (p.index, p.clone())));
                    frame.insert(index, Arc::new(Entity { class: e.server_class, serial: e.serial_number, props }));
                }
                UpdateType::Delta => match frame.get_mut(&index) {
                    Some(w) => Arc::make_mut(w).props.extend(e.props.iter().map(|p| (p.index, p.clone()))),
                    None => self.broken = true,
                },
                UpdateType::Leave | UpdateType::Delete => {
                    frame.remove(&index);
                }
            }
        }
        for r in &pe.removed_entities {
            frame.remove(&u32::from(*r));
        }
        // A later delta never builds on a frame older than this one's base.
        if let Some(base) = pe.delta {
            self.frames.retain(|(t, _)| *t >= u32::from(base));
        } else {
            self.frames.clear();
        }
        self.frames.push_back((tick, frame));
        if self.frames.len() > MAX_FRAMES {
            self.frames.pop_front();
        }
    }

    fn latest(&self) -> Option<&Frame> {
        self.frames.back().map(|(_, f)| f)
    }

    /// The entities may be off from here until the next full update.
    pub(crate) fn lost_frame(&mut self) {
        self.broken = true;
    }

    /// Each entity's class, serial and props, to compare two worlds.
    #[cfg(test)]
    pub(crate) fn dump(&self) -> BTreeMap<u32, Dumped> {
        self.latest()
            .into_iter()
            .flatten()
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
        self.latest().map_or(0, BTreeMap::len)
    }

    /// `net_Tick` and the entities in full of the frame at server tick `at` (the latest when it is
    /// not kept, or not given), as one message stream: the frame the next delta builds on, so a
    /// client that starts from it can follow. None when they cannot be trusted (see `broken`) or do
    /// not encode.
    pub(crate) fn full_update(&self, state: &ParserState, at: Option<u32>) -> Option<(u32, BitBuf)> {
        if self.broken || !self.seen_full {
            return None;
        }
        let (tick, frame) = at
            .and_then(|at| self.frames.iter().rev().find(|(t, _)| *t == at))
            .or(self.frames.back())
            .map(|(t, f)| (*t, f))?;
        let mut entities = Vec::with_capacity(frame.len());
        for (&index, e) in frame {
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
        Some((tick, w.finish()))
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
