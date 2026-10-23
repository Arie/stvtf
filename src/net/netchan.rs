//! Netchannel packet header, checksum and outbound packet construction (protocol 24).
//!
//! Header: i32 sequence | i32 `sequence_ack` | u8 flags | u16 checksum | u8 `reliable_state`
//! | [u8 choked if CHOKED] | [u32 challenge if CHALLENGE]. The checksum covers
//! everything after itself.

use super::bits::{BitBuf, BitWriter};
use anyhow::{Result, bail};

pub const FLAG_RELIABLE: u8 = 0x01;
pub const FLAG_CHOKED: u8 = 0x10;
pub const FLAG_CHALLENGE: u8 = 0x20;

/// A datagram starts with an i32: the sequence of an in-band packet, or one of the markers below.
pub const SEQUENCE_LEN: usize = 4;
const MARKER_OOB: i32 = -1;
const MARKER_SPLIT: i32 = -2;
const MARKER_COMPRESSED: i32 = -3;
/// Header offsets: flags after sequence and ack, then the checksum (u16).
const FLAGS_AT: usize = 2 * SEQUENCE_LEN;
const CHECKSUM_AT: usize = FLAGS_AT + 1;
/// Where the checksummed part starts (after sequence, ack, flags and the checksum itself).
pub const CHECKSUM_START: usize = CHECKSUM_AT + 2;
const CHALLENGE_LEN: usize = 4;
/// The shortest header: up to and including `reliable_state`.
pub const HEADER_MIN: usize = CHECKSUM_START + 1;

#[derive(Debug, Clone)]
pub struct Header {
    pub sequence: i32,
    pub flags: u8,
    pub reliable_state: u8,
    pub choked: Option<u8>,
    pub challenge: Option<u32>,
    pub body_offset: usize,
}

impl Header {
    pub fn reliable(&self) -> bool {
        self.flags & FLAG_RELIABLE != 0
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    Oob,
    Split,
    Compressed,
    InBand,
    Short,
}

pub fn classify(datagram: &[u8]) -> Kind {
    let Some(Ok(marker)) = datagram.get(..SEQUENCE_LEN).map(<[u8; SEQUENCE_LEN]>::try_from) else {
        return Kind::Short;
    };
    match i32::from_le_bytes(marker) {
        MARKER_OOB => Kind::Oob,
        MARKER_SPLIT => Kind::Split,
        MARKER_COMPRESSED => Kind::Compressed,
        _ => Kind::InBand,
    }
}

pub fn fold_checksum(data: &[u8]) -> u16 {
    let crc = crc32fast::hash(data);
    ((crc & 0xffff) ^ (crc >> 16)) as u16
}

pub fn verify_checksum(data: &[u8]) -> bool {
    if data.len() < HEADER_MIN {
        return false;
    }
    let stored = u16::from_le_bytes([data[CHECKSUM_AT], data[CHECKSUM_AT + 1]]);
    fold_checksum(&data[CHECKSUM_START..]) == stored
}

pub fn decode_header(data: &[u8]) -> Result<Header> {
    if data.len() < HEADER_MIN {
        bail!("short packet ({} bytes)", data.len());
    }
    let sequence = i32::from_le_bytes(data[..SEQUENCE_LEN].try_into()?);
    let flags = data[FLAGS_AT];
    let mut off = CHECKSUM_START;
    let reliable_state = data[off];
    off += 1;
    let mut choked = None;
    if flags & FLAG_CHOKED != 0 {
        choked = Some(*data.get(off).ok_or_else(|| anyhow::anyhow!("truncated choked"))?);
        off += 1;
    }
    let mut challenge = None;
    if flags & FLAG_CHALLENGE != 0 {
        let b = data.get(off..off + CHALLENGE_LEN).ok_or_else(|| anyhow::anyhow!("truncated challenge"))?;
        challenge = Some(u32::from_le_bytes(b.try_into()?));
        off += CHALLENGE_LEN;
    }
    Ok(Header { sequence, flags, reliable_state, choked, challenge, body_offset: off })
}

/// The header fields of an outbound packet.
#[derive(Debug, Clone, Copy)]
pub struct OutHeader {
    /// Our outgoing sequence number.
    pub sequence: i32,
    /// The last sequence we received.
    pub ack: i32,
    pub challenge: u32,
    /// Our reliable state bits for the server's subchannels.
    pub reliable_state: u8,
}

/// Outbound packet: RELIABLE only when a reliable region is present, CHALLENGE always.
/// The reliable region and the unreliable messages are concatenated at bit level.
pub fn build_packet(h: &OutHeader, reliable_region: Option<&BitBuf>, unreliable: Option<&BitBuf>) -> Vec<u8> {
    let mut flags = FLAG_CHALLENGE;
    if reliable_region.is_some() {
        flags |= FLAG_RELIABLE;
    }
    let mut body = BitWriter::new();
    for part in [reliable_region, unreliable].into_iter().flatten() {
        body.write_buf(part);
    }
    let mut after = vec![h.reliable_state];
    after.extend_from_slice(&h.challenge.to_le_bytes());
    after.extend_from_slice(&body.into_bytes());
    let mut out = Vec::with_capacity(CHECKSUM_START + after.len());
    out.extend_from_slice(&h.sequence.to_le_bytes());
    out.extend_from_slice(&h.ack.to_le_bytes());
    out.push(flags);
    out.extend_from_slice(&fold_checksum(&after).to_le_bytes());
    out.extend_from_slice(&after);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(sequence: i32, ack: i32, challenge: u32, reliable_state: u8) -> OutHeader {
        OutHeader { sequence, ack, challenge, reliable_state }
    }

    fn bits(val: u32, n: usize) -> BitBuf {
        let mut w = BitWriter::new();
        w.write_bits(val, n);
        w.finish()
    }

    #[test]
    fn built_packet_decodes_and_verifies() {
        let pkt = build_packet(&header(7, 42, 0xdead_beef, 0b101), Some(&bits(0x1ff, 9)), None);
        assert!(verify_checksum(&pkt));
        let h = decode_header(&pkt).unwrap();
        assert_eq!(h.sequence, 7);
        assert!(h.reliable());
        assert_eq!(h.reliable_state, 0b101);
        assert_eq!(h.challenge, Some(0xdead_beef));
        assert_eq!(h.body_offset, 16);
        assert_eq!(&pkt[16..], &[0xff, 0x01]);
    }

    #[test]
    fn corrupt_packet_fails_checksum() {
        let mut pkt = build_packet(&header(1, 1, 1, 0), None, Some(&bits(0x3412, 16)));
        assert!(verify_checksum(&pkt));
        let last = pkt.len() - 1;
        pkt[last] ^= 0x40;
        assert!(!verify_checksum(&pkt));
    }

    #[test]
    fn classify_markers() {
        assert_eq!(classify(&(-1i32).to_le_bytes()), Kind::Oob);
        assert_eq!(classify(&(-2i32).to_le_bytes()), Kind::Split);
        assert_eq!(classify(&(-3i32).to_le_bytes()), Kind::Compressed);
        assert_eq!(classify(&5i32.to_le_bytes()), Kind::InBand);
        assert_eq!(classify(&[1, 2]), Kind::Short);
    }

    #[test]
    fn bit_level_concatenation() {
        // 3-bit reliable region followed by an 8-bit unreliable byte lands unaligned.
        let pkt = build_packet(&header(1, 1, 1, 0), Some(&bits(0b101, 3)), Some(&bits(0xff, 8)));
        assert_eq!(&pkt[16..], &[0b1111_1101, 0b0000_0111]);
    }
}
