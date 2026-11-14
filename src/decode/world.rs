//! Every entity's props as the frames so far left them, to write a full update from: a client
//! that starts from it has the same entities as one that followed from the start.
//!
//! The full update sends each entity entering with the props that differ from its class's static
//! baseline (the `instancebaseline` string table, which the client has too); every prop instead
//! would not fit the message's 20-bit length.
//!
//! A delta builds on the frame it names, which from a live `SourceTV` is the last one acked, often
//! a few ticks back: the frames since are kept (entities shared between them), as a game client
//! keeps them. An entity entering on a baseline of the stream's own (`updated_base_line`) cannot be
//! followed from a full update, as the client lacks that baseline: a server sends such baselines to
//! a viewer that acks them, which stvtf never does, so they stay unused, but if one is used no full
//! update is written.

use crate::net::bits::{BitBuf, BitWriter};
use crate::net::messages::{self, SVC_PACKETENTITIES};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use tf_demo_parser::ParserState;
use tf_demo_parser::demo::message::packetentities::{BaselineIndex, PacketEntitiesMessage, PacketEntity, UpdateType};
use tf_demo_parser::demo::packet::datatable::ClassId;
use tf_demo_parser::demo::sendprop::{SendProp, SendPropIdentifier, SendPropValue};

/// Ticks of frames kept for a delta to a client to build on: 3 s of a 66.67-tick server's, more
/// than a client's ack takes to come.
const KEEP_TICKS: u32 = 200;
/// Frames kept for later deltas to build on, at most.
const MAX_FRAMES: usize = 128;

#[derive(Clone)]
struct Entity {
    class: ClassId,
    serial: u32,
    /// By prop index, every prop sent since the entity entered, over its baseline.
    props: Props,
}

/// An entity's props, by index, in pages of [`PAGE`] indexes, each a sorted list: a frame that
/// changes a prop copies its page, not every prop of the entity (players have hundreds).
#[derive(Clone, Default)]
struct Props(Vec<Option<Arc<Vec<SendProp>>>>);

/// Prop indexes per page.
const PAGE: u32 = 32;

impl Props {
    fn set(&mut self, p: &SendProp) {
        let page = (p.index / PAGE) as usize;
        if self.0.len() <= page {
            self.0.resize(page + 1, None);
        }
        let list = Arc::make_mut(self.0[page].get_or_insert_with(Arc::default));
        match list.binary_search_by_key(&p.index, |q| q.index) {
            Ok(i) => list[i] = p.clone(),
            Err(i) => list.insert(i, p.clone()),
        }
    }

    fn get(&self, index: u32) -> Option<&SendProp> {
        let list = self.0.get((index / PAGE) as usize)?.as_ref()?;
        list.binary_search_by_key(&index, |q| q.index).ok().map(|i| &list[i])
    }

    fn values(&self) -> impl Iterator<Item = &SendProp> {
        self.0.iter().flatten().flat_map(|l| l.iter())
    }
}

/// A frame's entities, by index: a slot per index, so copying one for the next frame is cheap.
#[derive(Clone, Default)]
struct Frame(Vec<Option<Arc<Entity>>>);

impl Frame {
    fn get(&self, index: u32) -> Option<&Arc<Entity>> {
        self.0.get(index as usize)?.as_ref()
    }

    fn get_mut(&mut self, index: u32) -> Option<&mut Arc<Entity>> {
        self.0.get_mut(index as usize)?.as_mut()
    }

    fn insert(&mut self, index: u32, e: Arc<Entity>) {
        let i = index as usize;
        if self.0.len() <= i {
            self.0.resize(i + 1, None);
        }
        self.0[i] = Some(e);
    }

    fn remove(&mut self, index: u32) {
        if let Some(slot) = self.0.get_mut(index as usize) {
            *slot = None;
        }
    }

    fn iter(&self) -> impl Iterator<Item = (u32, &Arc<Entity>)> {
        self.0.iter().enumerate().filter_map(|(i, e)| Some((i as u32, e.as_ref()?)))
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.iter().flatten().count()
    }
}

/// An entity's class, serial and props by index, as text.
#[cfg(test)]
pub(crate) type Dumped = (u16, u32, Vec<(u32, String)>);

#[derive(Default)]
pub(crate) struct World {
    /// The frames by server tick, oldest first: the last is the latest.
    frames: VecDeque<(u32, Frame)>,
    max_entries: u16,
    /// No full update came yet, or a frame was lost or built on a baseline of its own since.
    broken: bool,
    seen_full: bool,
    /// Whether the stream's full update set baselines (as a server's to a viewer does): a
    /// keyframe does the same, so the client keeps the baselines the stream's frames expect.
    full_sets_baselines: bool,
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
                self.full_sets_baselines = pe.updated_base_line;
                Frame::default()
            }
            Some(base) => {
                if let Some((_, f)) = self.kept(u32::from(base)) {
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
                    let mut props = Props::default();
                    if let Ok(base) =
                        state.get_baseline(pe.base_line, e.entity_index, e.server_class, table, pe.delta.is_some())
                    {
                        for p in base.iter() {
                            props.set(p);
                        }
                    }
                    for p in &e.props {
                        props.set(p);
                    }
                    frame.insert(index, Arc::new(Entity { class: e.server_class, serial: e.serial_number, props }));
                }
                UpdateType::Delta => match frame.get_mut(index) {
                    Some(w) => {
                        let w = Arc::make_mut(w);
                        for p in &e.props {
                            w.props.set(p);
                        }
                    }
                    None => self.broken = true,
                },
                UpdateType::Leave | UpdateType::Delete => {
                    frame.remove(index);
                }
            }
        }
        for r in &pe.removed_entities {
            frame.remove(u32::from(*r));
        }
        // A later delta of the stream never builds on a frame older than this one's base, but one
        // written for a client does, on the frame it acked: those of the last [`KEEP_TICKS`] stay.
        // A full update keeps them: a stream that asked for one can have deltas from before it.
        if let Some(base) = pe.delta {
            let from = u32::from(base).min(tick.saturating_sub(KEEP_TICKS));
            self.frames.retain(|(t, _)| *t >= from);
        }
        self.frames.push_back((tick, frame));
        if self.frames.len() > MAX_FRAMES {
            self.frames.pop_front();
        }
    }

    /// Whether frame `tick` is kept.
    pub(crate) fn has(&self, tick: u32) -> bool {
        self.kept(tick).is_some()
    }

    /// Frame `tick`, with its tick, if kept.
    fn kept(&self, tick: u32) -> Option<&(u32, Frame)> {
        self.frames.iter().rev().find(|(t, _)| *t == tick)
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
            .flat_map(Frame::iter)
            .map(|(i, e)| {
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
        self.latest().map_or(0, Frame::len)
    }

    /// `net_Tick` and the entities in full of the frame at server tick `at` (the latest when not
    /// given; None when it is not kept), as one message stream: the frame the next delta builds on,
    /// so a client that starts from it can follow. None when they cannot be trusted (see `broken`)
    /// or do not encode.
    ///
    /// With `hold` (the seconds a tick lasts), every running round timer is held where it is at
    /// that frame: a client whose clock runs on with no frames coming (a room paused) shows the
    /// time as it was, until a full update without.
    pub(crate) fn full_update(&self, state: &ParserState, at: Option<u32>, hold: Option<f32>) -> Option<(u32, BitBuf)> {
        if self.broken || !self.seen_full {
            return None;
        }
        let (tick, frame) = match at {
            Some(at) => self.kept(at),
            None => self.frames.back(),
        }
        .map(|(t, f)| (*t, f))?;
        let mut entities = Vec::new();
        for (index, e) in frame.iter() {
            let mut entering = enter(state, index, e)?;
            let clock = timer_props(state, e, hold.map(|interval| tick as f32 * interval));
            for set in clock.into_iter().chain(reset_parity(state, e)) {
                entering.props.retain(|p| !set.iter().any(|q| q.index == p.index));
                entering.props.extend(set);
            }
            entering.props.sort_by_key(|p| p.index);
            entities.push(entering);
        }
        let msg = PacketEntitiesMessage {
            entities,
            removed_entities: Vec::new(),
            max_entries: self.max_entries,
            delta: None,
            base_line: BaselineIndex::First,
            updated_base_line: self.full_sets_baselines,
        };
        Some((tick, encode(state, &msg, tick)?))
    }

    /// `net_Tick(to)` and the entities of frame `to` as a delta from frame `from`, both kept: a
    /// client that has `from` gets `to` from it, where the stream's own frame `to` builds on one
    /// the client lacks.
    pub(crate) fn delta(&self, state: &ParserState, from: u32, to: u32) -> Option<BitBuf> {
        let entities = self.delta_entities(state, from, to)?;
        let msg = PacketEntitiesMessage {
            entities: entities.into_values().collect(),
            removed_entities: Vec::new(),
            max_entries: self.max_entries,
            delta: Some(from.into()),
            base_line: BaselineIndex::First,
            updated_base_line: false,
        };
        encode(state, &msg, to)
    }

    /// The entities of frame `to` as a delta from frame `from`, both kept, as the one message: to
    /// put in a frame's stream in place of its own, for a client that has `from` and not the frame
    /// those build on.
    pub(crate) fn delta_message(&self, state: &ParserState, from: u32, to: u32) -> Option<BitBuf> {
        let entities = self.delta_entities(state, from, to)?;
        let msg = PacketEntitiesMessage {
            entities: entities.into_values().collect(),
            removed_entities: Vec::new(),
            max_entries: self.max_entries,
            delta: Some(from.into()),
            base_line: BaselineIndex::First,
            updated_base_line: false,
        };
        super::encode_message(SVC_PACKETENTITIES, &msg, state)
    }

    /// By entity index, the props that changed from one kept frame to the next between `from` and
    /// `to`; none (the entity enters anew) for one that was gone, or another, in a frame between.
    fn changed_between(&self, from: u32, to: u32) -> BTreeMap<u32, Option<std::collections::BTreeSet<u32>>> {
        let mut changed: BTreeMap<u32, Option<std::collections::BTreeSet<u32>>> = BTreeMap::new();
        let way: Vec<&Frame> = self.frames.iter().filter(|(t, _)| (from..=to).contains(t)).map(|(_, f)| f).collect();
        for pair in way.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            for index in 0..a.0.len().max(b.0.len()) as u32 {
                match (a.get(index), b.get(index)) {
                    (None, None) => {}
                    (Some(x), Some(y)) if Arc::ptr_eq(x, y) => {}
                    (Some(x), Some(y)) if x.class == y.class && x.serial == y.serial => {
                        let Some(set) = changed.entry(index).or_insert_with(|| Some(std::collections::BTreeSet::new()))
                        else {
                            continue;
                        };
                        for (i, page) in y.props.0.iter().enumerate() {
                            let Some(page) = page else { continue };
                            let before = x.props.0.get(i).and_then(Option::as_ref);
                            if before.is_some_and(|p| Arc::ptr_eq(p, page)) {
                                continue;
                            }
                            set.extend(
                                page.iter()
                                    .filter(|p| !x.props.get(p.index).is_some_and(|q| same(&q.value, &p.value)))
                                    .map(|p| p.index),
                            );
                        }
                    }
                    _ => {
                        changed.insert(index, None);
                    }
                }
            }
        }
        changed
    }

    /// What frame `to` changed from frame `from`, by entity index, as a delta lists it.
    fn delta_entities(&self, state: &ParserState, from: u32, to: u32) -> Option<BTreeMap<u32, PacketEntity>> {
        if self.broken {
            return None;
        }
        let (old, new) = (&self.kept(from)?.1, &self.kept(to)?.1);
        let changed = self.changed_between(from, to);
        // By index, as the message lists them: what changed or entered, and what is gone. A game
        // client keeps every entity a delta does not name, so one gone is named, leaving and
        // deleted, as a server does (the message's own deletion list only frees what is left).
        let mut entities: BTreeMap<u32, PacketEntity> = BTreeMap::new();
        for (index, e) in new.iter() {
            match old.get(index) {
                Some(o) if o.class == e.class && o.serial == e.serial && changed.get(&index) != Some(&None) => {
                    // Every prop that changed on the way, as a server lists them: one back at
                    // its value in `from` is not at it for a client that took a frame between.
                    let on_the_way = changed.get(&index).and_then(Option::as_ref);
                    let props: Vec<SendProp> = e
                        .props
                        .values()
                        .filter(|p| {
                            on_the_way.is_some_and(|c| c.contains(&p.index))
                                || !o.props.get(p.index).is_some_and(|q| same(&q.value, &p.value))
                        })
                        .cloned()
                        .collect();
                    if !props.is_empty() {
                        entities.insert(index, packet_entity(index, e, UpdateType::Delta, props, Some(from)));
                    }
                }
                _ => {
                    entities.insert(index, enter(state, index, e)?);
                }
            }
        }
        for (index, o) in old.iter().filter(|(i, _)| new.get(*i).is_none()) {
            entities.insert(index, packet_entity(index, o, UpdateType::Delete, Vec::new(), Some(from)));
        }
        Some(entities)
    }
}

/// A round timer's clock, every prop of it, for a full update: as it is, or with `hold` (the time
/// now) a running one held where it is. Every one, not those off its baseline only: a client
/// keeps a timer held by an earlier full update, until told it runs. None for another entity.
fn timer_props(state: &ParserState, e: &Entity, hold: Option<f32>) -> Option<Vec<SendProp>> {
    if state.server_classes.get(usize::from(e.class))?.name.as_str() != "CTeamRoundTimer" {
        return None;
    }
    let table = state.send_tables.get(usize::from(e.class))?;
    let prop = |name: &str| {
        let id = SendPropIdentifier::new("DT_TeamRoundTimer", name);
        Some((super::props::prop_index(table, id)?, id))
    };
    let ((paused_at, paused_id), (left_at, left_id), (end_at, end_id)) =
        (prop("m_bTimerPaused")?, prop("m_flTimeRemaining")?, prop("m_flTimerEndTime")?);
    let paused = e.props.get(paused_at).is_some_and(|p| matches!(p.value, SendPropValue::Integer(v) if v != 0));
    let float = |at: u32| match e.props.get(at).map(|p| &p.value) {
        Some(SendPropValue::Float(v)) => *v,
        _ => 0.0,
    };
    let (end, left) = (float(end_at), float(left_at));
    let (paused, left) = match hold {
        Some(now) if !paused => (true, (end - now).max(0.0)),
        _ => (paused, left),
    };
    Some(vec![
        SendProp { index: paused_at, identifier: paused_id, value: SendPropValue::Integer(i64::from(paused)) },
        SendProp { index: left_at, identifier: left_id, value: SendPropValue::Float(left) },
        SendProp { index: end_at, identifier: end_id, value: SendPropValue::Float(end) },
    ])
}

/// A TF weapon's `m_bResetParity` as a client creates the weapon with it, for a full update: one
/// that differs from it, the client takes for a reset (`CTFWeaponBase::OnDataChanged`), and a
/// medigun's charge goes to 0 until it changes. A weapon whose own is the other one then misses
/// its next real reset, whose flip is to what the client has; what a reset changes still comes, as
/// props. None for another entity.
fn reset_parity(state: &ParserState, e: &Entity) -> Option<Vec<SendProp>> {
    let table = state.send_tables.get(usize::from(e.class))?;
    let id = SendPropIdentifier::new("DT_TFWeaponBase", "m_bResetParity");
    let index = super::props::prop_index(table, id)?;
    Some(vec![SendProp { index, identifier: id, value: SendPropValue::Integer(0) }])
}

/// An entity entering with the props that differ from its class's static baseline.
fn enter(state: &ParserState, index: u32, e: &Entity) -> Option<PacketEntity> {
    let table = state.send_tables.get(usize::from(e.class))?;
    let base: BTreeMap<u32, SendProp> =
        state.get_static_baseline(e.class, table).unwrap_or_default().into_iter().map(|p| (p.index, p)).collect();
    let props =
        e.props.values().filter(|p| !base.get(&p.index).is_some_and(|b| same(&b.value, &p.value))).cloned().collect();
    Some(packet_entity(index, e, UpdateType::Enter, props, None))
}

/// Entity `e` at `index` as a message lists it, `delta` the tick of the frame it builds on.
fn packet_entity(
    index: u32,
    e: &Entity,
    update_type: UpdateType,
    props: Vec<SendProp>,
    delta: Option<u32>,
) -> PacketEntity {
    PacketEntity {
        server_class: e.class,
        entity_index: index.into(),
        props,
        in_pvs: update_type != UpdateType::Delete,
        update_type,
        serial_number: e.serial,
        delay: None,
        delta: delta.map(Into::into),
        baseline_index: BaselineIndex::First,
    }
}

/// `net_Tick(tick)` and `msg`, as one message stream.
fn encode(state: &ParserState, msg: &PacketEntitiesMessage, tick: u32) -> Option<BitBuf> {
    let mut w = BitWriter::new();
    w.write_buf(&messages::tick(tick as i32));
    w.write_buf(&super::encode_message(SVC_PACKETENTITIES, msg, state)?);
    Some(w.finish())
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
