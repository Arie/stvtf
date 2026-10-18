//! Feeds network message streams into tf-demo-parser.
//!
//! A SourceTV client gets no send tables over the wire (the server expects the
//! client to build them from its own game DLL, checked by the SendTable CRC), so
//! the tables come from any demo recorded on the same server build. Each network
//! message stream is then wrapped as a demo `Message` packet and handed to the
//! same DemoHandler a demo file would use.

mod extract;

use crate::net::messages::Signon;
use anyhow::{Context, Result, bail};
use bitbuffer::{BitRead, BitReadBuffer, BitReadStream, LittleEndian};
use extract::Extractor;
use serde_json::Value;
use tf_demo_parser::demo::data::DemoTick;
use tf_demo_parser::demo::header::Header;
use tf_demo_parser::demo::message::generated::SignOnState;
use tf_demo_parser::demo::message::{Message, MessageType};
use tf_demo_parser::demo::packet::Packet;
use tf_demo_parser::demo::packet::datatable::DataTablePacket;
use tf_demo_parser::demo::packet::message::MessagePacket;
use tf_demo_parser::demo::parser::DemoHandler;
use tf_demo_parser::demo::parser::RawPacketStream;
use typed_arena::Arena;

/// The entity class tables of one server build, taken from a demo of that build.
pub struct Tables {
    header: Header,
    datatables: DataTablePacket,
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
                return Ok(Tables { header, datatables: dt });
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
}

/// What the session needs to know from the streams it just fed.
#[derive(Default, Debug)]
pub(crate) struct Lifecycle {
    pub signon: Vec<Signon>,
    pub server_info: Option<(i32, String)>,
    pub tick: Option<i32>,
    pub disconnect: Option<String>,
}

pub(crate) struct Decoder<'a> {
    arena: &'a Arena<u8>,
    /// Bytes copied into `arena`: it is only released with the decoder.
    arena_bytes: usize,
    handler: DemoHandler<'a, Extractor>,
    snapshot_hz: f32,
    last_snapshot: Option<u32>,
    parse_errors: u64,
    /// The parser panicked on some input; its state can no longer be trusted.
    panicked: bool,
    out: Vec<Value>,
}

impl<'a> Decoder<'a> {
    pub(crate) fn new(arena: &'a Arena<u8>, tables: &Tables, snapshot_hz: f32) -> Result<Self> {
        let mut handler = DemoHandler::with_analyser(Extractor::default());
        handler.handle_header(&tables.header);
        handler.handle_packet(Packet::DataTables(tables.datatables.clone()))?;
        Ok(Decoder {
            arena,
            arena_bytes: 0,
            handler,
            snapshot_hz,
            last_snapshot: None,
            parse_errors: 0,
            panicked: false,
            out: Vec::new(),
        })
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
        if nbits < 6 {
            return life;
        }
        let nbytes = nbits.div_ceil(8);
        let bytes: &'a [u8] = self.arena.alloc_extend(data[..nbytes].iter().copied());
        self.arena_bytes += nbytes;
        let mut outer = BitReadStream::new(BitReadBuffer::new(bytes, LittleEndian));
        let Ok(mut stream) = outer.read_bits(nbits) else { return life };
        let state = self.handler.get_parser_state();
        let mut messages = Vec::new();
        while stream.bits_left() > 6 {
            let start = stream.pos();
            let id: u8 = match stream.read_sized(6) {
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
                if state.should_parse_message(t) && t != MessageType::Empty {
                    Message::from_type(t, &mut stream, state).map(Some)
                } else {
                    Message::skip_type(t, &mut stream, state).map(|_| None)
                }
            });
            match parsed {
                Ok(Some(m)) => messages.push(m),
                Ok(None) => {}
                Err(e) => {
                    if extract::debug("errors") {
                        eprintln!("decode error at bit {start} of {nbits}, message id {id}: {e}");
                    }
                    self.parse_errors += 1;
                    break;
                }
            }
        }
        let mut has_entities = false;
        for msg in &messages {
            match msg {
                Message::SignOnState(s) => life.signon.push(signon(&s.state)),
                Message::ServerInfo(i) => life.server_info = Some((i.server_count as i32, i.map.clone())),
                Message::NetTick(t) => life.tick = Some(u32::from(t.tick) as i32),
                Message::PacketEntities(_) => has_entities = true,
                _ => {}
            }
        }
        let tick = self.extractor().server_tick;
        let packet = Packet::Message(MessagePacket { tick: DemoTick::from(tick), messages, meta: Default::default() });
        if self.handler.handle_packet(packet).is_err() {
            self.parse_errors += 1;
        }
        let x = self.handler.borrow_output();
        x.announce();
        let drained = x.drain();
        let now = x.server_tick;
        let every = snapshot_every(self.snapshot_hz, x.interval());
        self.out.extend(drained);
        if has_entities && snapshot_due(self.last_snapshot, now, every) {
            self.last_snapshot = Some(now);
            let snap = self.extractor().snapshot();
            self.out.push(snap);
        }
        life
    }

    pub(crate) fn extractor(&self) -> &Extractor {
        self.handler.borrow_output()
    }

    /// Takes the events and snapshots produced so far.
    pub(crate) fn take_output(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.out)
    }

    pub(crate) fn parse_errors(&self) -> u64 {
        self.parse_errors
    }

    /// Message bytes held for the parser since this decoder was created.
    pub(crate) fn bytes(&self) -> usize {
        self.arena_bytes
    }

    pub(crate) fn panicked(&self) -> bool {
        self.panicked
    }
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

/// Server ticks between snapshots for a rate cap, at `interval` seconds per tick;
/// 0 disables snapshots.
fn snapshot_every(hz: f32, interval: f32) -> u32 {
    if hz <= 0.0 { 0 } else { (1.0 / (interval * hz)).round().max(1.0) as u32 }
}

/// Whether a snapshot is due at server tick `now`. The tick restarts at a map load, so a
/// tick before the last snapshot is due too.
fn snapshot_due(last: Option<u32>, now: u32, every: u32) -> bool {
    every > 0 && last.is_none_or(|l| now < l || now - l >= every)
}

/// Runs a demo file through the same extractor a live session uses, passing events and
/// snapshots (at most `snapshot_hz` per second; 0 disables them) to `emit`.
pub fn demo_file(bytes: &[u8], snapshot_hz: f32, emit: &mut dyn FnMut(Value)) -> Result<()> {
    let demo = tf_demo_parser::Demo::new(bytes);
    let parser = tf_demo_parser::DemoParser::new_with_analyser(demo.get_stream(), Extractor::default());
    let (_header, mut ticker) = parser.ticker()?;
    let mut last: Option<u32> = None;
    while ticker.tick()? {
        let x = ticker.state();
        x.announce();
        for v in x.drain() {
            emit(v);
        }
        let now = x.server_tick;
        if now > 0 && snapshot_due(last, now, snapshot_every(snapshot_hz, x.interval())) {
            last = Some(now);
            emit(x.snapshot());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_timing() {
        assert!(snapshot_due(None, 5, 3));
        assert!(!snapshot_due(Some(5), 7, 3));
        assert!(snapshot_due(Some(5), 8, 3));
        assert!(snapshot_due(Some(500), 2, 3), "tick restarted at a map load");
        assert!(!snapshot_due(Some(u32::MAX - 1), u32::MAX, 3), "no overflow near u32::MAX");
        assert!(!snapshot_due(None, 5, 0), "disabled");
        assert_eq!(snapshot_every(20.0, 0.015), 3);
        assert_eq!(snapshot_every(20.0, 1.0 / 128.0), 6, "128-tick server");
        assert_eq!(snapshot_every(0.0, 0.015), 0);
    }
}
