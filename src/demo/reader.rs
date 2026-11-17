//! Reading a demo, packet by packet.

use std::io::{ErrorKind, Read, Seek, SeekFrom};

use super::command::{
    CONSOLE_CMD, DATA_TABLES, MAX_PACKET, MESSAGE, META_LEN, SIGNON, STOP, STRING_TABLES, SYNC_TICK, USER_CMD,
    VIEW_ANGLES,
};
use super::{HEADER_LEN, Header};
use crate::Result;
use crate::error::{Context, bail};

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
    pub(super) r: R,
    pub(super) pos: u64,
    /// The demo's header, as it was when the demo was opened.
    pub header: Header,
    /// Where the recording game looked (pitch, yaw, roll) as of the last frame read: a player's
    /// own demo has it.
    pub view: [f32; 3],
}

impl<R: Read + Seek> Reader<R> {
    /// Opens a demo, reading its header.
    pub fn new(mut r: R) -> Result<Self> {
        r.seek(SeekFrom::Start(0))?;
        let mut b = vec![0u8; HEADER_LEN];
        r.read_exact(&mut b).context("read the demo header")?;
        Ok(Reader { r, pos: HEADER_LEN as u64, header: Header::parse(&b)?, view: [0.0; 3] })
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
            Err(e) if e.io_kind() == Some(ErrorKind::UnexpectedEof) => {
                self.r.seek(SeekFrom::Start(self.pos))?;
                Ok(Next::More)
            }
            Err(e) => Err(e),
        }
    }

    pub(super) fn read_packet(&mut self) -> Result<Packet> {
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
            other => bail!("cannot read a demo packet of unknown kind {other}"),
        })
    }

    pub(super) fn u8(&mut self) -> std::io::Result<u8> {
        let mut b = [0u8; 1];
        self.r.read_exact(&mut b)?;
        Ok(b[0])
    }

    pub(super) fn u32(&mut self) -> std::io::Result<u32> {
        let mut b = [0u8; 4];
        self.r.read_exact(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// Passes over a packet's command info, keeping its view angles.
    pub(super) fn skip_meta(&mut self) -> std::io::Result<()> {
        let mut meta = [0u8; META_LEN];
        self.r.read_exact(&mut meta)?;
        for (i, angle) in self.view.iter_mut().enumerate() {
            let at = VIEW_ANGLES + 4 * i;
            *angle = f32::from_le_bytes([meta[at], meta[at + 1], meta[at + 2], meta[at + 3]]);
        }
        Ok(())
    }

    pub(super) fn data(&mut self) -> Result<Vec<u8>> {
        let len = self.u32()? as usize;
        if len > MAX_PACKET {
            bail!("cannot read a demo packet of {len} bytes");
        }
        let mut data = vec![0u8; len];
        self.r.read_exact(&mut data)?;
        Ok(data)
    }
}
