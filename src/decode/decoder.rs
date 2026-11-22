//! The decoder: parses a connection's message streams into game state and output lines.

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
use crate::Result;
use crate::net::bits::{BitBuf, BitReader, BitWriter};
use crate::net::messages::{Signon, TYPE_BITS};
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
    /// The rewritten stream, if the decoder rewrote it. `spans` then index into it.
    pub stream: Option<BitBuf>,
}

impl Lifecycle {
    /// The stream to pass on.
    pub fn bits<'a>(&'a self, fed: &'a BitBuf) -> &'a BitBuf {
        self.stream.as_ref().unwrap_or(fed)
    }

    /// `bits` (the stream this came from) cut at the end of its last message. What follows is
    /// padding, which would be read as messages if more were appended.
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

/// Field widths in `svc_GameEvent`: the length, then the event id.
pub(super) const GAMEEVENT_LENGTH_BITS: usize = 11;

pub(super) const EVENT_ID_BITS: usize = 9;

/// Bits in the entry count of `svc_PacketEntities` (`MAX_EDICT_BITS`).
pub(super) const ENTRY_COUNT_BITS: usize = 11;

/// A frame's entities: in full, or as a delta from the frame at a server tick.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Entities {
    Full,
    Delta(u32),
}

/// A stream's own baselines (see [`super::scene::View::baselines`]).
pub(crate) struct Baselines(pub(super) Box<[tf_demo_parser::demo::parser::state::Baseline; 2]>);

pub(crate) struct Decoder {
    pub(super) handler: DemoHandler<Extractor>,
    pub(super) snapshot_hz: f32,
    pub(super) snapshots: SnapshotClock,
    pub(super) parse_errors: u64,
    /// The parser panicked on some input; its state can no longer be trusted.
    pub(super) panicked: bool,
    /// Entities have come in full since this decoder started: until then a delta has no base.
    pub(super) seen_full_update: bool,
    /// What a relay keeps of the stream for its clients (see `scene`).
    pub(super) scene: super::scene::Scene,
    /// The stream comes from a player's game, not from a `SourceTV`.
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
            scene: super::scene::Scene::default(),
            players_own: false,
            out: Vec::new(),
        })
    }

    /// Records the stream's `ServerInfo`. Returns its count of map loads and its map.
    pub(super) fn server_info(&mut self, i: &tf_demo_parser::demo::message::ServerInfoMessage) -> (i32, String) {
        self.players_own = !i.stv;
        if let (true, Some(w)) = (self.players_own, &mut self.scene.world) {
            w.set_player(u32::from(i.player_slot) + 1);
        }
        (i.server_count as i32, i.map.clone())
    }

    /// Whether the stream comes from a player's game (a demo a player recorded), not from a
    /// `SourceTV`. A client sent this stream plays as that player.
    pub(crate) fn players_own(&self) -> bool {
        self.players_own
    }

    /// Continues from `baselines`, taken from another decoder.
    pub(crate) fn set_baselines(&mut self, baselines: &Baselines) {
        self.handler.state_handler.instance_baselines.clone_from(&baselines.0);
        if let Some(w) = &mut self.scene.world {
            w.set_own_baselines();
        }
    }

    /// The server tick of message stream `data` (its `net_Tick`). Changes no decoder state.
    pub(crate) fn frame_tick(&self, data: &[u8]) -> Option<u32> {
        self.find_message(data, MessageType::NetTick, |stream, _| {
            tf_demo_parser::demo::message::NetTickMessage::read(stream).ok().map(|m| u32::from(m.tick))
        })
    }

    /// The entities in message stream `data`, if any: in full, or as a delta from which frame.
    /// Changes no decoder state.
    pub(crate) fn entities(&self, data: &[u8]) -> Option<Entities> {
        entities_in(self.handler.get_parser_state(), data)
    }

    /// Skips to the first message of type `kind` in message stream `data` and returns `read` of it.
    pub(super) fn find_message<T>(
        &self,
        data: &[u8],
        kind: MessageType,
        read: impl FnOnce(&mut BitReadStream<'_, LittleEndian>, &ParserState) -> Option<T>,
    ) -> Option<T> {
        find_message(self.handler.get_parser_state(), data, kind, read)
    }

    /// The server tick of the last frame.
    pub(crate) fn server_tick(&self) -> u32 {
        self.extractor().server_tick
    }

    /// Seconds per server tick.
    pub(crate) fn tick_interval(&self) -> f32 {
        self.extractor().interval()
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
        // For each message parsed, the index of its span in `life.spans`.
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
        // The world applies these after the parser handled the packet: a new class's baseline may
        // come in the same packet as the entities that need it.
        let frames = if self.scene.world.is_some() { entity_frames(&messages) } else { Vec::new() };
        self.scene.tables.note(&messages);
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
        if let Some(w) = &mut self.scene.world {
            let tick = self.handler.borrow_output().server_tick;
            for pe in &frames {
                w.apply(pe, self.handler.get_parser_state(), tick);
            }
        }
        let x = self.handler.borrow_output();
        life.lost_frame |= x.take_lost_frame();
        if life.lost_frame
            && let Some(w) = &mut self.scene.world
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

    /// Keeps the last event that decides whether the round's win panel shows (see `Scene::round`).
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
                self.scene.round = Some(BitReader::slice(data, span.bits.clone()));
            }
        }
    }

    /// Places `instancebaseline` entries as a game client does; rewrites the stream if any moved.
    pub(super) fn place_baselines(
        &mut self,
        life: &mut Lifecycle,
        data: &[u8],
        messages: &mut [Message<'_>],
        spans_of: &[usize],
    ) {
        let moved = self.scene.tables.place(messages);
        if !moved.is_empty() {
            replace_messages(life, data, &moved, spans_of, messages, self.handler.get_parser_state());
        }
    }

    /// The user id and entity of every player in the match and alive.
    pub(crate) fn living_players(&self) -> Vec<(u16, u32)> {
        self.extractor().snapshot().players.iter().filter(|p| p.alive).map(|p| (p.id.0, p.ent)).collect()
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

/// Skips to the first message of type `kind` in message stream `data` and returns `read` of it.
/// `state` is the parser state the stream is read with.
pub(super) fn find_message<T>(
    state: &ParserState,
    data: &[u8],
    kind: MessageType,
    read: impl FnOnce(&mut BitReadStream<'_, LittleEndian>, &ParserState) -> Option<T>,
) -> Option<T> {
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

/// The entities in message stream `data`, if any: in full, or as a delta from which frame.
pub(super) fn entities_in(state: &ParserState, data: &[u8]) -> Option<Entities> {
    find_message(state, data, MessageType::PacketEntities, |stream, state| {
        let pe = tf_demo_parser::demo::message::packetentities::PacketEntitiesMessage::parse(stream, state).ok()?;
        Some(pe.delta.map_or(Entities::Full, |b| Entities::Delta(u32::from(b))))
    })
}

/// Puts `data` with the `moved` string table updates re-encoded in `life`, with its spans.
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

/// `msg` as a message stream that holds only it, with its type `svc` first.
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

/// The `PacketEntities` messages among `messages`.
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
