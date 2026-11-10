//! `.dem` files as raw packets: the signon and message packets keep their bits as they are, so
//! they can be passed on to a client unchanged, and a live session can be recorded as one.
//!
//! A demo is a 1072-byte header, then packets: a command byte, a tick, and per command:
//! signon and message packets 84 bytes of view and sequence data then a length and the message
//! stream; data tables, string tables and console commands a length and the data; user commands
//! a sequence, a length and the data; sync ticks nothing. A stop packet ends it.

use anyhow::{Context, Result, bail};

mod record;
pub use record::Files;
pub(crate) use record::Recorder;
#[cfg(test)]
pub(crate) use record::tests::record_capture as record_capture_for_tests;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};

/// Bytes before the first packet.
pub const HEADER_LEN: usize = 1072;
const MAGIC: &[u8; 8] = b"HL2DEMO\0";
const DEMO_PROTOCOL: u32 = 3;
/// Each of the header's names is this long, NUL padded (`MAX_OSPATH`).
const NAME_LEN: usize = 260;
/// Where the header's names start: after the magic and the demo and network protocols.
const NAMES_AT: usize = 16;
/// Where the header's counts start: after the server, client, map and game names.
const COUNTS_AT: usize = NAMES_AT + 4 * NAME_LEN;
const _: () = assert!(COUNTS_AT + 16 == HEADER_LEN, "seconds, ticks, frames and signon bytes end the header");
/// View origins and angles, then the in and out sequence numbers.
const META_LEN: usize = 84;
/// Longer than any packet a demo has; a corrupt length is not read into memory.
const MAX_PACKET: usize = 1 << 26;

const SIGNON: u8 = 1;
const MESSAGE: u8 = 2;
const SYNC_TICK: u8 = 3;
const CONSOLE_CMD: u8 = 4;
const USER_CMD: u8 = 5;
const DATA_TABLES: u8 = 6;
const STOP: u8 = 7;
const STRING_TABLES: u8 = 8;

/// What a demo's header says.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    /// The network protocol (24 for TF2).
    pub protocol: u32,
    /// The server's name.
    pub server: String,
    /// The recording client's name; `SourceTV Demo` for an STV demo.
    pub client: String,
    /// The map.
    pub map: String,
    /// The game directory: `tf`.
    pub game: String,
    /// How long it plays.
    pub seconds: f32,
    /// Ticks from the first frame to the last.
    pub ticks: u32,
    /// Message packets.
    pub frames: u32,
    /// Bytes of packets before the first frame.
    pub signon_bytes: u32,
}

impl Header {
    /// Reads a header from the start of a demo.
    pub fn parse(b: &[u8]) -> Result<Header> {
        if b.len() < HEADER_LEN || &b[..MAGIC.len()] != MAGIC {
            bail!("not a demo");
        }
        let u = |at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap_or_default());
        let s = |i: usize| {
            let f = &b[NAMES_AT + i * NAME_LEN..][..NAME_LEN];
            String::from_utf8_lossy(&f[..f.iter().position(|&c| c == 0).unwrap_or(NAME_LEN)]).into_owned()
        };
        Ok(Header {
            protocol: u(NAMES_AT - 4),
            server: s(0),
            client: s(1),
            map: s(2),
            game: s(3),
            seconds: f32::from_bits(u(COUNTS_AT)),
            ticks: u(COUNTS_AT + 4),
            frames: u(COUNTS_AT + 8),
            signon_bytes: u(COUNTS_AT + 12),
        })
    }

    /// The header as a demo starts with it.
    pub fn bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(HEADER_LEN);
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&DEMO_PROTOCOL.to_le_bytes());
        b.extend_from_slice(&self.protocol.to_le_bytes());
        for s in [&self.server, &self.client, &self.map, &self.game] {
            let mut f = [0u8; NAME_LEN];
            let n = s.len().min(NAME_LEN - 1);
            f[..n].copy_from_slice(&s.as_bytes()[..n]);
            b.extend_from_slice(&f);
        }
        b.extend_from_slice(&self.seconds.to_bits().to_le_bytes());
        b.extend_from_slice(&self.ticks.to_le_bytes());
        b.extend_from_slice(&self.frames.to_le_bytes());
        b.extend_from_slice(&self.signon_bytes.to_le_bytes());
        b
    }
}

/// One packet of a demo. Signon and message packets hold a message stream, byte padded.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)] // the fields are a packet's tick and data
pub enum Packet {
    /// Messages of the signon.
    Signon { tick: i32, data: Vec<u8> },
    /// The messages of one frame.
    Message { tick: i32, data: Vec<u8> },
    /// Where the frames start.
    SyncTick { tick: i32 },
    /// A console command the recording client ran.
    ConsoleCmd { tick: i32, data: Vec<u8> },
    /// A user command of the recording client.
    UserCmd { tick: i32, data: Vec<u8> },
    /// The send tables and server classes.
    DataTables { tick: i32, data: Vec<u8> },
    /// The string tables as recording started.
    StringTables { tick: i32, data: Vec<u8> },
    /// The end.
    Stop,
}

/// What reading on found.
#[derive(Debug, PartialEq)]
pub enum Next {
    /// A packet.
    Packet(Packet),
    /// The demo's stop packet: there is no more.
    Stop,
    /// The file ends inside a packet, or before one: it may still be being written.
    More,
}

/// Reads a demo's packets in order, from a file that may still grow.
pub struct Reader<R> {
    r: R,
    pos: u64,
    /// The demo's header, as it was when the demo was opened.
    pub header: Header,
}

impl<R: Read + Seek> Reader<R> {
    /// Opens a demo, reading its header.
    pub fn new(mut r: R) -> Result<Self> {
        r.seek(SeekFrom::Start(0))?;
        let mut b = vec![0u8; HEADER_LEN];
        r.read_exact(&mut b).context("reading the demo header")?;
        Ok(Reader { r, pos: HEADER_LEN as u64, header: Header::parse(&b)? })
    }

    /// Reads on from `pos`, where a packet starts (one [`Reader::position`] gave).
    pub fn seek_to(&mut self, pos: u64) -> Result<()> {
        self.r.seek(SeekFrom::Start(pos))?;
        self.pos = pos;
        Ok(())
    }

    /// Bytes read so far: up to the end of the last packet.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// The next packet. At the end of what was written so far it stays where it was, so a
    /// later call reads the packet once it is all there.
    pub fn next_packet(&mut self) -> Result<Next> {
        match self.read_packet() {
            Ok(p) => {
                self.pos = self.r.stream_position()?;
                Ok(match p {
                    Packet::Stop => Next::Stop,
                    p => Next::Packet(p),
                })
            }
            Err(e) if e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == ErrorKind::UnexpectedEof) => {
                self.r.seek(SeekFrom::Start(self.pos))?;
                Ok(Next::More)
            }
            Err(e) => Err(e),
        }
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let cmd = self.u8()?;
        if cmd == STOP {
            return Ok(Packet::Stop);
        }
        let tick = self.u32()? as i32;
        Ok(match cmd {
            SIGNON | MESSAGE => {
                self.skip(META_LEN)?;
                let data = self.data()?;
                if cmd == SIGNON { Packet::Signon { tick, data } } else { Packet::Message { tick, data } }
            }
            SYNC_TICK => Packet::SyncTick { tick },
            CONSOLE_CMD => Packet::ConsoleCmd { tick, data: self.data()? },
            USER_CMD => {
                self.skip(4)?;
                Packet::UserCmd { tick, data: self.data()? }
            }
            DATA_TABLES => Packet::DataTables { tick, data: self.data()? },
            STRING_TABLES => Packet::StringTables { tick, data: self.data()? },
            other => bail!("demo packet of unknown kind {other}"),
        })
    }

    fn u8(&mut self) -> std::io::Result<u8> {
        let mut b = [0u8; 1];
        self.r.read_exact(&mut b)?;
        Ok(b[0])
    }

    fn u32(&mut self) -> std::io::Result<u32> {
        let mut b = [0u8; 4];
        self.r.read_exact(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    fn skip(&mut self, n: usize) -> std::io::Result<()> {
        let mut b = [0u8; META_LEN];
        self.r.read_exact(&mut b[..n])
    }

    fn data(&mut self) -> Result<Vec<u8>> {
        let len = self.u32()? as usize;
        if len > MAX_PACKET {
            bail!("demo packet of {len} bytes");
        }
        let mut data = vec![0u8; len];
        self.r.read_exact(&mut data)?;
        Ok(data)
    }
}

/// Writes a demo as a session goes. The header's counts are written again by [`Writer::finish`]
/// and every [`Writer::flush`], so a demo cut short still plays to where it got.
pub struct Writer<W> {
    w: W,
    header: Header,
    tick_interval: f32,
    first_tick: Option<i32>,
    in_signon: bool,
}

impl<W: Write + Seek> Writer<W> {
    /// Starts a demo with `header` (its counts are kept up to date), on a server ticking every
    /// `tick_interval` seconds.
    pub fn new(mut w: W, header: Header, tick_interval: f32) -> Result<Self> {
        w.write_all(&header.bytes())?;
        Ok(Writer { w, header, tick_interval, first_tick: None, in_signon: true })
    }

    /// The data tables packet of the build, as a tables demo has it.
    pub fn data_tables(&mut self, data: &[u8]) -> Result<()> {
        self.packet(DATA_TABLES, 0, None, data)
    }

    /// Signon messages, byte padded.
    pub fn signon(&mut self, data: &[u8]) -> Result<()> {
        self.packet(SIGNON, 0, Some(&[0u8; META_LEN]), data)
    }

    /// One datagram's messages at server tick `server_tick`, byte padded.
    pub fn message(&mut self, server_tick: i32, data: &[u8]) -> Result<()> {
        if self.in_signon {
            // Everything up to the first frame is the signon, as the header counts it.
            self.in_signon = false;
            self.header.signon_bytes = (self.w.stream_position()? - HEADER_LEN as u64) as u32;
            self.packet(SYNC_TICK, 0, None, &[])?;
        }
        let first = *self.first_tick.get_or_insert(server_tick);
        let tick = (server_tick - first).max(0);
        self.packet(MESSAGE, tick, Some(&[0u8; META_LEN]), data)?;
        self.header.frames += 1;
        self.header.ticks = self.header.ticks.max(tick as u32);
        self.header.seconds = self.header.ticks as f32 * self.tick_interval;
        Ok(())
    }

    fn packet(&mut self, cmd: u8, tick: i32, meta: Option<&[u8]>, data: &[u8]) -> Result<()> {
        self.w.write_all(&[cmd])?;
        self.w.write_all(&tick.to_le_bytes())?;
        if let Some(m) = meta {
            self.w.write_all(m)?;
        }
        if cmd != SYNC_TICK {
            self.w.write_all(&(data.len() as u32).to_le_bytes())?;
            self.w.write_all(data)?;
        }
        Ok(())
    }

    /// Writes the header's counts as they are now and flushes.
    pub fn flush(&mut self) -> Result<()> {
        let end = self.w.stream_position()?;
        self.w.seek(SeekFrom::Start(0))?;
        self.w.write_all(&self.header.bytes())?;
        self.w.seek(SeekFrom::Start(end))?;
        self.w.flush()?;
        Ok(())
    }

    /// Writes out what is buffered, so a reader sees every packet so far; the header's counts
    /// wait for [`Writer::flush`].
    pub fn flush_packets(&mut self) -> Result<()> {
        Ok(self.w.flush()?)
    }

    /// The stop packet and the final header.
    pub fn finish(mut self) -> Result<W> {
        let tick = self.header.ticks as i32;
        self.w.write_all(&[STOP])?;
        self.w.write_all(&tick.to_le_bytes())?;
        self.flush()?;
        Ok(self.w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn a_demo_reads_as_its_packets() {
        let bytes = fixture("pl_upward.dem");
        let mut r = Reader::new(Cursor::new(&bytes)).unwrap();
        assert_eq!(r.header.map, "pl_upward_f12");
        let (mut signon, mut messages, mut tables) = (0, 0, 0);
        loop {
            match r.next_packet().unwrap() {
                Next::Packet(Packet::Signon { .. }) => signon += 1,
                Next::Packet(Packet::Message { .. }) => messages += 1,
                Next::Packet(Packet::DataTables { .. }) => tables += 1,
                Next::Packet(_) => {}
                Next::Stop | Next::More => break,
            }
        }
        assert_eq!((signon, tables), (3, 1));
        assert!(messages > 100, "{messages}");
    }

    #[test]
    fn a_demo_cut_short_waits_for_the_rest() {
        let bytes = fixture("pl_upward.dem");
        // Cut a little way into the tenth packet.
        let mut whole = Reader::new(Cursor::new(&bytes)).unwrap();
        for _ in 0..9 {
            assert!(matches!(whole.next_packet().unwrap(), Next::Packet(_)));
        }
        let at = whole.pos;
        let tenth = whole.next_packet().unwrap();
        let mut r = Reader::new(Cursor::new(bytes[..at as usize + 10].to_vec())).unwrap();
        for _ in 0..9 {
            assert!(matches!(r.next_packet().unwrap(), Next::Packet(_)));
        }
        assert_eq!(r.next_packet().unwrap(), Next::More);
        assert_eq!(r.next_packet().unwrap(), Next::More, "and again");
        assert_eq!(r.pos, at, "it stays before the packet");
        // The rest arrives: reading goes on with that packet.
        let mut grown = Reader::new(Cursor::new(&bytes)).unwrap();
        grown.r.seek(SeekFrom::Start(r.pos)).unwrap();
        grown.pos = r.pos;
        assert_eq!(grown.next_packet().unwrap(), tenth);
    }

    #[test]
    fn a_written_demo_reads_back() {
        let header = Header {
            protocol: 24,
            server: "s".into(),
            client: "SourceTV Demo".into(),
            map: "cp_x".into(),
            game: "tf".into(),
            seconds: 0.0,
            ticks: 0,
            frames: 0,
            signon_bytes: 0,
        };
        let mut w = Writer::new(Cursor::new(Vec::new()), header, 0.015).unwrap();
        w.data_tables(b"tables").unwrap();
        w.signon(b"one").unwrap();
        w.message(1000, b"two").unwrap();
        w.message(1004, b"three").unwrap();
        let bytes = w.finish().unwrap().into_inner();
        let mut r = Reader::new(Cursor::new(&bytes)).unwrap();
        assert_eq!((r.header.frames, r.header.ticks), (2, 4));
        assert!(r.header.signon_bytes > 0);
        let mut got = Vec::new();
        let end = loop {
            match r.next_packet().unwrap() {
                Next::Packet(p) => got.push(p),
                end => break end,
            }
        };
        assert_eq!(end, Next::Stop);
        assert_eq!(
            got,
            [
                Packet::DataTables { tick: 0, data: b"tables".to_vec() },
                Packet::Signon { tick: 0, data: b"one".to_vec() },
                Packet::SyncTick { tick: 0 },
                Packet::Message { tick: 0, data: b"two".to_vec() },
                Packet::Message { tick: 4, data: b"three".to_vec() },
            ]
        );
    }
}
