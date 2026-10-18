//! Reassembly of `-2` split datagrams.
//!
//! Header (12 bytes, LE): i32 -2 | i32 group id | u8 part count | u8 part number | u16 split size.
//! The joined payload is a complete datagram again (usually `-3 SNAP`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

const HEADER_LEN: usize = 12;
const MAX_PARTS: u8 = 128;
const MAX_JOINED: usize = 1 << 18;
const MAX_OPEN: usize = 8;
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct Group {
    count: u8,
    split_size: u16,
    started: Instant,
    parts: HashMap<u8, Vec<u8>>,
}

#[derive(Debug, Default)]
pub struct SplitReassembler {
    open: HashMap<i32, Group>,
    pub dropped: u64,
}

impl SplitReassembler {
    pub fn feed(&mut self, data: &[u8], now: Instant) -> Option<Vec<u8>> {
        self.open.retain(|_, g| now.duration_since(g.started) < TIMEOUT);
        if data.len() < HEADER_LEN {
            self.dropped += 1;
            return None;
        }
        let gid = i32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let (count, number) = (data[8], data[9]);
        let split_size = u16::from_le_bytes([data[10], data[11]]);
        let split_len = usize::from(split_size);
        if count == 0 || number >= count || count > MAX_PARTS || usize::from(count) * split_len > MAX_JOINED {
            self.dropped += 1;
            return None;
        }
        let payload = &data[HEADER_LEN..];
        if payload.len() > split_len || (number < count - 1 && payload.len() != split_len) {
            self.dropped += 1;
            return None;
        }
        if let Some(g) = self.open.get(&gid)
            && (g.count != count || g.split_size != split_size)
        {
            self.open.remove(&gid);
        }
        if !self.open.contains_key(&gid)
            && self.open.len() >= MAX_OPEN
            && let Some(oldest) = self.open.iter().min_by_key(|(_, g)| g.started).map(|(k, _)| *k)
        {
            self.open.remove(&oldest);
            self.dropped += 1;
        }
        let g =
            self.open.entry(gid).or_insert_with(|| Group { count, split_size, started: now, parts: HashMap::new() });
        g.parts.entry(number).or_insert_with(|| payload.to_vec());
        if g.parts.len() < usize::from(count) {
            return None;
        }
        let g = self.open.remove(&gid)?;
        let mut joined = Vec::with_capacity(usize::from(count) * split_len);
        for n in 0..count {
            joined.extend_from_slice(g.parts.get(&n)?);
        }
        Some(joined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(gid: i32, count: u8, number: u8, size: u16, payload: &[u8]) -> Vec<u8> {
        let mut d = (-2i32).to_le_bytes().to_vec();
        d.extend_from_slice(&gid.to_le_bytes());
        d.push(count);
        d.push(number);
        d.extend_from_slice(&size.to_le_bytes());
        d.extend_from_slice(payload);
        d
    }

    #[test]
    fn joins_out_of_order_and_interleaved() {
        let mut r = SplitReassembler::default();
        let now = Instant::now();
        assert!(r.feed(&part(1, 3, 2, 4, b"xy"), now).is_none());
        assert!(r.feed(&part(2, 2, 0, 4, b"AAAA"), now).is_none());
        assert!(r.feed(&part(1, 3, 0, 4, b"abcd"), now).is_none());
        assert!(r.feed(&part(1, 3, 0, 4, b"abcd"), now).is_none(), "duplicate is ignored");
        assert_eq!(r.feed(&part(1, 3, 1, 4, b"efgh"), now).unwrap(), b"abcdefghxy");
        assert_eq!(r.feed(&part(2, 2, 1, 4, b"B"), now).unwrap(), b"AAAAB");
    }

    #[test]
    fn rejects_bad_headers_and_short_parts() {
        let mut r = SplitReassembler::default();
        let now = Instant::now();
        assert!(r.feed(&part(1, 2, 2, 4, b"abcd"), now).is_none());
        assert!(r.feed(&part(1, 2, 0, 4, b"ab"), now).is_none(), "non-final part must be full size");
        assert_eq!(r.dropped, 2);
    }

    #[test]
    fn expires_stale_groups() {
        let mut r = SplitReassembler::default();
        let t0 = Instant::now();
        assert!(r.feed(&part(9, 2, 0, 2, b"ab"), t0).is_none());
        let later = t0 + Duration::from_secs(6);
        assert!(r.feed(&part(9, 2, 1, 2, b"cd"), later).is_none(), "first part expired");
    }
}
