//! `-3 SNAP` datagrams are raw Snappy; compressed reliable transfers are
//! Snappy or Valve LZSS depending on their magic.

use anyhow::{Result, bail};

const MAX_OUTPUT: usize = 1 << 22;
/// A compressed block starts with its kind; LZSS's then has the uncompressed size (u32).
pub const SNAPPY_MAGIC: &[u8; 4] = b"SNAP";
const LZSS_MAGIC: &[u8; 4] = b"LZSS";
const LZSS_HEADER: usize = LZSS_MAGIC.len() + 4;
/// LZSS: each command byte flags the next 8 items (1 a back reference, 0 a literal byte).
const ITEMS_PER_COMMAND: u8 = 8;

pub fn snappy(block: &[u8]) -> Result<Vec<u8>> {
    let len = snap::raw::decompress_len(block)?;
    if len > MAX_OUTPUT {
        bail!("snappy declared size {len} over cap");
    }
    Ok(snap::raw::Decoder::new().decompress_vec(block)?)
}

pub fn lzss(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < LZSS_HEADER || &data[..LZSS_MAGIC.len()] != LZSS_MAGIC {
        bail!("not an LZSS block");
    }
    let actual = u32::from_le_bytes(data[LZSS_MAGIC.len()..LZSS_HEADER].try_into()?) as usize;
    if actual > MAX_OUTPUT {
        bail!("LZSS declared size {actual} over cap");
    }
    let mut out = Vec::with_capacity(actual);
    let mut src = LZSS_HEADER;
    let mut cmd = 0u8;
    let mut left = 0;
    while out.len() < actual {
        if left == 0 {
            let Some(&c) = data.get(src) else { break };
            cmd = c;
            src += 1;
            left = ITEMS_PER_COMMAND;
        }
        if cmd & 1 == 1 {
            let Some(&[hi, lo]) = data.get(src..src + 2) else { break };
            let pos = (usize::from(hi) << 4) | usize::from(lo >> 4);
            let count = usize::from(lo & 0xf) + 1;
            src += 2;
            if count == 1 {
                break;
            }
            let Some(mut r) = out.len().checked_sub(pos + 1) else {
                bail!("LZSS back-reference before start");
            };
            if out.len() + count > actual {
                bail!("LZSS back-reference past declared size {actual}");
            }
            for _ in 0..count {
                out.push(out[r]);
                r += 1;
            }
        } else {
            let Some(&b) = data.get(src) else { break };
            out.push(b);
            src += 1;
        }
        cmd >>= 1;
        left -= 1;
    }
    // Like the engine, reject a stream that ends early.
    if out.len() != actual {
        bail!("LZSS produced {} of {actual} bytes", out.len());
    }
    Ok(out)
}

pub fn inflate_transfer(payload: &[u8]) -> Result<Vec<u8>> {
    match payload.get(..SNAPPY_MAGIC.len()) {
        Some(m) if m == SNAPPY_MAGIC => snappy(&payload[SNAPPY_MAGIC.len()..]),
        Some(m) if m == LZSS_MAGIC => lzss(payload),
        _ => bail!("unknown compressed transfer magic"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snappy_roundtrip() {
        let input: Vec<u8> = b"spectate spectate spectate spectate".repeat(20);
        let block = snap::raw::Encoder::new().compress_vec(&input).unwrap();
        assert_eq!(snappy(&block).unwrap(), input);
        let mut framed = b"SNAP".to_vec();
        framed.extend_from_slice(&block);
        assert_eq!(inflate_transfer(&framed).unwrap(), input);
    }

    #[test]
    fn lzss_literals_and_backref() {
        // "abcabcabc": 3 literals, then a back-reference of distance 3, length 6.
        // Command byte bits (LSB first): L L L R, then terminator R(count=1).
        let mut data = b"LZSS".to_vec();
        data.extend_from_slice(&9u32.to_le_bytes());
        data.push(0b0001_1000);
        data.extend_from_slice(b"abc");
        // pos = distance - 1 = 2, count - 1 = 5
        data.push((2 >> 4) as u8);
        data.push(((2 & 0xf) << 4) as u8 | 5);
        assert_eq!(lzss(&data).unwrap(), b"abcabcabc");
    }

    fn lzss_block(size: u32, body: &[u8]) -> Vec<u8> {
        let mut data = b"LZSS".to_vec();
        data.extend_from_slice(&size.to_le_bytes());
        data.extend_from_slice(body);
        data
    }

    #[test]
    fn lzss_command_byte_at_end_is_an_error() {
        // The command byte promises a literal that is not there: used to index past the end.
        assert!(lzss(&lzss_block(1, &[0x00])).is_err());
    }

    #[test]
    fn lzss_truncated_stream_is_an_error() {
        assert!(lzss(&lzss_block(4, &[0x00, b'a', b'b'])).is_err());
        assert!(lzss(&lzss_block(4, &[0x01, 0x00])).is_err(), "half a back-reference");
    }

    #[test]
    fn lzss_back_reference_past_declared_size_is_an_error() {
        // One literal, then a back-reference of 16 bytes into a 4-byte output.
        assert!(lzss(&lzss_block(4, &[0b10, b'a', 0x00, 0x0f])).is_err());
    }

    #[test]
    fn rejects_unknown_magic() {
        assert!(inflate_transfer(b"ZZZZ1234").is_err());
    }
}
