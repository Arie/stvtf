//! The packets of a TF2 .dem, for the examples that cut one.
//!
//! Packet layout (demo protocol 3): u8 command, i32 tick, then by command:
//!
//! ```text
//! 1 signon / 2 packet: 76 bytes cmd info, i32 in seq, i32 out seq, i32 length, data
//! 3 sync tick:         nothing
//! 4 console command:   i32 length, data
//! 5 user command:      i32 sequence, i32 length, data
//! 6 data tables:       i32 length, data
//! 7 stop:              end of the demo
//! 8 string tables:     i32 length, data
//! ```

// Each example uses part of this.
#![allow(dead_code)]

use anyhow::{Context, Result, bail};

pub const MAGIC: &[u8; 8] = b"HL2DEMO\0";
pub const HEADER: usize = 1072;
// Header fields after the four 260-byte strings.
pub const DURATION: usize = 1056;
pub const TICKS: usize = 1060;
pub const FRAMES: usize = 1064;
pub const CMD_INFO: usize = 76;
pub const SIGNON: u8 = 1;
pub const PACKET: u8 = 2;
pub const SYNC_TICK: u8 = 3;
pub const CONSOLE_CMD: u8 = 4;
pub const USER_CMD: u8 = 5;
pub const DATA_TABLES: u8 = 6;
pub const STOP: u8 = 7;
pub const STRING_TABLES: u8 = 8;

pub fn i32_at(data: &[u8], pos: usize) -> Result<i32> {
    let bytes = data.get(pos..pos + 4).context("demo truncated")?;
    Ok(i32::from_le_bytes(bytes.try_into()?))
}

/// A length field followed by that many bytes, starting at `pos`; returns the end.
fn skip_sized(data: &[u8], pos: usize) -> Result<usize> {
    let len = usize::try_from(i32_at(data, pos)?).context("negative length")?;
    Ok(pos + 4 + len)
}

pub struct Packet {
    pub start: usize,
    pub end: usize,
    pub cmd: u8,
    pub tick: i32,
}

/// The packets after the header, up to a stop packet or the end of the file.
pub fn packets(data: &[u8]) -> Result<Vec<Packet>> {
    if data.len() < HEADER || &data[..8] != MAGIC {
        bail!("not a Source demo");
    }
    let mut out = Vec::new();
    let mut pos = HEADER;
    // Some demos end without a stop packet, in a few bytes of trailer.
    while pos + 5 <= data.len() {
        let start = pos;
        let cmd = data[pos];
        let tick = i32_at(data, pos + 1)?;
        pos += 5;
        pos = match cmd {
            SIGNON | PACKET => skip_sized(data, pos + CMD_INFO + 8)?,
            SYNC_TICK => pos,
            CONSOLE_CMD | DATA_TABLES | STRING_TABLES => skip_sized(data, pos)?,
            USER_CMD => skip_sized(data, pos + 4)?,
            STOP => break,
            _ => bail!("unknown demo command {cmd} at byte {start}"),
        };
        if pos > data.len() {
            bail!("demo truncated in the packet at byte {start}");
        }
        out.push(Packet { start, end: pos, cmd, tick });
    }
    Ok(out)
}

/// Ends `out` with a stop packet at `tick`.
pub fn stop(out: &mut Vec<u8>, tick: i32) {
    out.push(STOP);
    out.extend_from_slice(&tick.to_le_bytes());
    out.push(0); // the engine writes one padding byte after the stop command
}
