//! Writing a demo, packet by packet.

use std::io::{Seek, SeekFrom, Write};

use anyhow::Result;

use super::command::{DATA_TABLES, MESSAGE, META_LEN, SIGNON, STOP, SYNC_TICK, USER_CMD};
use super::{HEADER_LEN, Header};

/// Writes a demo as a session goes. The header's counts are written again by [`Writer::finish`]
/// and every [`Writer::flush`], so a demo cut short still plays to where it got.
pub struct Writer<W> {
    pub(super) w: W,
    pub(super) header: Header,
    pub(super) tick_interval: f32,
    pub(super) first_tick: Option<i32>,
    pub(super) in_signon: bool,
}

impl<W: Write + Seek> Writer<W> {
    /// Starts a demo with `header` (its counts are kept up to date), on a server ticking every
    /// `tick_interval` seconds.
    pub fn new(mut w: W, header: Header, tick_interval: f32) -> crate::Result<Self> {
        w.write_all(&header.bytes())?;
        Ok(Writer { w, header, tick_interval, first_tick: None, in_signon: true })
    }

    /// The data tables packet of the build, as a tables demo has it.
    pub fn data_tables(&mut self, data: &[u8]) -> crate::Result<()> {
        Ok(self.packet(DATA_TABLES, 0, data)?)
    }

    /// Signon messages, byte padded.
    pub fn signon(&mut self, data: &[u8]) -> crate::Result<()> {
        Ok(self.packet(SIGNON, 0, data)?)
    }

    /// One datagram's messages at server tick `server_tick`, byte padded.
    pub fn message(&mut self, server_tick: i32, data: &[u8]) -> crate::Result<()> {
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

    pub(super) fn packet(&mut self, cmd: u8, tick: i32, data: &[u8]) -> Result<()> {
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
    pub fn flush(&mut self) -> crate::Result<()> {
        let end = self.w.stream_position()?;
        self.w.seek(SeekFrom::Start(0))?;
        self.w.write_all(&self.header.bytes())?;
        self.w.seek(SeekFrom::Start(end))?;
        self.w.flush()?;
        Ok(())
    }

    /// Writes out what is buffered, so a reader sees every packet so far; the header's counts
    /// wait for [`Writer::flush`].
    pub fn flush_packets(&mut self) -> crate::Result<()> {
        Ok(self.w.flush()?)
    }

    /// The stop packet and the final header.
    pub fn finish(mut self) -> crate::Result<W> {
        let tick = self.header.ticks as i32;
        self.w.write_all(&[STOP])?;
        self.w.write_all(&tick.to_le_bytes())?;
        self.flush()?;
        Ok(self.w)
    }
}

/// Ends a demo written to `out`: the stop packet, and `header` with its length at `interval`
/// seconds a tick written over the one it started with.
pub(super) fn end_demo<W: Write + Seek>(mut out: W, header: &mut Header, interval: f32) -> Result<W> {
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
pub(super) fn put<W: Write>(w: &mut W, cmd: u8, tick: i32, data: &[u8]) -> Result<()> {
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
