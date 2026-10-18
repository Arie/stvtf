//! Feeds network message streams into tf-demo-parser.
//!
//! A SourceTV client gets no send tables over the wire (the server expects the
//! client to build them from its own game DLL, checked by the SendTable CRC), so
//! the tables come from any demo recorded on the same server build. Each network
//! message stream is then wrapped as a demo `Message` packet and handed to the
//! same DemoHandler a demo file would use.

pub mod extract;

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

pub struct Tables {
    pub header: Header,
    pub datatables: DataTablePacket,
}

impl Tables {
    pub fn from_demo(path: &std::path::Path) -> Result<Tables> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_demo_bytes(&bytes)
    }

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
}

/// What the session needs to know from the streams it just fed.
#[derive(Default, Debug)]
pub struct Lifecycle {
    pub signon: Vec<u8>,
    pub server_info: Option<(i32, String)>,
    pub tick: Option<i32>,
    pub disconnect: Option<String>,
}

pub struct Decoder<'a> {
    arena: &'a Arena<u8>,
    handler: DemoHandler<'a, Extractor>,
    snapshot_ticks: u32,
    last_snapshot: Option<u32>,
    pub parse_errors: u64,
    pub out: Vec<Value>,
}

impl<'a> Decoder<'a> {
    pub fn new(arena: &'a Arena<u8>, tables: &Tables, snapshot_hz: f32) -> Result<Self> {
        let mut handler = DemoHandler::with_analyser(Extractor::default());
        handler.handle_header(&tables.header);
        handler.handle_packet(Packet::DataTables(tables.datatables.clone()))?;
        let snapshot_ticks = if snapshot_hz <= 0.0 { 0 } else { (66.67 / snapshot_hz).round().max(1.0) as u32 };
        Ok(Decoder { arena, handler, snapshot_ticks, last_snapshot: None, parse_errors: 0, out: Vec::new() })
    }

    /// Feeds one message stream (bit packed from bit 0) and returns its lifecycle messages.
    ///
    /// The stream is parsed with its exact bit length: a datagram ends in up to five
    /// junk padding bits, and once realigned to bit 0 a byte-granular reader would
    /// see enough bits to decode them as a bogus message.
    pub fn feed(&mut self, data: &[u8], nbits: usize) -> Lifecycle {
        let mut life = Lifecycle::default();
        if nbits < 6 {
            return life;
        }
        let bytes: &'a [u8] = self.arena.alloc_extend(data[..nbits.div_ceil(8)].iter().copied());
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
                    if std::env::var("STVMAP_DEBUG").is_ok_and(|v| v == "errors") {
                        eprintln!("decode error at bit {start} of {nbits}, message id {id}: {e}");
                    }
                    self.parse_errors += 1;
                    break;
                }
            }
        }
        let tick = self.extractor().server_tick;
        let packet = Packet::Message(MessagePacket { tick: DemoTick::from(tick), messages, meta: Default::default() });
        let mut has_entities = false;
        if let Packet::Message(m) = &packet {
            for msg in &m.messages {
                match msg {
                    Message::SignOnState(s) => life.signon.push(signon_num(&s.state)),
                    Message::ServerInfo(i) => life.server_info = Some((i.server_count as i32, i.map.clone())),
                    Message::NetTick(t) => life.tick = Some(u32::from(t.tick) as i32),
                    Message::PacketEntities(_) => has_entities = true,
                    _ => {}
                }
            }
        }
        if self.handler.handle_packet(packet).is_err() {
            self.parse_errors += 1;
        }
        let x = self.handler.borrow_output();
        x.announce();
        let drained = x.drain();
        let now = x.server_tick;
        self.out.extend(drained);
        if has_entities
            && self.snapshot_ticks > 0
            && self.last_snapshot.is_none_or(|l| now >= l + self.snapshot_ticks || now < l)
        {
            self.last_snapshot = Some(now);
            let snap = self.extractor().snapshot();
            self.out.push(snap);
        }
        life
    }

    pub fn extractor(&self) -> &Extractor {
        self.handler.borrow_output()
    }
}

fn signon_num(s: &SignOnState) -> u8 {
    match s {
        SignOnState::None => 0,
        SignOnState::Challenge => 1,
        SignOnState::Connected => 2,
        SignOnState::New => 3,
        SignOnState::PreSpawn => 4,
        SignOnState::Spawn => 5,
        SignOnState::Full => 6,
        SignOnState::ChangeLevel => 7,
    }
}

/// Runs a demo file through the same extractor a live session uses.
pub fn demo_file(bytes: &[u8], snapshot_hz: f32, emit: &mut dyn FnMut(Value)) -> Result<()> {
    let demo = tf_demo_parser::Demo::new(bytes);
    let parser = tf_demo_parser::DemoParser::new_with_analyser(demo.get_stream(), Extractor::default());
    let (_header, mut ticker) = parser.ticker()?;
    let every = if snapshot_hz <= 0.0 { 0 } else { (66.67 / snapshot_hz).round().max(1.0) as u32 };
    let mut last: Option<u32> = None;
    while ticker.tick()? {
        let x = ticker.state();
        x.announce();
        for v in x.drain() {
            emit(v);
        }
        let now = x.server_tick;
        if every > 0 && now > 0 && last.is_none_or(|l| now >= l + every || now < l) {
            last = Some(now);
            emit(x.snapshot());
        }
    }
    Ok(())
}
