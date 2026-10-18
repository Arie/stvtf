//! One receive path: datagram -> netchannel packet -> message bit streams.
//!
//! Per datagram: join `-2` split parts, inflate `-3 SNAP`, verify the checksum
//! before touching any state, drop stale sequences, parse the reliable region,
//! then cut the unreliable tail out at the bit where the reliable region ended.

use super::bits::BitReader;
use super::compress;
use super::netchan::{self, Header, Kind};
use super::reliable::{self, Reassembler};
use super::split::SplitReassembler;
use std::time::Instant;

pub struct MsgStream {
    pub reliable: bool,
    pub data: Vec<u8>,
    pub nbits: usize,
}

pub struct Received {
    pub header: Header,
    pub subchannel: Option<u8>,
    /// The reliable region parsed: the receiver acknowledges it by flipping its bit.
    pub reliable_ok: bool,
    /// Reliable streams first, then the unreliable tail, in processing order.
    pub streams: Vec<MsgStream>,
}

#[derive(Default, Debug, Clone)]
pub struct Counters {
    pub datagrams: u64,
    pub packets: u64,
    pub bad_checksum: u64,
    pub stale: u64,
    pub lost: u64,
    pub reliable_errors: u64,
    pub compressed_bad: u64,
}

#[derive(Default)]
pub struct Receiver {
    pub in_seq: i32,
    /// Only packets carrying this challenge belong to our connection.
    pub challenge: Option<u32>,
    splits: SplitReassembler,
    reasm: Reassembler,
    pub counters: Counters,
}

impl Receiver {
    pub fn new(challenge: Option<u32>) -> Self {
        Receiver { challenge, ..Default::default() }
    }

    pub fn feed(&mut self, datagram: &[u8], now: Instant) -> Option<Received> {
        self.counters.datagrams += 1;
        let joined;
        let mut data = datagram;
        if netchan::classify(data) == Kind::Split {
            joined = self.splits.feed(data, now)?;
            data = &joined;
        }
        let inflated;
        match netchan::classify(data) {
            Kind::InBand => {}
            Kind::Compressed => {
                if data.get(4..8) != Some(b"SNAP") {
                    self.counters.compressed_bad += 1;
                    return None;
                }
                if let Ok(v) = compress::snappy(&data[8..]) {
                    inflated = v;
                    data = &inflated;
                } else {
                    self.counters.compressed_bad += 1;
                    return None;
                }
            }
            _ => return None,
        }
        if !netchan::verify_checksum(data) {
            self.counters.bad_checksum += 1;
            return None;
        }
        let header = netchan::decode_header(data).ok()?;
        if let (Some(theirs), Some(ours)) = (header.challenge, self.challenge)
            && theirs != ours
        {
            return None;
        }
        if header.sequence <= self.in_seq {
            self.counters.stale += 1;
            return None;
        }
        if self.in_seq != 0 {
            let gap = (header.sequence - self.in_seq - 1).max(0) as u64;
            self.counters.lost += gap.saturating_sub(u64::from(header.choked.unwrap_or(0)));
        }
        self.in_seq = header.sequence;
        self.counters.packets += 1;

        let mut streams = Vec::new();
        let mut subchannel = None;
        let mut reliable_ok = false;
        let mut tail = Some(header.body_offset * 8);
        if header.reliable() {
            subchannel = reliable::latch_subchannel(data, header.body_offset);
            if let Ok(region) = self.reasm.feed(data, header.body_offset) {
                reliable_ok = true;
                for payload in region.completed {
                    let nbits = payload.len() * 8;
                    streams.push(MsgStream { reliable: true, data: payload, nbits });
                }
                tail = Some(region.end_bit);
            } else {
                self.counters.reliable_errors += 1;
                tail = None;
            }
        }
        if let Some(bit) = tail
            && bit < data.len() * 8
        {
            let (bytes, nbits) = BitReader::at(data, bit).read_remaining();
            streams.push(MsgStream { reliable: false, data: bytes, nbits });
        }
        Some(Received { header, subchannel, reliable_ok, streams })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{messages, netchan::build_packet};

    #[test]
    fn reliable_then_unreliable_streams() {
        let body = messages::signonstate(4, 9);
        let region = messages::reliable_region(&body, 2);
        let tick = messages::tick(1000);
        let pkt = build_packet(5, 0, 77, 0, Some((&region.0, region.1)), Some((&tick.0, tick.1)));
        let mut rx = Receiver::new(Some(77));
        let got = rx.feed(&pkt, Instant::now()).unwrap();
        assert!(got.reliable_ok);
        assert_eq!(got.subchannel, Some(2));
        assert_eq!(got.streams.len(), 2);
        assert!(got.streams[0].reliable);
        let mut r = BitReader::new(&got.streams[1].data);
        assert_eq!(r.read_bits(6).unwrap(), messages::NET_TICK);
        assert_eq!(r.read_i32().unwrap(), 1000);
    }

    #[test]
    fn stale_wrong_challenge_and_corrupt_are_dropped() {
        let tick = messages::tick(1);
        let mut rx = Receiver::new(Some(77));
        let p5 = build_packet(5, 0, 77, 0, None, Some((&tick.0, tick.1)));
        assert!(rx.feed(&p5, Instant::now()).is_some());
        assert!(rx.feed(&p5, Instant::now()).is_none(), "duplicate");
        let other = build_packet(6, 0, 78, 0, None, Some((&tick.0, tick.1)));
        assert!(rx.feed(&other, Instant::now()).is_none(), "other challenge");
        let mut bad = build_packet(7, 0, 77, 0, None, Some((&tick.0, tick.1)));
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(rx.feed(&bad, Instant::now()).is_none());
        assert_eq!(rx.counters.stale, 1);
        assert_eq!(rx.counters.bad_checksum, 1);
        let p9 = build_packet(9, 0, 77, 0, None, Some((&tick.0, tick.1)));
        assert!(rx.feed(&p9, Instant::now()).is_some());
        assert_eq!(rx.counters.lost, 3);
    }

    #[test]
    fn snap_compressed_split_datagram() {
        let tick = messages::tick(42);
        let inner = build_packet(3, 0, 1, 0, None, Some((&tick.0, tick.1)));
        let mut comp = (-3i32).to_le_bytes().to_vec();
        comp.extend_from_slice(b"SNAP");
        comp.extend_from_slice(&snap::raw::Encoder::new().compress_vec(&inner).unwrap());
        let split = comp.len().div_ceil(2);
        let mut parts = Vec::new();
        for (n, chunk) in comp.chunks(split).enumerate() {
            let mut d = (-2i32).to_le_bytes().to_vec();
            d.extend_from_slice(&11i32.to_le_bytes());
            d.push(2);
            d.push(n as u8);
            d.extend_from_slice(&(split as u16).to_le_bytes());
            d.extend_from_slice(chunk);
            parts.push(d);
        }
        let mut rx = Receiver::new(Some(1));
        assert!(rx.feed(&parts[1], Instant::now()).is_none());
        let got = rx.feed(&parts[0], Instant::now()).unwrap();
        let mut r = BitReader::new(&got.streams[0].data);
        assert_eq!(r.read_bits(6).unwrap(), messages::NET_TICK);
        assert_eq!(r.read_i32().unwrap(), 42);
    }
}
