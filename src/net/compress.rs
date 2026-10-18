//! `-3 SNAP` datagrams are raw Snappy; compressed reliable transfers are
//! Snappy or Valve LZSS depending on their magic.

use anyhow::{Result, bail};

const MAX_OUTPUT: usize = 1 << 22;

pub fn snappy(block: &[u8]) -> Result<Vec<u8>> {
    let len = snap::raw::decompress_len(block)?;
    if len > MAX_OUTPUT {
        bail!("snappy declared size {len} over cap");
    }
    Ok(snap::raw::Decoder::new().decompress_vec(block)?)
}

pub fn lzss(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 8 || &data[..4] != b"LZSS" {
        bail!("not an LZSS block");
    }
    let actual = u32::from_le_bytes(data[4..8].try_into()?) as usize;
    if actual > MAX_OUTPUT {
        bail!("LZSS declared size {actual} over cap");
    }
    let mut out = Vec::with_capacity(actual);
    let mut src = 8;
    let mut cmd = 0u8;
    let mut left = 0;
    while out.len() < actual && src < data.len() {
        if left == 0 {
            cmd = data[src];
            src += 1;
            left = 8;
        }
        if cmd & 1 == 1 {
            if src + 1 >= data.len() {
                break;
            }
            let pos = ((data[src] as usize) << 4) | (data[src + 1] as usize >> 4);
            let count = (data[src + 1] & 0xf) as usize + 1;
            src += 2;
            if count == 1 {
                break;
            }
            let Some(mut r) = out.len().checked_sub(pos + 1) else {
                bail!("LZSS back-reference before start");
            };
            for _ in 0..count {
                out.push(out[r]);
                r += 1;
            }
        } else {
            out.push(data[src]);
            src += 1;
        }
        cmd >>= 1;
        left -= 1;
    }
    Ok(out)
}

pub fn inflate_transfer(payload: &[u8]) -> Result<Vec<u8>> {
    match payload.get(..4) {
        Some(b"SNAP") => snappy(&payload[4..]),
        Some(b"LZSS") => lzss(payload),
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

    #[test]
    fn rejects_unknown_magic() {
        assert!(inflate_transfer(b"ZZZZ1234").is_err());
    }
}
