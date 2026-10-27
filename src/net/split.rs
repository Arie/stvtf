//! Reassembly of `-2` split datagrams.
//!
//! Header (12 bytes, LE): i32 -2 | i32 group id | u8 part count | u8 part number | u16 split size.
//! The joined payload is a complete datagram again (usually `-3 SNAP`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// After the split marker: i32 group, u8 parts, u8 this part's number, u16 part size.
const GROUP_AT: usize = 4;
const COUNT_AT: usize = 8;
const NUMBER_AT: usize = 9;
const SIZE_AT: usize = 10;
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
        let gid = i32::from_le_bytes(data[GROUP_AT..COUNT_AT].try_into().ok()?);
        let (count, number) = (data[COUNT_AT], data[NUMBER_AT]);
        let split_size = u16::from_le_bytes(data[SIZE_AT..HEADER_LEN].try_into().ok()?);
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

/// Payload bytes in each part of a datagram we split (`nSplitSizeMinusHeader`), within the
/// 564 to 1248 the engine accepts.
pub const SPLIT_SIZE: usize = 1200;

/// Cuts a datagram too large for one packet into `-2` parts of group `group`.
pub fn split(datagram: &[u8], group: i32) -> Vec<Vec<u8>> {
    let chunks: Vec<&[u8]> = datagram.chunks(SPLIT_SIZE).collect();
    let count = u8::try_from(chunks.len()).expect("datagrams stay under 128 parts");
    chunks
        .iter()
        .enumerate()
        .map(|(n, chunk)| {
            let mut d = Vec::with_capacity(HEADER_LEN + chunk.len());
            d.extend_from_slice(&(-2i32).to_le_bytes());
            d.extend_from_slice(&group.to_le_bytes());
            d.push(count);
            d.push(n as u8);
            d.extend_from_slice(&(SPLIT_SIZE as u16).to_le_bytes());
            d.extend_from_slice(chunk);
            d
        })
        .collect()
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
    fn split_parts_join_again() {
        let data: Vec<u8> = (0..3000).map(|i| (i % 251) as u8).collect();
        let parts = split(&data, 4);
        assert_eq!(parts.len(), 3);
        let mut r = SplitReassembler::default();
        let now = Instant::now();
        assert!(r.feed(&parts[2], now).is_none());
        assert!(r.feed(&parts[0], now).is_none());
        assert_eq!(r.feed(&parts[1], now).unwrap(), data);
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
