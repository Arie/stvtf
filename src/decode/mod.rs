//! Feeds network message streams into tf-demo-parser.
//!
//! A `SourceTV` client gets no send tables over the wire (the server expects the
//! client to build them from its own game DLL, checked by the `SendTable` CRC), so
//! the tables come from any demo recorded on the same server build. Each network
//! message stream is then wrapped as a demo `Message` packet and handed to the
//! same `DemoHandler` a demo file would use.

mod detail;
mod extract;
mod modes;
mod props;

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
    /// The entities may have missed a frame: a message failed to decode, or a delta came from a
    /// frame never seen. Later deltas build on it, so only a full update puts them right.
    pub lost_frame: bool,
    /// Entities came as a full update, from no earlier frame.
    pub full_update: bool,
}

pub(crate) struct Decoder<'a> {
    arena: &'a Arena<u8>,
    /// Bytes copied into `arena`: it is only released with the decoder.
    arena_bytes: usize,
    handler: DemoHandler<'a, Extractor>,
    snapshot_hz: f32,
    snapshots: SnapshotClock,
    parse_errors: u64,
    /// The parser panicked on some input; its state can no longer be trusted.
    panicked: bool,
    out: Vec<Line>,
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
            snapshots: SnapshotClock::default(),
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
        if nbits < TYPE_BITS {
            return life;
        }
        let nbytes = nbits.div_ceil(8);
        let bytes: &'a [u8] = self.arena.alloc_extend(data[..nbytes].iter().copied());
        self.arena_bytes += nbytes;
        let mut outer = BitReadStream::new(BitReadBuffer::new(bytes, LittleEndian));
        let Ok(mut stream) = outer.read_bits(nbits) else { return life };
        let state = self.handler.get_parser_state();
        let mut messages = Vec::new();
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
                if state.should_parse_message(t) && t != MessageType::Empty {
                    Message::from_type(t, &mut stream, state).map(Some)
                } else {
                    Message::skip_type(t, &mut stream, state).map(|()| None)
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
                    life.lost_frame = true;
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
                Message::PacketEntities(pe) => {
                    has_entities = true;
                    life.full_update |= pe.delta.is_none();
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
        let x = self.handler.borrow_output();
        life.lost_frame |= x.take_lost_frame();
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
}
