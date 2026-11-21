//! The decoder: a connection's messages, as the game state and the lines of it.

use bitbuffer::{BitRead, BitReadBuffer, BitReadStream, LittleEndian};
use tf_demo_parser::ParserState;
use tf_demo_parser::demo::data::DemoTick;
use tf_demo_parser::demo::message::generated::SignOnState;
use tf_demo_parser::demo::message::{Message, MessageType};
use tf_demo_parser::demo::packet::Packet;
use tf_demo_parser::demo::packet::message::{MessagePacket, MessagePacketMeta};
use tf_demo_parser::demo::parser::{DemoHandler, Parse};

use super::Tables;
use super::extract::Extractor;
#[cfg(test)]
use super::props_index_for_tests;
pub(crate) use super::tables::Placed;
use super::world::Pose;
use crate::Result;
use crate::net::bits::{BitBuf, BitReader, BitWriter};
use crate::net::messages::{SVC_GAMEEVENT, Signon, TYPE_BITS};
use crate::output::Line;

/// What the session needs to know from the streams it just fed.
#[derive(Clone, Debug, Default)]
pub(crate) struct Lifecycle {
    pub signon: Vec<Signon>,
    pub server_info: Option<(i32, String)>,
    pub tick: Option<i32>,
    pub disconnect: Option<String>,
    /// The entities may have missed a frame: a message failed to decode, or a delta came from a
    /// frame never seen. Later deltas build on it, so only a full update puts them right.
    pub lost_frame: bool,
    /// Entities came as a full update, from no earlier frame.
    pub full_update: bool,
    /// Where each message of the stream is, in order. After a decode error the rest of the
    /// stream is one span of type [`UNPARSED`].
    pub spans: Vec<Span>,
    /// The stream if the decoder rewrote it; `spans` are of it.
    pub stream: Option<BitBuf>,
}

impl Lifecycle {
    /// The stream to pass on.
    pub fn bits<'a>(&'a self, fed: &'a BitBuf) -> &'a BitBuf {
        self.stream.as_ref().unwrap_or(fed)
    }

    /// `bits`, the stream this came from, up to the end of its last message: what follows is
    /// padding, and followed by more it would be read as messages.
    pub fn cut(&self, bits: &BitBuf) -> BitBuf {
        let end = self.spans.last().map_or(0, |s| s.bits.end);
        BitReader::slice(bits.bytes(), 0..end)
    }
}

/// One message in a stream: its type id and its bits.
#[derive(Clone, Debug)]
pub(crate) struct Span {
    pub id: u8,
    pub bits: std::ops::Range<usize>,
}

/// The type of a span that could not be decoded.
pub(crate) const UNPARSED: u8 = u8::MAX;

/// `svc_GameEvent`: its length's bits, then the event id's.
pub(super) const GAMEEVENT_LENGTH_BITS: usize = 11;

pub(super) const EVENT_ID_BITS: usize = 9;

/// `svc_PacketEntities`: its entry count's bits (`MAX_EDICT_BITS`).
pub(super) const ENTRY_COUNT_BITS: usize = 11;

/// A frame's entities: in full, or as a delta from the frame at a server tick.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Entities {
    Full,
    Delta(u32),
}

/// A stream's own baselines (see [`Decoder::baselines`]).
pub(crate) struct Baselines(Box<[tf_demo_parser::demo::parser::state::Baseline; 2]>);

pub(crate) struct Decoder {
    pub(super) handler: DemoHandler<Extractor>,
    pub(super) snapshot_hz: f32,
    pub(super) snapshots: SnapshotClock,
    pub(super) parse_errors: u64,
    /// The parser panicked on some input; its state can no longer be trusted.
    pub(super) panicked: bool,
    /// Entities have come in full since this decoder started: until then a delta has no base.
    pub(super) seen_full_update: bool,
    /// Kept only when asked for: what a full update is written from.
    pub(super) world: Option<super::world::World>,
    /// The string tables, as far as a jump back needs them (see `tables`).
    pub(super) tables: super::tables::Tables,
    /// The last round start, win panel or game over event (see [`Decoder::round_event`]).
    pub(super) round: Option<BitBuf>,
    /// The stream is a player's game's, not a `SourceTV`'s.
    pub(super) players_own: bool,
    pub(super) out: Vec<Line>,
}

impl Decoder {
    pub(crate) fn new(tables: &Tables, snapshot_hz: f32) -> Result<Self> {
        let mut handler = DemoHandler::with_analyser(Extractor::default());
        handler.handle_header(&tables.header);
        handler.handle_packet(Packet::DataTables(tables.datatables.clone()))?;
        Ok(Decoder {
            handler,
            snapshot_hz,
            snapshots: SnapshotClock::default(),
            parse_errors: 0,
            panicked: false,
            seen_full_update: false,
            world: None,
            tables: super::tables::Tables::default(),
            round: None,
            players_own: false,
            out: Vec::new(),
        })
    }

    /// Keeps every entity's props from the next full update on, for [`Decoder::keyframe`].
    pub(crate) fn keep_world(&mut self) {
        self.world.get_or_insert_with(super::world::World::default);
    }

    /// `net_Tick` and every entity in full, of the frame that `next` (the next frame's message
    /// stream, if known) builds on, or the latest when it is a full update or not known, with that
    /// frame's server tick: a client that starts from it, and gets the frames after it, can
    /// follow. None without [`Decoder::keep_world`], before a full update came, after a frame may
    /// have been missed, or when the frame `next` builds on is not kept.
    pub(crate) fn keyframe(&self, next: Option<&[u8]>) -> Option<(u32, BitBuf)> {
        let base = next.and_then(|d| self.delta_base(d));
        self.world.as_ref()?.full_update(self.handler.get_parser_state(), base, None)
    }

    /// [`Decoder::keyframe`] with every running round timer held where it is, for clients whose
    /// clock runs on while no frames come (a room paused).
    pub(crate) fn keyframe_held(&self, next: Option<&[u8]>) -> Option<(u32, BitBuf)> {
        let base = next.and_then(|d| self.delta_base(d));
        self.world.as_ref()?.full_update(self.handler.get_parser_state(), base, Some(self.tick_interval()))
    }

    /// The entities of the last frame decoded as a delta from frame `from`, the one message: for a
    /// client that acked `from`. None when either is not kept, or it does not encode.
    pub(crate) fn entities_from(&self, from: u32) -> Option<BitBuf> {
        self.world.as_ref()?.delta_message(self.handler.get_parser_state(), from, self.server_tick())
    }

    /// A demo's string tables as its recording started (`data`, its packet of them), as a message
    /// stream of what they have over this decoder's (see [`super::tables::Tables::up_to`]); none when
    /// nothing, or it does not parse.
    pub(crate) fn snapshot_stream(&self, data: &[u8]) -> Option<BitBuf> {
        use tf_demo_parser::demo::packet::stringtable::StringTable;
        let mut s = BitReadStream::new(BitReadBuffer::new(data, LittleEndian));
        let count: u8 = s.read().ok()?;
        let snapshot: Vec<StringTable<'_>> = s.read_sized(usize::from(count)).ok()?;
        let msgs = self.tables.up_to(&snapshot, self.handler.get_parser_state());
        if msgs.is_empty() {
            return None;
        }
        let mut w = BitWriter::new();
        for m in &msgs {
            w.write_buf(m);
        }
        Some(w.finish())
    }

    /// Takes the stream's `ServerInfo`: its count of map loads and map.
    pub(super) fn server_info(&mut self, i: &tf_demo_parser::demo::message::ServerInfoMessage) -> (i32, String) {
        self.players_own = !i.stv;
        if let (true, Some(w)) = (self.players_own, &mut self.world) {
            w.set_player(u32::from(i.player_slot) + 1);
        }
        (i.server_count as i32, i.map.clone())
    }

    /// Whether the stream is a player's game's (a demo a player recorded), not a `SourceTV`'s: a
    /// client given it is that player.
    pub(crate) fn players_own(&self) -> bool {
        self.players_own
    }

    #[cfg(test)]
    pub(crate) fn player_for_tests(&self) -> Option<u32> {
        self.world.as_ref()?.player()
    }

    /// Whether the stream's entity messages are for no client as they are (see `world`).
    pub(crate) fn own_baselines(&self) -> bool {
        self.world.as_ref().is_some_and(super::world::World::own_baselines)
    }

    /// The stream's own baselines, for a decoder that goes on from here.
    pub(crate) fn baselines(&self) -> Option<Baselines> {
        self.own_baselines().then(|| Baselines(Box::new(self.handler.get_parser_state().instance_baselines.clone())))
    }

    /// Goes on with `baselines`, another decoder's there.
    pub(crate) fn set_baselines(&mut self, baselines: &Baselines) {
        self.handler.state_handler.instance_baselines.clone_from(&baselines.0);
        if let Some(w) = &mut self.world {
            w.set_own_baselines();
        }
    }

    /// The frames to send after a keyframe of frame `key`, from `recent` (frames played, by server
    /// tick): those after it, each as played if the client still has the frame it builds on, else
    /// (from a live server, a frame a few ticks back) rewritten as a delta from `key`, or failing
    /// that the last it was sent. A game client keeps only frames with entities, and drops those
    /// older than the base of each delta it takes.
    pub(crate) fn catch_up(&self, key: u32, recent: &[(u32, BitBuf)]) -> Vec<BitBuf> {
        let Some(world) = &self.world else { return Vec::new() };
        let state = self.handler.get_parser_state();
        let mut have = vec![key];
        let mut out = Vec::new();
        let own = world.own_baselines();
        for (tick, bits) in recent.iter().filter(|(t, _)| *t > key) {
            match self.entities(bits.bytes()) {
                // No entities in it: nothing builds on it.
                None => out.push(bits.clone()),
                Some(Entities::Delta(base)) if have.contains(&base) && !own => {
                    out.push(bits.clone());
                    have.retain(|t| *t >= base);
                }
                _ => {
                    // From the keyframe's frame while the client has it: the stream's next frames
                    // may still build on that one.
                    let from = if have.contains(&key) { key } else { have.last().copied().unwrap_or(key) };
                    match world.delta(state, from, *tick) {
                        Some(d) => {
                            out.push(d);
                            have.retain(|t| *t >= from);
                        }
                        None if own => {}
                        None => out.push(bits.clone()),
                    }
                }
            }
            if world.has(*tick) && !have.contains(tick) {
                have.push(*tick);
            }
        }
        out
    }

    /// The server tick message stream `data` is of (its `net_Tick`), read, not decoded into
    /// anything.
    pub(crate) fn frame_tick(&self, data: &[u8]) -> Option<u32> {
        self.find_message(data, MessageType::NetTick, |stream, _| {
            tf_demo_parser::demo::message::NetTickMessage::read(stream).ok().map(|m| u32::from(m.tick))
        })
    }

    /// The entities in message stream `data`, if any: in full, or as a delta from which frame;
    /// read, not decoded into anything.
    pub(crate) fn entities(&self, data: &[u8]) -> Option<Entities> {
        self.find_message(data, MessageType::PacketEntities, |stream, state| {
            let pe = tf_demo_parser::demo::message::packetentities::PacketEntitiesMessage::parse(stream, state).ok()?;
            Some(pe.delta.map_or(Entities::Full, |b| Entities::Delta(u32::from(b))))
        })
    }

    /// The server tick of the frame the entities in message stream `data` build on, if they are a
    /// delta; it is read, not decoded into anything.
    pub(super) fn delta_base(&self, data: &[u8]) -> Option<u32> {
        self.find_message(data, MessageType::PacketEntities, |stream, state| {
            let pe = tf_demo_parser::demo::message::packetentities::PacketEntitiesMessage::parse(stream, state).ok()?;
            pe.delta.map(u32::from)
        })
    }

    /// `read` of the first message of type `kind` in message stream `data`, the ones before it
    /// skipped.
    pub(super) fn find_message<T>(
        &self,
        data: &[u8],
        kind: MessageType,
        read: impl FnOnce(&mut BitReadStream<'_, LittleEndian>, &ParserState) -> Option<T>,
    ) -> Option<T> {
        let state = self.handler.get_parser_state();
        let mut stream = BitReadStream::new(BitReadBuffer::new(data, LittleEndian));
        while stream.bits_left() > TYPE_BITS {
            let t = MessageType::read(&mut stream).ok()?;
            if t == kind {
                return read(&mut stream, state);
            }
            Message::skip_type(t, &mut stream, state).ok()?;
        }
        None
    }

    /// Whether frame `tick` is kept.
    #[cfg(test)]
    pub(crate) fn has_frame_for_tests(&self, tick: u32) -> bool {
        self.world.as_ref().is_some_and(|w| w.has(tick))
    }

    /// A full update of frame `at`, kept.
    #[cfg(test)]
    pub(crate) fn keyframe_at_for_tests(&self, at: u32) -> Option<BitBuf> {
        Some(self.world.as_ref()?.full_update(self.handler.get_parser_state(), Some(at), None)?.1)
    }

    /// Frame `to` as a delta from frame `from`, both kept.
    #[cfg(test)]
    pub(crate) fn delta_for_tests(&self, from: u32, to: u32) -> Option<BitBuf> {
        self.world.as_ref()?.delta(self.handler.get_parser_state(), from, to)
    }

    /// The server tick of the last frame.
    pub(crate) fn server_tick(&self) -> u32 {
        self.extractor().server_tick
    }

    /// Seconds per server tick.
    pub(crate) fn tick_interval(&self) -> f32 {
        self.extractor().interval()
    }

    /// Every entity as the world has it.
    #[cfg(test)]
    pub(crate) fn world_dump(&self) -> std::collections::BTreeMap<u32, super::world::Dumped> {
        self.world.as_ref().map_or_default(super::world::World::dump)
    }

    /// [`Decoder::world_dump`] to compare two decoders, one started from a full update: without
    /// the weapons' `m_bResetParity`, which a full update gives as a client starts with it, nor a
    /// player's own demo's player's origin for the other players, which it gives as their own.
    #[cfg(test)]
    pub(crate) fn world_compared(&self) -> std::collections::BTreeMap<u32, super::world::Dumped> {
        let state = self.handler.get_parser_state();
        let parity = tf_demo_parser::demo::sendprop::SendPropIdentifier::new("DT_TFWeaponBase", "m_bResetParity");
        let mut dump = self.world_dump();
        for (class, _, props) in dump.values_mut() {
            if let Some(at) = props_index_for_tests(state, *class, parity) {
                props.retain(|(i, _)| *i != at);
            }
        }
        if let Some((class, _, props)) = self.player_for_tests().and_then(|p| dump.get_mut(&p)) {
            for name in ["m_vecOrigin", "m_vecOrigin[2]"] {
                let id = tf_demo_parser::demo::sendprop::SendPropIdentifier::new("DT_TFNonLocalPlayerExclusive", name);
                if let Some(at) = props_index_for_tests(state, *class, id) {
                    props.retain(|(i, _)| *i != at);
                }
            }
        }
        dump
    }

    /// The entities the keyframe would hold.
    #[cfg(test)]
    pub(crate) fn world_len(&self) -> usize {
        self.world.as_ref().map_or(0, super::world::World::len)
    }

    /// Feeds one message stream (bit packed from bit 0) and returns its lifecycle messages.
    ///
    /// The stream is parsed with its exact bit length: a datagram ends in up to five
    /// junk padding bits, and once realigned to bit 0 a byte-granular reader would
    /// see enough bits to decode them as a bogus message.
    pub(crate) fn feed(&mut self, data: &[u8], nbits: usize) -> Lifecycle {
        if self.panicked {
            return Lifecycle::default();
        }
        // The parser is a dependency fed with server data: contain a panic in it to
        // this decoder (the session then reconnects) instead of the whole process.
        let fed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.feed_inner(data, nbits)));
        fed.unwrap_or_else(|_| {
            self.panicked = true;
            self.parse_errors += 1;
            Lifecycle::default()
        })
    }

    pub(super) fn feed_inner(&mut self, data: &[u8], nbits: usize) -> Lifecycle {
        let mut life = Lifecycle::default();
        if nbits < TYPE_BITS {
            return life;
        }
        let nbytes = nbits.div_ceil(8);
        let mut outer = BitReadStream::new(BitReadBuffer::new(&data[..nbytes], LittleEndian));
        let Ok(mut stream) = outer.read_bits(nbits) else { return life };
        let state = self.handler.get_parser_state();
        let mut messages = Vec::new();
        // The span of each message parsed.
        let mut parsed_spans = Vec::new();
        while stream.bits_left() > TYPE_BITS {
            let start = stream.pos();
            let id: u8 = match stream.read_sized(TYPE_BITS) {
                Ok(id) => id,
                Err(_) => break,
            };
            if u32::from(id) == crate::net::messages::NET_DISCONNECT {
                // net_Disconnect never appears in demos, so the parser has no type for it.
                let reason: String = stream.read().unwrap_or_default();
                life.disconnect = Some(reason);
                break;
            }
            let _ = stream.set_pos(start);
            let parsed = MessageType::read(&mut stream).map_err(tf_demo_parser::ParseError::from).and_then(|t| {
                // The server may send a delta in a datagram before the end of the full update it
                // builds on, still coming reliably: a game client ignores one until it has entities
                // in full, and so does this.
                let early_delta = t == MessageType::PacketEntities && !self.seen_full_update && is_delta(&stream);
                if !early_delta && state.should_parse_message(t) && t != MessageType::Empty {
                    Message::from_type(t, &mut stream, state).map(Some)
                } else {
                    Message::skip_type(t, &mut stream, state).map(|()| None)
                }
            });
            if parsed.is_ok() {
                life.spans.push(Span { id, bits: start..stream.pos() });
            }
            match parsed {
                Ok(Some(m)) => {
                    messages.push(m);
                    parsed_spans.push(life.spans.len() - 1);
                }
                Ok(None) => {}
                Err(e) => {
                    life.spans.push(Span { id: UNPARSED, bits: start..nbits });
                    if super::extract::debug("errors") {
                        let tick = self.handler.borrow_output().server_tick;
                        eprintln!("decode error at tick {tick}, bit {start} of {nbits}, message id {id}: {e}");
                    }
                    self.parse_errors += 1;
                    life.lost_frame = true;
                    break;
                }
            }
        }
        self.place_baselines(&mut life, &data[..nbytes], &mut messages, &parsed_spans);
        // Applied once the packet is: a new class's baseline may come in the same packet, before
        // the entities that need it.
        let frames = if self.world.is_some() { entity_frames(&messages) } else { Vec::new() };
        self.tables.note(&messages);
        let bytes = life.stream.as_ref().map_or(&data[..nbytes], BitBuf::bytes);
        self.note_round(&messages, &parsed_spans, &life.spans, bytes);
        let mut has_entities = false;
        for msg in &messages {
            match msg {
                Message::SignOnState(s) => life.signon.push(signon(&s.state)),
                Message::ServerInfo(i) => life.server_info = Some(self.server_info(i)),
                Message::NetTick(t) => life.tick = Some(u32::from(t.tick) as i32),
                Message::PacketEntities(pe) => {
                    has_entities = true;
                    life.full_update |= pe.delta.is_none();
                    self.seen_full_update |= pe.delta.is_none();
                }
                _ => {}
            }
        }
        let tick = self.extractor().server_tick;
        let packet =
            Packet::Message(MessagePacket { tick: DemoTick::from(tick), messages, meta: MessagePacketMeta::default() });
        if self.handler.handle_packet(packet).is_err() {
            self.parse_errors += 1;
            life.lost_frame = true;
        }
        if let Some(w) = &mut self.world {
            let tick = self.handler.borrow_output().server_tick;
            for pe in &frames {
                w.apply(pe, self.handler.get_parser_state(), tick);
            }
        }
        let x = self.handler.borrow_output();
        life.lost_frame |= x.take_lost_frame();
        if life.lost_frame
            && let Some(w) = &mut self.world
        {
            w.lost_frame();
        }
        x.announce();
        let drained = x.drain();
        let now = x.server_tick;
        let every = snapshot_every(self.snapshot_hz, x.interval());
        self.out.extend(drained);
        if has_entities && self.snapshots.due(now, every) {
            let snap = self.extractor().snapshot();
            self.out.push(Line::Snapshot(snap));
        }
        life
    }

    /// Keeps the last event that decides whether the round's win panel shows (see `round`).
    pub(super) fn note_round(&mut self, messages: &[Message<'_>], spans_of: &[usize], spans: &[Span], data: &[u8]) {
        use tf_demo_parser::demo::gameevent_gen::GameEventType as E;
        for (m, &i) in messages.iter().zip(spans_of) {
            if let Message::GameEvent(e) = m
                && matches!(
                    e.event_type,
                    E::TeamPlayRoundStart
                        | E::TeamPlayWinPanel
                        | E::TeamPlayGameOver
                        | E::TfGameOver
                        | E::ArenaWinPanel
                )
                && let Some(span) = spans.get(i)
            {
                self.round = Some(BitReader::slice(data, span.bits.clone()));
            }
        }
    }

    /// The last round start, win panel or game over event: what a client that skipped there
    /// (a jump, or one just in) is sent after a full update, so its HUD shows the round's end, or
    /// hides it, as the game does. Before the demo's first, a round start written here: one jumped
    /// back there from a win panel has it hidden.
    pub(crate) fn round_event(&self) -> Option<BitBuf> {
        use tf_demo_parser::demo::gameevent_gen::{GameEvent, TeamPlayRoundStartEvent};
        self.round
            .clone()
            .or_else(|| self.game_event(GameEvent::TeamPlayRoundStart(TeamPlayRoundStartEvent { full_reset: false })))
    }

    /// Sets the last round event, for a decoder that starts from a checkpoint.
    pub(crate) fn set_round_event(&mut self, event: Option<BitBuf>) {
        self.round = event;
    }

    /// The entity of user id `user`, while they are in the match.
    pub(crate) fn player_entity(&self, user: u16) -> Option<u32> {
        Some(self.extractor().snapshot().players.iter().find(|p| p.id.0 == user)?.ent)
    }

    /// The user id and entity of every player in the match and alive.
    pub(crate) fn living_players(&self) -> Vec<(u16, u32)> {
        self.extractor().snapshot().players.iter().filter(|p| p.alive).map(|p| (p.id.0, p.ent)).collect()
    }

    /// `teamplay_update_timer`: the HUD's timers redraw, as from a time the clock went back past
    /// (a jump back, playback going on after a pause), and nothing else does (`ResetHUD` would
    /// blank the medic's charge meter until the charge changes).
    pub(crate) fn update_timer(&self) -> Option<BitBuf> {
        use tf_demo_parser::demo::gameevent_gen::{GameEvent, TeamPlayUpdateTimerEvent};
        self.game_event(GameEvent::TeamPlayUpdateTimer(TeamPlayUpdateTimerEvent {}))
    }

    /// `hltv_chase` into the eyes of user id `player`, for clients on `spec_autodirector 1`.
    pub(crate) fn hltv_chase(&self, player: u16) -> Option<BitBuf> {
        use tf_demo_parser::demo::gameevent_gen::{GameEvent, HLTVChaseEvent};
        let ent = self.player_entity(player)?;
        self.game_event(GameEvent::HLTVChase(HLTVChaseEvent {
            target_1: u16::try_from(ent).ok()?,
            target_2: 0,
            distance: 96,
            theta: 0,
            phi: 0,
            inertia: 0,
            in_eye: 1,
        }))
    }

    /// `event` as an `svc_GameEvent`, if the demo's event list has its kind.
    pub(super) fn game_event(&self, event: tf_demo_parser::demo::gameevent_gen::GameEvent) -> Option<BitBuf> {
        use tf_demo_parser::demo::message::gameevent::GameEventMessage;
        let state = self.handler.get_parser_state();
        let event_type = event.event_type();
        let def = state.event_definitions.iter().find(|d| d.event_type == event_type)?;
        encode_message(SVC_GAMEEVENT, &GameEventMessage { event_type_id: def.id, event_type, event }, state)
    }

    /// `hltv_fixed` at `pos` looking along `pitch` and `yaw` (degrees), for the same clients.
    /// The game takes the coordinates and angles as signed, and no field of view but its own.
    #[allow(clippy::cast_sign_loss)]
    pub(crate) fn hltv_fixed(&self, pos: [i32; 3], pitch: i16, yaw: i16) -> Option<BitBuf> {
        use tf_demo_parser::demo::gameevent_gen::{GameEvent, HLTVFixedEvent};
        self.game_event(GameEvent::HLTVFixed(HLTVFixedEvent {
            pos_x: pos[0] as u32,
            pos_y: pos[1] as u32,
            pos_z: pos[2] as u32,
            theta: pitch as u16,
            phi: yaw as u16,
            offset: 0,
            fov: 90.0,
            target: 0,
        }))
    }

    /// `hltv_cameraman`: the same clients see what player `index` does.
    pub(crate) fn hltv_cameraman(&self, index: u32) -> Option<BitBuf> {
        use tf_demo_parser::demo::gameevent_gen::{GameEvent, HLTVCameramanEvent};
        self.game_event(GameEvent::HLTVCameraman(HLTVCameramanEvent { index: u16::try_from(index).ok()? }))
    }

    /// The clients' camera from the next frame on: at `pose`, or (none) the director's again.
    pub(crate) fn set_shot(&mut self, pose: Option<Pose>) {
        let (model, slots) = (self.tables.studio(), self.extractor().max_players());
        if let Some(w) = &mut self.world {
            w.set_shot(pose, model, slots);
        }
    }

    /// The same without a frame, for clients who get none (a room paused). Whether that changed
    /// what they are to have: each needs a full update then.
    pub(crate) fn reshoot(&mut self) -> bool {
        let state = self.handler.get_parser_state();
        self.world.as_mut().is_some_and(|w| w.reshoot(state))
    }

    /// Whether the last frame changed what is the relay's own of the camera: its entities are for
    /// no client as they are then.
    pub(crate) fn shot_touched(&self) -> bool {
        self.world.as_ref().is_some_and(super::world::World::shot_touched)
    }

    /// The player who watches for the clients where the relay has the camera, once there.
    pub(crate) fn cameraman(&self) -> Option<u32> {
        self.world.as_ref()?.cameraman()
    }

    #[cfg(test)]
    pub(crate) fn rig_for_tests(&self) -> Option<(u32, u32)> {
        self.world.as_ref()?.rig()
    }

    /// The player slots nobody is in, highest first: not the cameraman's.
    pub(crate) fn spare_slots(&self) -> Vec<u8> {
        let man = self.cameraman();
        self.extractor().spare_slots().into_iter().filter(|s| man != Some(u32::from(*s) + 1)).collect()
    }

    /// Whether the message at `span` is a director camera cue (`hltv_chase`, `hltv_fixed`, …).
    pub(crate) fn is_director_cue(&self, data: &[u8], span: &Span) -> bool {
        use tf_demo_parser::demo::gameevent_gen::GameEventType as E;
        if u32::from(span.id) != SVC_GAMEEVENT {
            return false;
        }
        let at = span.bits.start + TYPE_BITS + GAMEEVENT_LENGTH_BITS;
        let Ok(id) = BitReader::at(data, at).read_bits(EVENT_ID_BITS) else { return false };
        let defs = &self.handler.get_parser_state().event_definitions;
        defs.get(id as usize).is_some_and(|d| {
            matches!(
                d.event_type,
                E::HLTVChase | E::HLTVFixed | E::HLTVCameraman | E::HLTVRankCamera | E::HLTVRankEntity
            )
        })
    }

    /// How many `userinfo` and `instancebaseline` entries were rewritten so far (see `tables`).
    pub(crate) fn tables_rewritten(&self) -> u32 {
        self.tables.rewritten()
    }

    /// Adds `add` (BLU's, RED's) to the teams' scores in what is written for a client (see
    /// `World::set_score_add`); whether that changed.
    pub(crate) fn set_score_add(&mut self, add: [i64; 2]) -> bool {
        self.world.as_mut().is_some_and(|w| w.set_score_add(add))
    }

    /// Whether the last frame set the score of a team that has something added.
    pub(crate) fn score_touched(&self) -> bool {
        self.world.as_ref().is_some_and(super::world::World::score_touched)
    }

    /// A message naming the players in the match as `names` has them (see [`Tables::named`]).
    pub(crate) fn named(&self, names: &std::collections::BTreeMap<String, Option<String>>) -> Option<BitBuf> {
        self.tables.named(names, self.handler.get_parser_state())
    }

    /// Messages that set a client with the string tables of `later` (a decoder further on in the
    /// same demo) back to this one's, as far as they matter; None if they cannot be written.
    pub(crate) fn set_tables_back(&self, later: &Decoder) -> Option<Vec<BitBuf>> {
        self.tables.set_back(&later.tables, self.handler.get_parser_state())
    }

    /// Places `instancebaseline` entries as a game client does; rewrites the stream if any moved.
    pub(super) fn place_baselines(
        &mut self,
        life: &mut Lifecycle,
        data: &[u8],
        messages: &mut [Message<'_>],
        spans_of: &[usize],
    ) {
        let moved = self.tables.place(messages);
        if !moved.is_empty() {
            replace_messages(life, data, &moved, spans_of, messages, self.handler.get_parser_state());
        }
    }

    /// Baseline placement.
    pub(crate) fn placed(&self) -> Placed {
        self.tables.placed()
    }

    /// Placement for streams already placed.
    pub(crate) fn set_placed(&mut self, placed: Placed) {
        self.tables.set_placed(placed);
    }

    #[cfg(test)]
    pub(crate) fn table_names_for_tests(&self, name: &str) -> Option<std::collections::BTreeMap<u16, Option<String>>> {
        self.tables.names_for_tests(name)
    }

    #[cfg(test)]
    pub(crate) fn parser_state_for_tests(&self) -> &ParserState {
        self.handler.get_parser_state()
    }

    pub(crate) fn extractor(&self) -> &Extractor {
        self.handler.borrow_output()
    }

    /// Takes the events and snapshots produced so far.
    pub(crate) fn take_output(&mut self) -> Vec<Line> {
        std::mem::take(&mut self.out)
    }

    pub(crate) fn parse_errors(&self) -> u64 {
        self.parse_errors
    }

    pub(crate) fn panicked(&self) -> bool {
        self.panicked
    }
}

/// `data` with the `moved` updates re-encoded.
pub(super) fn replace_messages(
    life: &mut Lifecycle,
    data: &[u8],
    moved: &[usize],
    spans_of: &[usize],
    messages: &[Message<'_>],
    state: &ParserState,
) {
    let mut w = BitWriter::new();
    let mut spans = Vec::with_capacity(life.spans.len());
    for (n, span) in life.spans.iter().enumerate() {
        let encoded = moved.iter().find(|&&k| spans_of[k] == n).and_then(|&k| match &messages[k] {
            Message::UpdateStringTable(u) => encode_message(crate::net::messages::SVC_UPDATESTRINGTABLE, u, state),
            _ => None,
        });
        let msg = encoded.unwrap_or_else(|| BitReader::slice(data, span.bits.clone()));
        let start = w.nbits();
        w.write_buf(&msg);
        spans.push(Span { id: span.id, bits: start..w.nbits() });
    }
    life.spans = spans;
    life.stream = Some(w.finish());
}

/// `msg` as a message stream of it alone, its type `svc` first.
pub(super) fn encode_message(
    svc: u32,
    msg: &impl tf_demo_parser::demo::parser::Encode,
    state: &ParserState,
) -> Option<BitBuf> {
    let mut bytes = Vec::new();
    let nbits = {
        let mut s = bitbuffer::BitWriteStream::new(&mut bytes, LittleEndian);
        msg.encode(&mut s, state).ok()?;
        s.bit_len()
    };
    let mut w = BitWriter::new();
    w.write_bits(svc, TYPE_BITS);
    w.write_range(&bytes, 0..nbits);
    Some(w.finish())
}

/// The entities messages among `messages`.
pub(super) fn entity_frames(
    messages: &[Message<'_>],
) -> Vec<tf_demo_parser::demo::message::packetentities::PacketEntitiesMessage> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::PacketEntities(pe) => Some(pe.clone()),
            _ => None,
        })
        .collect()
}

/// Whether the `svc_PacketEntities` at `stream`, just past its type, is a delta: after its entry
/// count comes whether a base tick follows.
pub(super) fn is_delta(stream: &BitReadStream<'_, LittleEndian>) -> bool {
    let mut peek = stream.clone();
    peek.skip_bits(ENTRY_COUNT_BITS).is_ok() && peek.read_bool().unwrap_or(false)
}

pub(super) fn signon(s: &SignOnState) -> Signon {
    match s {
        SignOnState::None => Signon::None,
        SignOnState::Challenge => Signon::Challenge,
        SignOnState::Connected => Signon::Connected,
        SignOnState::New => Signon::New,
        SignOnState::PreSpawn => Signon::PreSpawn,
        SignOnState::Spawn => Signon::Spawn,
        SignOnState::Full => Signon::Full,
        SignOnState::ChangeLevel => Signon::ChangeLevel,
    }
}

/// Server ticks between snapshots for a rate cap, at `interval` seconds per tick; 0 disables
/// snapshots. Rounded up, so the rate stays at or under `hz`; the tolerance, a hundred-thousandth
/// of the ratio, keeps a rate that divides the tick rate (64 Hz on 128 ticks) from rounding up
/// over float error.
pub(super) fn snapshot_every(hz: f32, interval: f32) -> u32 {
    if hz <= 0.0 { 0 } else { (1.0 / (f64::from(interval) * f64::from(hz)) * (1.0 - 1e-5)).ceil().max(1.0) as u32 }
}

/// When snapshots are due. Each is due `every` ticks after the one before it was due, not after
/// the frame it went out with: the STV sends frames only every few ticks (`tv_snapshotrate`), and
/// counting from a late frame would skip the next one (frames every 3 ticks at `every` 4 would
/// give a snapshot every 6).
#[derive(Default)]
pub(super) struct SnapshotClock {
    /// The tick of the last snapshot and the tick the next is due.
    pub(super) last: Option<(u32, u32)>,
}

impl SnapshotClock {
    /// Whether a frame at server tick `now` gets a snapshot. The tick restarts at a map load, so
    /// a tick before the last snapshot is due too; one a whole interval late (a stall) starts
    /// the count again from `now`.
    pub(super) fn due(&mut self, now: u32, every: u32) -> bool {
        if every == 0 {
            return false;
        }
        let next = match self.last {
            Some((last, next)) if now >= last => {
                if now < next {
                    return false;
                }
                if now - next < every { next } else { now }
            }
            // The first snapshot, or the tick restarted.
            _ => now,
        };
        self.last = Some((now, next.saturating_add(every)));
        true
    }
}
