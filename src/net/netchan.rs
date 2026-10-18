//! Netchannel packet header, checksum and outbound packet construction (protocol 24).
//!
//! Header: i32 sequence | i32 sequence_ack | u8 flags | u16 checksum | u8 reliable_state
//! | [u8 choked if CHOKED] | [u32 challenge if CHALLENGE]. The checksum covers
//! everything after itself.

use anyhow::{Result, bail};

pub const FLAG_RELIABLE: u8 = 0x01;
pub const FLAG_CHOKED: u8 = 0x10;
pub const FLAG_CHALLENGE: u8 = 0x20;

pub const HEADER_MIN: usize = 11;

#[derive(Debug, Clone)]
pub struct Header {
    pub sequence: i32,
    pub sequence_ack: i32,
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
    if datagram.len() < 4 {
        return Kind::Short;
    }
    match i32::from_le_bytes([datagram[0], datagram[1], datagram[2], datagram[3]]) {
        -1 => Kind::Oob,
        -2 => Kind::Split,
        -3 => Kind::Compressed,
        _ => Kind::InBand,
    }
}

pub fn fold_checksum(data: &[u8]) -> u16 {
    let crc = crc32fast::hash(data);
    ((crc & 0xffff) ^ (crc >> 16)) as u16
}

pub fn verify_checksum(data: &[u8]) -> bool {
    if data.len() < HEADER_MIN + 1 {
        return false;
    }
    let stored = u16::from_le_bytes([data[9], data[10]]);
    fold_checksum(&data[HEADER_MIN..]) == stored
}

pub fn decode_header(data: &[u8]) -> Result<Header> {
    if data.len() < HEADER_MIN + 1 {
        bail!("short packet ({} bytes)", data.len());
    }
    let sequence = i32::from_le_bytes(data[0..4].try_into()?);
    let sequence_ack = i32::from_le_bytes(data[4..8].try_into()?);
    let flags = data[8];
    let mut off = HEADER_MIN;
    let reliable_state = data[off];
    off += 1;
    let mut choked = None;
    if flags & FLAG_CHOKED != 0 {
        choked = Some(*data.get(off).ok_or_else(|| anyhow::anyhow!("truncated choked"))?);
        off += 1;
    }
    let mut challenge = None;
    if flags & FLAG_CHALLENGE != 0 {
        let b = data.get(off..off + 4).ok_or_else(|| anyhow::anyhow!("truncated challenge"))?;
        challenge = Some(u32::from_le_bytes(b.try_into()?));
        off += 4;
    }
    Ok(Header { sequence, sequence_ack, flags, reliable_state, choked, challenge, body_offset: off })
}

/// Outbound packet: RELIABLE only when a reliable region is present, CHALLENGE always.
/// `reliable_region` and `unreliable` are bit streams; they are concatenated at bit level.
pub fn build_packet(
    out_seq: i32,
    in_seq: i32,
    challenge: u32,
    in_reliable_state: u8,
    reliable_region: Option<(&[u8], usize)>,
    unreliable: Option<(&[u8], usize)>,
) -> Vec<u8> {
    let mut flags = FLAG_CHALLENGE;
    if reliable_region.is_some() {
        flags |= FLAG_RELIABLE;
    }
    let mut body = super::bits::BitWriter::new();
    for (bytes, nbits) in [reliable_region, unreliable].into_iter().flatten() {
        let mut r = super::bits::BitReader::new(bytes);
        let mut left = nbits;
        while left > 0 {
            let n = left.min(32);
            body.write_bits(r.read_bits(n).unwrap_or(0), n);
            left -= n;
        }
    }
    let mut after = vec![in_reliable_state];
    after.extend_from_slice(&challenge.to_le_bytes());
    after.extend_from_slice(&body.into_bytes());
    let mut out = Vec::with_capacity(after.len() + 11);
    out.extend_from_slice(&out_seq.to_le_bytes());
    out.extend_from_slice(&in_seq.to_le_bytes());
    out.push(flags);
    out.extend_from_slice(&fold_checksum(&after).to_le_bytes());
    out.extend_from_slice(&after);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_packet_decodes_and_verifies() {
        let pkt = build_packet(7, 42, 0xdeadbeef, 0b101, Some((&[0xff, 0x01], 9)), None);
        assert!(verify_checksum(&pkt));
        let h = decode_header(&pkt).unwrap();
        assert_eq!(h.sequence, 7);
        assert_eq!(h.sequence_ack, 42);
        assert!(h.reliable());
        assert_eq!(h.reliable_state, 0b101);
        assert_eq!(h.challenge, Some(0xdeadbeef));
        assert_eq!(h.body_offset, 16);
        assert_eq!(&pkt[16..], &[0xff, 0x01]);
    }

    #[test]
    fn corrupt_packet_fails_checksum() {
        let mut pkt = build_packet(1, 1, 1, 0, None, Some((&[0x12, 0x34], 16)));
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
        let pkt = build_packet(1, 1, 1, 0, Some((&[0b101], 3)), Some((&[0xff], 8)));
        assert_eq!(&pkt[16..], &[0b1111_1101, 0b0000_0111]);
    }
}
