//! Feeds network message streams into tf-demo-parser.
//!
//! A `SourceTV` client gets no send tables over the wire (the server expects the
//! client to build them from its own game DLL, checked by the `SendTable` CRC), so
//! the tables come from any demo recorded on the same server build. Each network
//! message stream is then wrapped as a demo `Message` packet and handed to the
//! same `DemoHandler` a demo file would use.

mod crc;
mod detail;
mod extract;
mod modes;
mod props;
mod tables;
mod world;

use crate::net::messages::{Signon, TYPE_BITS};
use crate::output::Line;
use anyhow::{Context, Result, bail};
use bitbuffer::{BitRead, BitReadBuffer, BitReadStream, LittleEndian};
use extract::Extractor;
use tf_demo_parser::demo::data::DemoTick;
use tf_demo_parser::demo::header::Header;
use tf_demo_parser::demo::message::generated::SignOnState;
use tf_demo_parser::demo::message::{Message, MessageType};
use tf_demo_parser::demo::packet::Packet;
use tf_demo_parser::demo::packet::datatable::DataTablePacket;
use tf_demo_parser::demo::packet::message::{MessagePacket, MessagePacketMeta};
use tf_demo_parser::demo::parser::DemoHandler;
use tf_demo_parser::demo::parser::Parse;
use tf_demo_parser::demo::parser::RawPacketStream;

/// The entity class tables of one server build, taken from a demo of that build.
pub struct Tables {
    header: Header,
    datatables: DataTablePacket,
    crc: u32,
    /// The data tables packet's bytes, for a demo written with them.
    raw: Vec<u8>,
}

impl Tables {
    /// Reads the tables from a demo file (any STV or POV demo of the build).
    pub fn from_demo(path: &std::path::Path) -> Result<Tables> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_demo_bytes(&bytes)
    }

    /// Reads the tables from demo bytes; everything after the data tables is ignored.
    pub fn from_demo_bytes(bytes: &[u8]) -> Result<Tables> {
        let mut stream = BitReadStream::new(BitReadBuffer::new(bytes, LittleEndian));
        let header = Header::read(&mut stream)?;
        let handler = DemoHandler::with_analyser(tf_demo_parser::demo::parser::NullHandler);
        let mut packets = RawPacketStream::new(stream);
        while let Some(packet) = packets.next(handler.get_parser_state())? {
            if let Packet::DataTables(dt) = packet {
                let crc = crc::sendtable_crc(&dt)?;
                return Ok(Tables { header, datatables: dt, crc, raw: raw_datatables(bytes)? });
            }
        }
        bail!("no data tables in demo")
    }

    /// The server class names (`CTFPlayer`, …), in class id order.
    pub fn class_names(&self) -> impl Iterator<Item = &str> {
        self.datatables.server_classes.iter().map(|c| c.name.as_str())
    }

    /// The number of send tables.
    pub fn send_table_count(&self) -> usize {
        self.datatables.tables.len()
    }

    /// The data tables packet as a demo of the build has it.
    pub fn datatables_bytes(&self) -> &[u8] {
        &self.raw
    }

    /// The `SendTable` CRC of the build, as the server checks it at signon.
    pub fn sendtable_crc(&self) -> u32 {
        self.crc
    }
}

/// The data of the first data tables packet of a demo.
fn raw_datatables(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut r = crate::demo::Reader::new(std::io::Cursor::new(bytes))?;
    loop {
        match r.next_packet()? {
            crate::demo::Next::Packet(crate::demo::Packet::DataTables { data, .. }) => return Ok(data),
            crate::demo::Next::Packet(_) => {}
            _ => bail!("no data tables in demo"),
        }
    }
}

/// What the session needs to know from the streams it just fed.
#[derive(Default, Debug)]
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
}

/// One message in a stream: its type id and its bits.
#[derive(Debug, Clone)]
pub(crate) struct Span {
    pub id: u8,
    pub bits: std::ops::Range<usize>,
}

/// The type of a span that could not be decoded.
pub(crate) const UNPARSED: u8 = u8::MAX;

pub(crate) struct Decoder {
    handler: DemoHandler<Extractor>,
    snapshot_hz: f32,
    snapshots: SnapshotClock,
    parse_errors: u64,
    /// The parser panicked on some input; its state can no longer be trusted.
    panicked: bool,
    /// Entities have come in full since this decoder started: until then a delta has no base.
    seen_full_update: bool,
    /// Kept only when asked for: what a full update is written from.
    world: Option<world::World>,
    /// The string tables, as far as a jump back needs them (see `tables`).
    tables: tables::Tables,
    /// The last round start, win panel or game over event (see [`Decoder::round_event`]).
    round: Option<crate::net::bits::BitBuf>,
    out: Vec<Line>,
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
            tables: tables::Tables::default(),
            round: None,
            out: Vec::new(),
        })
    }

    /// Keeps every entity's props from the next full update on, for [`Decoder::keyframe`].
    pub(crate) fn keep_world(&mut self) {
        self.world.get_or_insert_with(world::World::default);
    }

    /// `net_Tick` and every entity in full, of the frame that `next` (the next frame's message
    /// stream, if known) builds on, or else the latest, with that frame's server tick: a client
    /// that starts from it, and gets the frames after it, can follow. None without
    /// [`Decoder::keep_world`], before a full update came, or after a frame may have been missed.
    pub(crate) fn keyframe(&self, next: Option<&[u8]>) -> Option<(u32, crate::net::bits::BitBuf)> {
        let base = next.and_then(|d| self.delta_base(d));
        self.world.as_ref()?.full_update(self.handler.get_parser_state(), base)
    }

    /// The frames to send after a keyframe of frame `key`, from `recent` (frames played, by server
    /// tick): those after it, each as played if the client has the frame it builds on, else (from
    /// a live server, a frame a few ticks back) rewritten as a delta from `key`.
    pub(crate) fn catch_up(
        &self,
        key: u32,
        recent: &[(u32, crate::net::bits::BitBuf)],
    ) -> Vec<crate::net::bits::BitBuf> {
        let Some(world) = &self.world else { return Vec::new() };
        let state = self.handler.get_parser_state();
        let mut have = vec![key];
        let mut out = Vec::new();
        for (tick, bits) in recent.iter().filter(|(t, _)| *t > key) {
            match self.delta_base(bits.bytes()) {
                Some(base) if have.contains(&base) => out.push(bits.clone()),
                // From the keyframe's frame: a client drops the frames older than a delta's base,
                // and the stream's next frames may still build on that one.
                _ => match world.delta(state, key, *tick) {
                    Some(d) => out.push(d),
                    // No entities in it: nothing builds on it.
                    None => out.push(bits.clone()),
                },
            }
            // A client keeps only frames with entities.
            if world.has(*tick) {
                have.push(*tick);
            }
        }
        out
    }

    /// The server tick message stream `data` is of (its `net_Tick`), read, not decoded into anything.
    pub(crate) fn frame_tick(&self, data: &[u8]) -> Option<u32> {
        let state = self.handler.get_parser_state();
        let mut stream = BitReadStream::new(BitReadBuffer::new(data, LittleEndian));
        while stream.bits_left() > TYPE_BITS {
            let t = MessageType::read(&mut stream).ok()?;
            if t == MessageType::NetTick {
                return tf_demo_parser::demo::message::NetTickMessage::read(&mut stream)
                    .ok()
                    .map(|m| u32::from(m.tick));
            }
            Message::skip_type(t, &mut stream, state).ok()?;
        }
        None
    }

    /// The server tick of the frame the entities in message stream `data` build on, if they are a
    /// delta; it is read, not decoded into anything.
    fn delta_base(&self, data: &[u8]) -> Option<u32> {
        let state = self.handler.get_parser_state();
        let mut stream = BitReadStream::new(BitReadBuffer::new(data, LittleEndian));
        while stream.bits_left() > TYPE_BITS {
            let t = MessageType::read(&mut stream).ok()?;
            if t == MessageType::PacketEntities {
                let pe =
                    tf_demo_parser::demo::message::packetentities::PacketEntitiesMessage::parse(&mut stream, state)
                        .ok()?;
                return pe.delta.map(u32::from);
            }
            Message::skip_type(t, &mut stream, state).ok()?;
        }
        None
    }

    /// The server tick of the last frame.
    pub(crate) fn server_tick(&self) -> u32 {
        self.extractor().server_tick
    }

    /// Seconds per server tick.
    pub(crate) fn tick_interval(&self) -> f32 {
        self.extractor().interval()
    }

    /// Every entity as the world has it, to compare two decoders.
    #[cfg(test)]
    pub(crate) fn world_dump(&self) -> std::collections::BTreeMap<u32, world::Dumped> {
        self.world.as_ref().map(world::World::dump).unwrap_or_default()
    }

    /// The entities the keyframe would hold.
    #[cfg(test)]
    pub(crate) fn world_len(&self) -> usize {
        self.world.as_ref().map_or(0, world::World::len)
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

    fn feed_inner(&mut self, data: &[u8], nbits: usize) -> Lifecycle {
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
                if early_delta {
                    Message::skip_type(t, &mut stream, state).map(|()| None)
                } else if state.should_parse_message(t) && t != MessageType::Empty {
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
                    if extract::debug("errors") {
                        let tick = self.handler.borrow_output().server_tick;
                        eprintln!("decode error at tick {tick}, bit {start} of {nbits}, message id {id}: {e}");
                    }
                    self.parse_errors += 1;
                    life.lost_frame = true;
                    break;
                }
            }
        }
        // Applied once the packet is: a new class's baseline may come in the same packet, before
        // the entities that need it.
        let frames = if self.world.is_some() { entity_frames(&messages) } else { Vec::new() };
        self.tables.note(&messages);
        self.note_round(&messages, &parsed_spans, &life.spans, &data[..nbytes]);
        let mut has_entities = false;
        for msg in &messages {
            match msg {
                Message::SignOnState(s) => life.signon.push(signon(&s.state)),
                Message::ServerInfo(i) => life.server_info = Some((i.server_count as i32, i.map.clone())),
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
    fn note_round(&mut self, messages: &[Message<'_>], spans_of: &[usize], spans: &[Span], data: &[u8]) {
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
                self.round = Some(crate::net::bits::BitReader::slice(data, span.bits.clone()));
            }
        }
    }

    /// The last round start, win panel or game over event: what a client that skipped there
    /// (a jump, or one just in) is sent after a full update, so its HUD shows the round's end, or
    /// hides it, as the game does.
    pub(crate) fn round_event(&self) -> Option<crate::net::bits::BitBuf> {
        self.round.clone()
    }

    /// Sets the last round event, for a decoder that starts from a checkpoint.
    pub(crate) fn set_round_event(&mut self, event: Option<crate::net::bits::BitBuf>) {
        self.round = event;
    }

    /// How many `userinfo` and `instancebaseline` entries were rewritten so far (see `tables`).
    pub(crate) fn tables_rewritten(&self) -> u32 {
        self.tables.rewritten()
    }

    /// Messages that set a client with the string tables of `later` (a decoder further on in the
    /// same demo) back to this one's, as far as they matter; None if they cannot be written.
    pub(crate) fn set_tables_back(&self, later: &Decoder) -> Option<Vec<crate::net::bits::BitBuf>> {
        self.tables.set_back(&later.tables, self.handler.get_parser_state())
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

/// The entities messages among `messages`.
fn entity_frames(
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

/// Whether the `svc_PacketEntities` at `stream`, just past its type, is a delta: after its 11-bit
/// entry count comes whether a base tick follows.
fn is_delta(stream: &BitReadStream<'_, LittleEndian>) -> bool {
    let mut peek = stream.clone();
    peek.skip_bits(11).is_ok() && peek.read_bool().unwrap_or(false)
}

fn signon(s: &SignOnState) -> Signon {
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
fn snapshot_every(hz: f32, interval: f32) -> u32 {
    if hz <= 0.0 { 0 } else { (1.0 / (f64::from(interval) * f64::from(hz)) * (1.0 - 1e-5)).ceil().max(1.0) as u32 }
}

/// When snapshots are due. Each is due `every` ticks after the one before it was due, not after
/// the frame it went out with: the STV sends frames only every few ticks (`tv_snapshotrate`), and
/// counting from a late frame would skip the next one (frames every 3 ticks at `every` 4 would
/// give a snapshot every 6).
#[derive(Default)]
struct SnapshotClock {
    /// The tick of the last snapshot and the tick the next is due.
    last: Option<(u32, u32)>,
}

impl SnapshotClock {
    /// Whether a frame at server tick `now` gets a snapshot. The tick restarts at a map load, so
    /// a tick before the last snapshot is due too; one a whole interval late (a stall) starts
    /// the count again from `now`.
    fn due(&mut self, now: u32, every: u32) -> bool {
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

/// Runs a demo file through the same extractor a live session uses, passing events and
/// snapshots (at most `snapshot_hz` per second; 0 disables them) to `emit`.
pub fn demo_file(bytes: &[u8], snapshot_hz: f32, emit: &mut dyn FnMut(Line)) -> Result<()> {
    let demo = tf_demo_parser::Demo::new(bytes);
    let parser = tf_demo_parser::DemoParser::new_with_analyser(demo.get_stream(), Extractor::default());
    let (_header, mut ticker) = parser.ticker()?;
    let mut snapshots = SnapshotClock::default();
    while ticker.tick()? {
        let x = ticker.state();
        x.announce();
        for line in x.drain() {
            emit(line);
        }
        let now = x.server_tick;
        if now > 0 && snapshots.due(now, snapshot_every(snapshot_hz, x.interval())) {
            emit(Line::Snapshot(x.snapshot()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_timing() {
        let sent = |frames: &[u32], every| {
            let mut clock = SnapshotClock::default();
            frames.iter().copied().filter(|&t| clock.due(t, every)).collect::<Vec<_>>()
        };
        assert_eq!(sent(&[5, 6, 7, 8, 9, 10, 11], 3), [5, 8, 11]);
        let every_third: Vec<u32> = (1..=8).map(|n| n * 3).collect();
        assert_eq!(sent(&every_third, 4), [3, 9, 12, 15, 21, 24], "frames 3 ticks apart: one per 4 ticks");
        assert_eq!(sent(&[4, 8, 13, 17, 21, 25], 4), [4, 8, 13, 17, 21, 25], "every frame of a slower STV");
        assert_eq!(sent(&[5, 6, 100, 101, 103], 3), [5, 100, 103], "after a stall, counted from there");
        assert_eq!(sent(&[500, 501, 2, 3, 5], 3), [500, 2, 5], "tick restarted at a map load");
        assert_eq!(sent(&[u32::MAX - 1, u32::MAX], 3), [u32::MAX - 1, u32::MAX], "saturates near u32::MAX");
        assert_eq!(sent(&[5, 6], 0), [] as [u32; 0], "disabled");
        assert_eq!(snapshot_every(20.0, 0.015), 4, "16.7/s: never over the cap");
        assert_eq!(snapshot_every(33.32, 0.015), 3, "just over a whole tick ratio still rounds up");
        assert_eq!(snapshot_every(20.0, 1.0 / 128.0), 7, "128-tick server");
        assert_eq!(snapshot_every(64.0, 1.0 / 128.0), 2, "an exact divisor stays exact");
        assert_eq!(snapshot_every(1000.0, 0.015), 1, "every tick");
        assert_eq!(snapshot_every(0.0, 0.015), 0);
    }

    fn tables() -> Tables {
        Tables::from_demo_bytes(crate::builtin(crate::CURRENT_BUILD).unwrap().tables).unwrap()
    }

    #[test]
    fn tables_come_only_from_a_readable_demo() {
        assert!(Tables::from_demo(std::path::Path::new("/nonexistent.dem")).is_err());
        assert!(Tables::from_demo_bytes(b"not a demo").is_err());
    }

    /// Read from `g_SendTableCRC` in that build's `engine_srv.so`.
    #[test]
    fn the_crc_is_the_engines() {
        assert_eq!(tables().sendtable_crc(), 0x5FCD_4733);
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pl_upward.dem");
        let full_demo = Tables::from_demo(std::path::Path::new(fixture)).unwrap();
        assert_eq!(full_demo.sendtable_crc(), 0x5FCD_4733, "any demo of the build");
    }

    #[test]
    fn a_message_that_does_not_parse_counts_and_asks_for_a_full_update() {
        let tables = tables();
        let mut dec = Decoder::new(&tables, 0.0).unwrap();
        let life = dec.feed(&[0xff; 64], 64 * 8);
        assert!(life.lost_frame);
        assert!(dec.parse_errors() >= 1);
        assert_eq!(life.spans.last().map(|s| s.id), Some(UNPARSED));
    }

    #[test]
    fn too_few_bits_for_a_message_are_nothing() {
        let tables = tables();
        let mut dec = Decoder::new(&tables, 0.0).unwrap();
        let life = dec.feed(&[0xff], 3);
        assert!(!life.lost_frame && life.spans.is_empty());
        assert_eq!(dec.parse_errors(), 0);
    }

    /// The server tells us to leave in a message the parser has no type for.
    #[test]
    fn a_disconnect_gives_its_reason() {
        let tables = tables();
        let mut dec = Decoder::new(&tables, 0.0).unwrap();
        let bye = crate::net::messages::disconnect("Server shutting down");
        let life = dec.feed(bye.bytes(), bye.nbits());
        assert_eq!(life.disconnect.as_deref(), Some("Server shutting down"));
    }

    #[test]
    fn signon_states_map_one_to_one() {
        let want = [
            (SignOnState::None, Signon::None),
            (SignOnState::Challenge, Signon::Challenge),
            (SignOnState::Connected, Signon::Connected),
            (SignOnState::New, Signon::New),
            (SignOnState::PreSpawn, Signon::PreSpawn),
            (SignOnState::Spawn, Signon::Spawn),
            (SignOnState::Full, Signon::Full),
            (SignOnState::ChangeLevel, Signon::ChangeLevel),
        ];
        for (state, signon_state) in want {
            assert_eq!(signon(&state), signon_state);
        }
    }

    /// What makes a session ask for a full update: nothing in a signon that was going well.
    #[test]
    fn a_delta_before_the_first_full_update_asks_for_nothing() {
        use crate::net::netchan::{self, Kind};
        use crate::net::receiver::Receiver;
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/delta_before_full_update.cap");
        let bytes = std::fs::read(path).unwrap();
        let records = crate::capture::read(&bytes).unwrap();
        let challenge = u32::from_le_bytes(records[0].data.try_into().unwrap());
        let tables = tables();
        let mut dec = Decoder::new(&tables, 0.0).unwrap();
        let mut rx = Receiver::new(Some(challenge));
        let now = std::time::Instant::now();
        let mut full_updates = 0;
        for r in records.iter().skip(1).filter(|r| netchan::classify(r.data) != Kind::Oob) {
            let Some(got) = rx.feed(r.data, now) else { continue };
            for s in &got.streams {
                let life = dec.feed(s.bits.bytes(), s.bits.nbits());
                assert!(!life.lost_frame, "after {full_updates} full updates");
                full_updates += usize::from(life.full_update);
            }
        }
        assert_eq!(full_updates, 1);
    }
}

#[cfg(test)]
mod keyframe_tests {
    use super::*;
    use crate::demo::{Next, Packet, Reader};
    use crate::net::bits::BitReader;
    use std::collections::BTreeMap;

    const SVC_CREATESTRINGTABLE: u8 = 12;
    const SVC_UPDATESTRINGTABLE: u8 = 13;

    /// The demo's tables, signon streams and frames.
    fn packets(path: &str) -> (Tables, Vec<Vec<u8>>, Vec<Vec<u8>>) {
        let tables = Tables::from_demo(std::path::Path::new(&path)).unwrap();
        let mut r = Reader::new(std::fs::File::open(path).unwrap()).unwrap();
        let (mut signon, mut frames) = (Vec::new(), Vec::new());
        while let Next::Packet(p) = r.next_packet().unwrap() {
            match p {
                Packet::Signon { data, .. } => signon.push(data),
                Packet::Message { data, .. } => frames.push(data),
                _ => {}
            }
        }
        (tables, signon, frames)
    }

    /// What of a snapshot comes from the entities alone: the rest (scores, a dead player's last
    /// place, respawn times, health, who left) is gathered from events and frames a late decoder never
    /// saw. Players are by id, to compare those both know.
    fn entities(lines: Vec<Line>) -> Vec<(u32, BTreeMap<u16, String>, serde_json::Value)> {
        lines
            .into_iter()
            .filter_map(|l| match l {
                Line::Snapshot(s) => Some(s),
                _ => None,
            })
            .map(|s| {
                let players = s.players.iter().map(|p| (p.id.0, format!("{:?} {:?}", p.class, p.team))).collect();
                (s.tick, players, serde_json::json!([s.buildings, s.projectiles, s.objectives]))
            })
            .collect()
    }

    fn assert_same_world(a: &Decoder, b: &Decoder, what: &str) {
        let (a, b) = (a.world.as_ref().unwrap().dump(), b.world.as_ref().unwrap().dump());
        assert_eq!(a.len(), b.len(), "{what}: entities");
        for (k, x) in &a {
            assert_eq!(Some(x), b.get(k), "{what}: entity {k}");
        }
    }

    #[test]
    fn a_decoder_started_from_a_keyframe_follows_as_one_from_the_start() {
        let fixture = |name: &str| format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        // A live session recorded as a demo too: its deltas build on frames a few ticks back.
        let opt = crate::Options { snapshot_hz: 0.0, ..crate::Options::default() };
        let recorded = crate::demo::record_capture_for_tests("bots_cp_badlands.cap", &opt).1;
        let demos = ["pl_upward.dem", "ctf_turbine.dem", "pd_watergate.dem", "sd_doomsday.dem"]
            .map(fixture)
            .into_iter()
            .chain([recorded.display().to_string()]);
        for demo in demos {
            follows_from_a_keyframe(&demo, 1, 2);
        }
    }

    /// A live recording of one's own, keyframes at several points.
    #[test]
    #[ignore = "a demo in STVTF_KEYFRAME_DEMO"]
    fn a_decoder_follows_a_given_demo_from_keyframes() {
        let demo = std::env::var("STVTF_KEYFRAME_DEMO").unwrap();
        for at in 1..8 {
            follows_from_a_keyframe(&demo, at, 8);
        }
    }

    /// Starts a decoder from a keyframe `at`/`of` into `demo` and follows it to the end.
    fn follows_from_a_keyframe(demo: &str, at: usize, of: usize) {
        {
            let (tables, signon, frames) = packets(demo);
            let mut whole = Decoder::new(&tables, 1000.0).unwrap();
            whole.keep_world();
            assert!(whole.keyframe(None).is_none(), "{demo}: nothing before the first full update");
            for s in &signon {
                whole.feed(s, s.len() * 8);
            }
            // The string table changes so far, as the relay's log keeps them for a late client.
            let half = frames.len() * at / of;
            let mut tables_since = Vec::new();
            let mut ticks = Vec::new();
            for f in &frames[..half] {
                let life = whole.feed(f, f.len() * 8);
                ticks.push(whole.server_tick());
                for span in life.spans.iter().filter(|s| matches!(s.id, SVC_CREATESTRINGTABLE | SVC_UPDATESTRINGTABLE))
                {
                    tables_since.push(BitReader::slice(f, span.bits.clone()));
                }
            }
            // Of the frame the next builds on.
            let (key_tick, key) = whole.keyframe(Some(&frames[half])).expect("a keyframe");
            assert!(whole.world_len() > 50, "{demo}: {} entities", whole.world_len());
            whole.take_output();

            let mut late = Decoder::new(&tables, 1000.0).unwrap();
            late.keep_world();
            for s in &signon {
                late.feed(s, s.len() * 8);
            }
            for t in &tables_since {
                late.feed(t.bytes(), t.nbits());
            }
            let life = late.feed(key.bytes(), key.nbits());
            assert!(life.full_update && !life.lost_frame, "{demo}");
            // Then the frames played since the one it is of, as the relay sends them with it.
            let recent: Vec<(u32, crate::net::bits::BitBuf)> = frames[..half]
                .iter()
                .zip(&ticks)
                .map(|(f, t)| (*t, crate::net::bits::BitBuf::from_bytes(f.clone())))
                .collect();
            for f in whole.catch_up(key_tick, &recent) {
                let life = late.feed(f.bytes(), f.nbits());
                assert!(!life.lost_frame, "{demo}: a frame sent with the keyframe builds on one the client lacks");
            }
            // A demo's next frame builds on the last: the keyframe is of where `whole` is. (A live
            // session's builds on one a few ticks back, which the keyframe is of instead.)
            if whole.delta_base(&frames[half]) == Some(whole.server_tick()) {
                assert_same_world(&whole, &late, &format!("{demo} at the keyframe"));
            }
            late.take_output();

            for f in &frames[half..] {
                whole.feed(f, f.len() * 8);
                let life = late.feed(f, f.len() * 8);
                assert!(!life.lost_frame, "{demo}: every delta after builds on it");
            }
            assert_same_world(&whole, &late, &format!("{demo} at the end"));
            let (a, b) = (entities(whole.take_output()), entities(late.take_output()));
            assert!(a.len() > 10, "{demo}");
            assert_eq!(a.len(), b.len(), "{demo}");
            for ((tick, pa, xa), (_, pb, xb)) in a.iter().zip(&b) {
                assert_eq!(xa, xb, "{demo} at tick {tick}");
                for (id, p) in pa {
                    if let Some(q) = pb.get(id) {
                        assert_eq!(p, q, "{demo} at tick {tick}: player {id}");
                    }
                }
                assert!(pb.keys().filter(|id| pa.contains_key(id)).count() >= pa.len().min(4), "{demo}: players known");
            }
        }
    }
}

#[cfg(test)]
mod speed {
    use super::*;
    use crate::demo::{Next, Packet, Reader};

    #[test]
    #[ignore = "a demo in STVTF_SPEED_DEMO"]
    fn decoding_speed() {
        let path = std::env::var("STVTF_SPEED_DEMO").unwrap();
        let tables = Tables::from_demo(std::path::Path::new(&path)).unwrap();
        for world in [false, true] {
            let mut r = Reader::new(std::fs::File::open(&path).unwrap()).unwrap();
            let mut dec = Decoder::new(&tables, 0.0).unwrap();
            if world {
                dec.keep_world();
            }
            let t = std::time::Instant::now();
            let mut n = 0;
            while let Next::Packet(p) = r.next_packet().unwrap() {
                if let Packet::Signon { data, .. } | Packet::Message { data, .. } = p {
                    dec.feed(&data, data.len() * 8);
                    n += 1;
                }
            }
            eprintln!("world {world}: {n} packets in {:.1} s", t.elapsed().as_secs_f32());
        }
    }
}
