//! A demo's header: what it is of, and its counts.

use crate::error::bail;
/// Bytes before the first packet.
pub const HEADER_LEN: usize = 1072;

pub(super) const MAGIC: &[u8; 8] = b"HL2DEMO\0";

pub(super) const DEMO_PROTOCOL: u32 = 3;

/// Each of the header's names is this long, NUL padded (`MAX_OSPATH`).
pub(super) const NAME_LEN: usize = 260;

/// Where the header's names start: after the magic and the demo and network protocols.
pub(super) const NAMES_AT: usize = 16;

/// Where the header's counts start: after the server, client, map and game names.
pub(super) const COUNTS_AT: usize = NAMES_AT + 4 * NAME_LEN;

const _: () = assert!(COUNTS_AT + 16 == HEADER_LEN, "seconds, ticks, frames and signon bytes end the header");

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
    pub fn parse(b: &[u8]) -> crate::Result<Header> {
        if b.len() < HEADER_LEN || &b[..MAGIC.len()] != MAGIC {
            bail!("cannot read a file that is not a demo");
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
