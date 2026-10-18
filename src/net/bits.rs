//! LSB-first bit reader and writer, matching Valve's bf_read / bf_write.

use anyhow::{Result, bail};

/// A bit stream packed from bit 0: `bytes` holds exactly `nbits` bits, the last byte
/// possibly partial. Only `BitWriter`, `from_bytes` and `read_remaining` build one,
/// which keeps the two fields consistent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BitBuf {
    bytes: Vec<u8>,
    nbits: usize,
}

impl BitBuf {
    /// Whole bytes as a bit stream.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let nbits = bytes.len() * 8;
        BitBuf { bytes, nbits }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn nbits(&self) -> usize {
        self.nbits
    }
}

#[derive(Debug)]
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], pos: usize) -> Self {
        BitReader { data, pos }
    }

    /// The current bit position.
    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn bits_left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    pub fn read_bits(&mut self, n: usize) -> Result<u32> {
        if n > 32 || n > self.bits_left() {
            bail!("read_bits({n}) past end");
        }
        let mut val: u64 = 0;
        let mut got = 0;
        while got < n {
            let byte = u64::from(self.data[(self.pos + got) >> 3]);
            let shift = (self.pos + got) & 7;
            let take = (8 - shift).min(n - got);
            let bits = (byte >> shift) & ((1 << take) - 1);
            val |= bits << got;
            got += take;
        }
        self.pos += n;
        Ok(val as u32)
    }

    pub fn read_bit(&mut self) -> Result<bool> {
        Ok(self.read_bits(1)? == 1)
    }

    pub fn read_u8(&mut self) -> Result<u8> {
        Ok(self.read_bits(8)? as u8)
    }

    pub fn read_i32(&mut self) -> Result<i32> {
        Ok(self.read_bits(32)? as i32)
    }

    pub fn read_varint32(&mut self) -> Result<u32> {
        let mut result: u32 = 0;
        for shift in (0..35).step_by(7) {
            let b = self.read_u8()?;
            result |= u32::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(result);
            }
        }
        bail!("varint32 too long")
    }

    pub fn read_string(&mut self, max: usize) -> Result<String> {
        let mut out = Vec::new();
        while out.len() < max {
            let c = self.read_u8()?;
            if c == 0 {
                break;
            }
            out.push(c);
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    pub fn read_bytes(&mut self, n: usize) -> Result<Vec<u8>> {
        if n.checked_mul(8).is_none_or(|bits| bits > self.bits_left()) {
            bail!("read_bytes({n}) past end");
        }
        if self.pos & 7 == 0 {
            let start = self.pos >> 3;
            self.pos += n * 8;
            return Ok(self.data[start..start + n].to_vec());
        }
        (0..n).map(|_| self.read_u8()).collect()
    }

    /// Every remaining bit, repacked to start at bit 0.
    pub fn read_remaining(&mut self) -> BitBuf {
        let nbits = self.bits_left();
        let mut out = vec![0u8; nbits.div_ceil(8)];
        let shift = self.pos & 7;
        let start = self.pos >> 3;
        for (i, slot) in out.iter_mut().enumerate() {
            let lo = u16::from(self.data[start + i]);
            let hi = u16::from(*self.data.get(start + i + 1).unwrap_or(&0));
            *slot = (((hi << 8) | lo) >> shift) as u8;
        }
        if !nbits.is_multiple_of(8) {
            let last = out.len() - 1;
            out[last] &= (1u8 << (nbits % 8)) - 1;
        }
        self.pos += nbits;
        BitBuf { bytes: out, nbits }
    }
}

#[derive(Debug, Default)]
pub struct BitWriter {
    bytes: Vec<u8>,
    nbits: usize,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write_bits(&mut self, val: u32, n: usize) {
        debug_assert!(n <= 32, "write_bits({n})");
        for i in 0..n {
            if self.nbits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if (val >> i) & 1 == 1 {
                let last = self.bytes.len() - 1;
                self.bytes[last] |= 1 << (self.nbits % 8);
            }
            self.nbits += 1;
        }
    }

    pub fn write_bit(&mut self, b: bool) {
        self.write_bits(u32::from(b), 1);
    }

    pub fn write_u8(&mut self, v: u8) {
        self.write_bits(u32::from(v), 8);
    }

    pub fn write_i32(&mut self, v: i32) {
        self.write_bits(v as u32, 32);
    }

    pub fn write_bytes(&mut self, data: &[u8]) {
        for &b in data {
            self.write_u8(b);
        }
    }

    pub fn write_string(&mut self, s: &str) {
        self.write_bytes(s.as_bytes());
        self.write_u8(0);
    }

    /// Appends all bits of another stream.
    pub fn write_buf(&mut self, buf: &BitBuf) {
        let mut r = BitReader::new(&buf.bytes);
        let mut left = buf.nbits;
        while left > 0 {
            let n = left.min(32);
            // In range: a BitBuf holds exactly nbits bits.
            self.write_bits(r.read_bits(n).unwrap_or(0), n);
            left -= n;
        }
    }

    pub fn write_varint32(&mut self, mut v: u32) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                self.write_u8(b | 0x80);
            } else {
                self.write_u8(b);
                return;
            }
        }
    }

    pub fn nbits(&self) -> usize {
        self.nbits
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn finish(self) -> BitBuf {
        BitBuf { bytes: self.bytes, nbits: self.nbits }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_mixed_widths() {
        let mut w = BitWriter::new();
        w.write_bits(5, 3);
        w.write_bit(true);
        w.write_i32(-12345);
        w.write_varint32(300);
        w.write_string("hello");
        w.write_bits(0x2abcd, 18);
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(3).unwrap(), 5);
        assert!(r.read_bit().unwrap());
        assert_eq!(r.read_i32().unwrap(), -12345);
        assert_eq!(r.read_varint32().unwrap(), 300);
        assert_eq!(r.read_string(64).unwrap(), "hello");
        assert_eq!(r.read_bits(18).unwrap(), 0x2abcd);
    }

    #[test]
    fn lsb_first_layout() {
        let mut w = BitWriter::new();
        w.write_bits(1, 1);
        w.write_bits(0b101, 3);
        assert_eq!(w.into_bytes(), vec![0b1011]);
    }

    #[test]
    fn read_remaining_realigns() {
        let mut w = BitWriter::new();
        w.write_bits(0b101, 3);
        w.write_u8(0xAB);
        w.write_u8(0xCD);
        w.write_bits(0b11, 2);
        let bytes = w.into_bytes();
        let mut r = BitReader::at(&bytes, 3);
        let rest = r.read_remaining();
        let out = rest.bytes();
        assert_eq!(rest.nbits(), bytes.len() * 8 - 3);
        assert_eq!(out[0], 0xAB);
        assert_eq!(out[1], 0xCD);
        assert_eq!(out[2] & 0b11, 0b11);
    }

    #[test]
    fn past_end_errors() {
        let data = [0xffu8];
        let mut r = BitReader::new(&data);
        assert!(r.read_bits(9).is_err());
        assert_eq!(r.read_bits(8).unwrap(), 0xff);
        assert!(r.read_bit().is_err());
        assert!(BitReader::new(&data).read_bytes(usize::MAX).is_err(), "no overflow in the length check");
    }

    #[test]
    fn write_buf_appends_unaligned() {
        let mut a = BitWriter::new();
        a.write_bits(0b101, 3);
        let mut w = BitWriter::new();
        w.write_bit(true);
        w.write_buf(&a.finish());
        let b = w.finish();
        assert_eq!(b.nbits(), 4);
        assert_eq!(b.bytes(), &[0b1011]);
    }
}
