//! Reliable subchannel region parsing and multi-fragment transfer reassembly.
//!
//! Region layout after the header when RELIABLE is set:
//!   ubit(3) subchannel, then per stream (0 = messages, 1 = files):
//!     bit data-follows; if set: bit multi; if multi: ubit(18) `start_fragment` | ubit(3) `num_fragments`
//!     if `start_fragment` == 0:
//!       single: bit compressed [-> ubit(26) size], varint32 nbytes
//!       multi:  bit `is_file` [-> ubit(32) id, string name], bit compressed [-> ubit(26)], ubit(26) nbytes
//!     `num_fragments` * 256 bytes (the last fragment trimmed)

use super::bits::BitReader;
use super::compress;
use anyhow::{Result, bail};

/// The netchannel's reliable subchannels, numbered in 3 bits.
pub const SUBCHANNEL_BITS: usize = 3;
pub const SUBCHANNELS: u8 = 1 << SUBCHANNEL_BITS;
const FRAGMENT_SIZE: usize = 256;
/// A multi-fragment block: the number of its first fragment and how many it carries.
const START_FRAGMENT_BITS: usize = 18;
const FRAGMENT_COUNT_BITS: usize = 3;
/// A file transfer's id and file name (`MAX_OSPATH`), skipped.
const TRANSFER_ID_BITS: usize = 32;
const MAX_FILE_NAME: usize = 260;
/// A transfer's sizes (`MAX_FILE_SIZE_BITS`).
const SIZE_BITS: usize = 26;
const MAX_TRANSFER: usize = 4 << 20;
/// Index of the message stream in `Reassembler::streams` (1 is the file stream).
const STREAM_MESSAGES: usize = 0;

#[derive(Debug, Default)]
struct Transfer {
    nbytes: usize,
    total_frags: usize,
    have: Vec<bool>,
    /// Number of `true` entries in `have`.
    received: usize,
    buf: Vec<u8>,
    /// Declared size after decompression, for compressed transfers.
    uncompressed: Option<usize>,
}

impl Transfer {
    fn active(&self) -> bool {
        self.total_frags > 0
    }

    fn complete(&self) -> bool {
        self.active() && self.received == self.total_frags
    }
}

#[derive(Debug, Default)]
pub struct Reassembler {
    streams: [Transfer; 2],
}

#[derive(Debug)]
pub struct Region {
    pub subchannel: u8,
    /// Completed message-stream payloads (the file stream is discarded).
    pub completed: Vec<Vec<u8>>,
    /// Bit offset where the unreliable messages start.
    pub end_bit: usize,
}

pub fn latch_subchannel(data: &[u8], body_offset: usize) -> Option<u8> {
    BitReader::at(data, body_offset * 8).read_bits(SUBCHANNEL_BITS).ok().map(|v| v as u8)
}

impl Reassembler {
    pub fn feed(&mut self, data: &[u8], body_offset: usize) -> Result<Region> {
        let mut br = BitReader::at(data, body_offset * 8);
        let subchannel = br.read_bits(SUBCHANNEL_BITS)? as u8;
        let mut completed = Vec::new();
        for (idx, st) in self.streams.iter_mut().enumerate() {
            if !br.read_bit()? {
                continue;
            }
            if let Some(done) = read_block(&mut br, st)?
                && idx == STREAM_MESSAGES
            {
                completed.push(done);
            }
        }
        Ok(Region { subchannel, completed, end_bit: br.pos() })
    }
}

fn read_block(br: &mut BitReader<'_>, st: &mut Transfer) -> Result<Option<Vec<u8>>> {
    let multi = br.read_bit()?;
    let (start_frag, mut num_frags) = if multi {
        (br.read_bits(START_FRAGMENT_BITS)? as usize, br.read_bits(FRAGMENT_COUNT_BITS)? as usize)
    } else {
        (0, 0)
    };
    if start_frag == 0 {
        *st = Transfer::default();
        if multi {
            if br.read_bit()? {
                br.read_bits(TRANSFER_ID_BITS)?;
                br.read_string(MAX_FILE_NAME)?;
            }
            if br.read_bit()? {
                st.uncompressed = Some(br.read_bits(SIZE_BITS)? as usize);
            }
            st.nbytes = br.read_bits(SIZE_BITS)? as usize;
        } else {
            if br.read_bit()? {
                st.uncompressed = Some(br.read_bits(SIZE_BITS)? as usize);
            }
            st.nbytes = br.read_varint32()? as usize;
            num_frags = st.nbytes.div_ceil(FRAGMENT_SIZE);
            if st.nbytes == 0 {
                // An empty single block carries nothing to deliver.
                *st = Transfer::default();
                return Ok(None);
            }
        }
        let nbytes = st.nbytes;
        if nbytes > MAX_TRANSFER {
            *st = Transfer::default();
            bail!("declared transfer {nbytes} over cap");
        }
        st.total_frags = nbytes.div_ceil(FRAGMENT_SIZE);
        st.have = vec![false; st.total_frags];
    }
    if !st.active() {
        bail!("continuation of an unseen transfer");
    }
    if start_frag + num_frags > st.total_frags {
        *st = Transfer::default();
        bail!("fragments {start_frag}+{num_frags} past transfer end");
    }
    let mut length = num_frags * FRAGMENT_SIZE;
    if start_frag + num_frags == st.total_frags {
        let trim = st.nbytes % FRAGMENT_SIZE;
        if trim != 0 {
            length -= FRAGMENT_SIZE - trim;
        }
    }
    let chunk = br.read_bytes(length)?;
    let offset = start_frag * FRAGMENT_SIZE;
    if st.buf.len() < offset + chunk.len() {
        st.buf.resize(offset + chunk.len(), 0);
    }
    st.buf[offset..offset + chunk.len()].copy_from_slice(&chunk);
    for have in &mut st.have[start_frag..start_frag + num_frags] {
        if !*have {
            *have = true;
            st.received += 1;
        }
    }
    if !st.complete() {
        return Ok(None);
    }
    let mut out = std::mem::take(&mut st.buf);
    out.truncate(st.nbytes);
    let uncompressed = st.uncompressed;
    *st = Transfer::default();
    match uncompressed {
        Some(size) => {
            let inflated = compress::inflate_transfer(&out)?;
            if inflated.len() != size {
                bail!("transfer inflated to {} bytes, declared {size}", inflated.len());
            }
            Ok(Some(inflated))
        }
        None => Ok(Some(out)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::bits::BitWriter;

    fn single_block(sub: u8, body: &[u8]) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_bits(u32::from(sub), 3);
        w.write_bit(true);
        w.write_bit(false);
        w.write_bit(false);
        w.write_varint32(body.len() as u32);
        w.write_bytes(body);
        w.write_bit(false);
        w.write_bits(0xab, 8); // unreliable tail
        w.into_bytes()
    }

    fn multi_fragment(sub: u8, start: usize, num: usize, total: usize, chunk: &[u8]) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_bits(u32::from(sub), 3);
        w.write_bit(true);
        w.write_bit(true);
        w.write_bits(start as u32, 18);
        w.write_bits(num as u32, 3);
        if start == 0 {
            w.write_bit(false);
            w.write_bit(false);
            w.write_bits(total as u32, 26);
        }
        w.write_bytes(chunk);
        w.write_bit(false);
        w.into_bytes()
    }

    #[test]
    fn single_block_completes_and_reports_tail() {
        let pkt = single_block(5, b"hello world");
        let mut r = Reassembler::default();
        let region = r.feed(&pkt, 0).unwrap();
        assert_eq!(region.subchannel, 5);
        assert_eq!(region.completed, vec![b"hello world".to_vec()]);
        let mut tail = BitReader::at(&pkt, region.end_bit);
        assert_eq!(tail.read_u8().unwrap(), 0xab);
    }

    #[test]
    fn multi_fragment_out_of_order() {
        let total = 600;
        let payload: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
        let mut r = Reassembler::default();
        let first = multi_fragment(1, 0, 1, total, &payload[..256]);
        assert!(r.feed(&first, 0).unwrap().completed.is_empty(), "incomplete after the first fragment");
        let last = multi_fragment(1, 2, 1, total, &payload[512..]);
        assert!(r.feed(&last, 0).unwrap().completed.is_empty(), "incomplete without the middle fragment");
        let resent_last = multi_fragment(1, 2, 1, total, &payload[512..]);
        assert!(r.feed(&resent_last, 0).unwrap().completed.is_empty(), "a resend must not complete with a hole");
        let middle = multi_fragment(1, 1, 1, total, &payload[256..512]);
        assert_eq!(r.feed(&middle, 0).unwrap().completed, vec![payload]);
    }

    #[test]
    fn continuation_without_start_is_an_error() {
        let pkt = multi_fragment(0, 1, 1, 0, &[0u8; 256]);
        assert!(Reassembler::default().feed(&pkt, 0).is_err());
    }

    #[test]
    fn fragments_past_transfer_end_are_an_error() {
        // A 300-byte transfer has two fragments; the first block claims three.
        let pkt = multi_fragment(0, 0, 3, 300, &[0u8; 768]);
        assert!(Reassembler::default().feed(&pkt, 0).is_err());
    }

    #[test]
    fn empty_single_block_delivers_nothing() {
        let region = Reassembler::default().feed(&single_block(0, b""), 0).unwrap();
        assert!(region.completed.is_empty(), "{:?}", region.completed);
    }

    #[test]
    fn compressed_single_block() {
        let body = b"compressed compressed compressed".repeat(10);
        let mut framed = b"SNAP".to_vec();
        framed.extend_from_slice(&snap::raw::Encoder::new().compress_vec(&body).unwrap());
        let mut w = BitWriter::new();
        w.write_bits(0, 3);
        w.write_bit(true);
        w.write_bit(false);
        w.write_bit(true);
        w.write_bits(body.len() as u32, 26);
        w.write_varint32(framed.len() as u32);
        w.write_bytes(&framed);
        w.write_bit(false);
        let region = Reassembler::default().feed(&w.into_bytes(), 0).unwrap();
        assert_eq!(region.completed, vec![body]);
    }
}
