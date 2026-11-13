//! `.dem` files as raw packets: the signon and message packets keep their bits as they are, so
//! they can be passed on to a client unchanged, and a live session can be recorded as one.
//!
//! A demo is a 1072-byte header, then packets: a command byte, a tick, and per command:
//! signon and message packets 84 bytes of view and sequence data then a length and the message
//! stream; data tables, string tables and console commands a length and the data; user commands
//! a sequence, a length and the data; sync ticks nothing. A stop packet ends it.

use crate::net::messages::{SVC_CLASSINFO, TYPE_BITS};
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

/// Whether a signon packet opens with the class list (`svc_ClassInfo`). The data tables go just
/// before that one: TF2 playing a demo itself takes them nowhere else (first of all, it crashes
/// once loaded).
pub(crate) fn opens_with_classes(signon: &[u8]) -> bool {
    signon.first().is_some_and(|b| u32::from(*b) & ((1 << TYPE_BITS) - 1) == SVC_CLASSINFO)
}

/// Data tables waiting for their place in a signon: just before the packet that opens with the
/// class list, else at its end.
pub(crate) struct HeldTables<T>(Option<T>);

impl<T> HeldTables<T> {
    pub(crate) fn new(tables: Option<T>) -> Self {
        HeldTables(tables)
    }

    /// The tables, if they go before signon packet `signon`.
    pub(crate) fn before(&mut self, signon: &[u8]) -> Option<T> {
        if opens_with_classes(signon) { self.0.take() } else { None }
    }

    /// The tables, if they are still to go: the signon ends here.
    pub(crate) fn at_end(&mut self) -> Option<T> {
        self.0.take()
    }
}

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
#[expect(missing_docs, reason = "the fields are a packet's tick and data")]
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
                self.skip_meta()?;
                let data = self.data()?;
                if cmd == SIGNON { Packet::Signon { tick, data } } else { Packet::Message { tick, data } }
            }
            SYNC_TICK => Packet::SyncTick { tick },
            CONSOLE_CMD => Packet::ConsoleCmd { tick, data: self.data()? },
            USER_CMD => {
                self.u32()?; // its sequence number
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

    fn skip_meta(&mut self) -> std::io::Result<()> {
        self.r.read_exact(&mut [0u8; META_LEN])
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
        self.packet(DATA_TABLES, 0, data)
    }

    /// Signon messages, byte padded.
    pub fn signon(&mut self, data: &[u8]) -> Result<()> {
        self.packet(SIGNON, 0, data)
    }

    /// One datagram's messages at server tick `server_tick`, byte padded.
    pub fn message(&mut self, server_tick: i32, data: &[u8]) -> Result<()> {
        if self.in_signon {
            // Everything up to the first frame is the signon, as the header counts it.
            self.in_signon = false;
            self.header.signon_bytes = (self.w.stream_position()? - HEADER_LEN as u64) as u32;
            self.packet(SYNC_TICK, 0, &[])?;
        }
        let first = *self.first_tick.get_or_insert(server_tick);
        let tick = (server_tick - first).max(0);
        self.packet(MESSAGE, tick, data)?;
        self.header.frames += 1;
        self.header.ticks = self.header.ticks.max(tick as u32);
        self.header.seconds = self.header.ticks as f32 * self.tick_interval;
        Ok(())
    }

    fn packet(&mut self, cmd: u8, tick: i32, data: &[u8]) -> Result<()> {
        self.w.write_all(&[cmd])?;
        self.w.write_all(&tick.to_le_bytes())?;
        if matches!(cmd, SIGNON | MESSAGE) {
            self.w.write_all(&[0u8; META_LEN])?;
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

/// Voice to write into a demo: Opus frames (20 ms each) as the voice of the player in `slot`
/// (their entity less one), whose Steam account is `account`, heard at demo tick `tick`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voiced {
    /// The demo's tick it is heard at: the server's, less that of the demo's first frame.
    pub tick: i32,
    /// The speaker's slot.
    pub slot: u8,
    /// The speaker's Steam account (`[U:1:<account>]`).
    pub account: u32,
    /// Samples a second the frames are labelled with.
    pub rate: u16,
    /// The Opus packets.
    pub frames: Vec<Vec<u8>>,
}

/// The tick voice due at `due` is heard at, before the frame at `tick`: no earlier than what
/// went `before` it, nor later than the frame (whatever a demo's ticks say).
fn heard_at(due: i32, before: i32, tick: i32) -> i32 {
    due.max(before).min(tick)
}

/// Copies demo `src` to `out` with `voices` in it, each a frame of its own before the first of
/// the demo's at or after its tick, as the game's own voice messages (so the demo plays them as
/// the players talking). The header's counts are made anew, and a demo still being written (no
/// stop packet yet) gets one.
pub fn with_voice<R: Read + Seek, W: Write + Seek>(src: R, voices: &[Voiced], mut out: W) -> Result<W> {
    let mut r = Reader::new(src)?;
    let mut header = r.header.clone();
    let interval = if header.ticks > 0 && header.seconds > 0.0 { header.seconds / header.ticks as f32 } else { 0.015 };
    (header.frames, header.ticks) = (0, 0);
    out.write_all(&header.bytes())?;
    let mut order: Vec<&Voiced> = voices.iter().collect();
    order.sort_by_key(|v| v.tick);
    let mut order = order.into_iter().peekable();
    // A speaker's frames are numbered on from one message to the next.
    let mut numbers: std::collections::HashMap<u8, u16> = std::collections::HashMap::new();
    let mut last = 0;
    // Data tables that come before the signon (as older recordings have them) wait for their
    // place in it.
    let (mut tables, mut signed_on) = (HeldTables::<Vec<u8>>::new(None), false);
    let mut say = |out: &mut W, header: &mut Header, v: &Voiced, tick: i32| -> Result<()> {
        let seq = numbers.entry(v.slot).or_insert(0);
        for part in crate::decode::voice_runs(&v.frames) {
            if let Some(m) = crate::decode::steam_voice(v.slot, u64::from(v.account), v.rate, part, seq) {
                put(out, MESSAGE, tick, m.bytes())?;
                header.frames += 1;
            }
        }
        Ok(())
    };
    loop {
        match r.next_packet()? {
            Next::Packet(Packet::Message { tick, data }) => {
                while let Some(v) = order.next_if(|v| v.tick <= tick) {
                    last = heard_at(v.tick, last, tick);
                    say(&mut out, &mut header, v, last)?;
                }
                put(&mut out, MESSAGE, tick, &data)?;
                last = tick;
                header.frames += 1;
                header.ticks = header.ticks.max(tick.max(0) as u32);
            }
            Next::Packet(Packet::Signon { tick, data }) => {
                if let Some(t) = tables.before(&data) {
                    put(&mut out, DATA_TABLES, tick, &t)?;
                }
                signed_on = true;
                put(&mut out, SIGNON, tick, &data)?;
            }
            Next::Packet(Packet::SyncTick { tick }) => {
                if let Some(t) = tables.at_end() {
                    put(&mut out, DATA_TABLES, tick, &t)?;
                }
                // Everything before the frames is the signon, as the header counts it.
                header.signon_bytes = (out.stream_position()? - HEADER_LEN as u64) as u32;
                put(&mut out, SYNC_TICK, tick, &[])?;
            }
            Next::Packet(Packet::ConsoleCmd { tick, data }) => put(&mut out, CONSOLE_CMD, tick, &data)?,
            Next::Packet(Packet::UserCmd { tick, data }) => put(&mut out, USER_CMD, tick, &data)?,
            Next::Packet(Packet::DataTables { data, .. }) if !signed_on => tables = HeldTables::new(Some(data)),
            Next::Packet(Packet::DataTables { tick, data }) => put(&mut out, DATA_TABLES, tick, &data)?,
            Next::Packet(Packet::StringTables { tick, data }) => put(&mut out, STRING_TABLES, tick, &data)?,
            Next::Packet(Packet::Stop) | Next::Stop | Next::More => break,
        }
    }
    // Said after the demo's last frame: with it.
    for v in order {
        say(&mut out, &mut header, v, last)?;
    }
    out.write_all(&[STOP])?;
    out.write_all(&(header.ticks as i32).to_le_bytes())?;
    header.seconds = header.ticks as f32 * interval;
    out.seek(SeekFrom::Start(0))?;
    out.write_all(&header.bytes())?;
    out.seek(SeekFrom::End(0))?;
    out.flush()?;
    Ok(out)
}

/// One packet, as a demo has it.
fn put<W: Write>(w: &mut W, cmd: u8, tick: i32, data: &[u8]) -> Result<()> {
    w.write_all(&[cmd])?;
    w.write_all(&tick.to_le_bytes())?;
    if matches!(cmd, SIGNON | MESSAGE) {
        w.write_all(&[0u8; META_LEN])?;
    }
    if cmd == USER_CMD {
        w.write_all(&0u32.to_le_bytes())?;
    }
    if cmd != SYNC_TICK {
        w.write_all(&(data.len() as u32).to_le_bytes())?;
        w.write_all(data)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_demo_reads_as_its_packets() {
        let bytes = std::fs::read(crate::fixture("pl_upward.dem")).unwrap();
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
        let bytes = std::fs::read(crate::fixture("pl_upward.dem")).unwrap();
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
        grown.seek_to(r.pos).unwrap();
        assert_eq!(grown.next_packet().unwrap(), tenth);
    }

    #[test]
    fn voice_is_written_into_a_demo_as_frames_of_its_own_at_their_ticks() {
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
        // As older recordings have it: the tables first. TF2 will not play that.
        w.data_tables(b"tables").unwrap();
        w.signon(b"one").unwrap();
        w.signon(&[SVC_CLASSINFO as u8, 1]).unwrap();
        for tick in [1000, 1004, 1010] {
            w.message(tick, b"game").unwrap();
        }
        let demo = w.finish().unwrap().into_inner();
        let said = |tick, slot, frame: u8| Voiced { tick, slot, account: 77, rate: 24000, frames: vec![vec![frame]] };
        // Out of order, before the first frame, and past the last: each lands where it can.
        let voices = [said(7, 3, 2), said(2, 3, 1), said(-5, 4, 9), said(99, 3, 3)];
        let out = with_voice(Cursor::new(&demo), &voices, Cursor::new(Vec::new())).unwrap().into_inner();
        let mut r = Reader::new(Cursor::new(&out)).unwrap();
        assert_eq!((r.header.frames, r.header.ticks, r.header.map.as_str()), (7, 10, "cp_x"));
        let (mut got, mut before) = (Vec::new(), Vec::new());
        while let Next::Packet(p) = r.next_packet().unwrap() {
            match p {
                Packet::Message { tick, data } => got.push((tick, data)),
                Packet::Signon { data, .. } | Packet::DataTables { data, .. } => before.push(data),
                _ => {}
            }
        }
        assert_eq!(
            before,
            [b"one".to_vec(), b"tables".to_vec(), vec![SVC_CLASSINFO as u8, 1]],
            "the tables before the class list"
        );
        // A speaker's frames are numbered on, in the order they are heard.
        let (mut three, mut four) = (0, 0);
        let mut voice = |slot, frame: u8| {
            let seq = if slot == 3 { &mut three } else { &mut four };
            crate::decode::steam_voice(slot, 77, 24000, &[vec![frame]], seq).unwrap().bytes().to_vec()
        };
        let game = b"game".to_vec();
        let expected = [
            (0, voice(4, 9)),
            (0, game.clone()),
            (2, voice(3, 1)),
            (4, game.clone()),
            (7, voice(3, 2)),
            (10, game),
            (10, voice(3, 3)),
        ];
        assert_eq!(got, expected);
        assert_eq!(three, 3);
    }

    #[test]
    fn voice_is_heard_no_earlier_than_what_went_before_it_whatever_a_demos_ticks_say() {
        assert_eq!(heard_at(7, 4, 10), 7);
        assert_eq!(heard_at(2, 4, 10), 4);
        // A demo whose ticks go back: at the frame's, no panic.
        assert_eq!(heard_at(-5, 0, -1), -1);
    }

    #[test]
    fn a_demo_still_being_written_gets_its_voice_and_an_end() {
        let mut w = Writer::new(Cursor::new(Vec::new()), Header { map: "cp_x".into(), ..blank() }, 0.015).unwrap();
        w.signon(b"one").unwrap();
        w.message(1000, b"game").unwrap();
        w.message(1004, b"game").unwrap();
        w.flush_packets().unwrap();
        // No stop packet, and the header's counts not written yet.
        let cut = w.w.into_inner();
        let voices = [Voiced { tick: 2, slot: 1, account: 5, rate: 24000, frames: vec![vec![1]] }];
        let out = with_voice(Cursor::new(&cut), &voices, Cursor::new(Vec::new())).unwrap().into_inner();
        let mut r = Reader::new(Cursor::new(&out)).unwrap();
        assert_eq!((r.header.frames, r.header.ticks), (3, 4), "counted as it is copied");
        assert!(r.header.signon_bytes > 0, "and the signon measured");
        let mut n = 0;
        let end = loop {
            match r.next_packet().unwrap() {
                Next::Packet(_) => n += 1,
                end => break end,
            }
        };
        assert_eq!((n, end), (5, Next::Stop), "signon, sync, two frames and the voice, then the end");
    }

    fn blank() -> Header {
        Header {
            protocol: 24,
            server: String::new(),
            client: String::new(),
            map: String::new(),
            game: "tf".into(),
            seconds: 0.0,
            ticks: 0,
            frames: 0,
            signon_bytes: 0,
        }
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
