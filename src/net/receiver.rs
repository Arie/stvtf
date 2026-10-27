//! One receive path: datagram -> netchannel packet -> message bit streams.
//!
//! Per datagram: join `-2` split parts, inflate `-3 SNAP`, verify the checksum
//! before touching any state, drop stale sequences, parse the reliable region,
//! then cut the unreliable tail out at the bit where the reliable region ended.

use super::bits::{BitBuf, BitReader};
use super::compress;
use super::netchan::{self, Header, Kind};
use super::reliable::{self, Reassembler};
use super::split::SplitReassembler;
use std::time::Instant;

#[derive(Debug)]
pub struct MsgStream {
    pub bits: BitBuf,
    /// A completed reliable transfer, not the unreliable tail.
    pub reliable: bool,
}

#[derive(Debug)]
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
    pub bad_challenge: u64,
    pub lost: u64,
    pub reliable_errors: u64,
    pub compressed_bad: u64,
}

#[derive(Debug, Default)]
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
                // After the compressed marker: Snappy's magic and the block.
                let block = netchan::SEQUENCE_LEN + compress::SNAPPY_MAGIC.len();
                if data.get(netchan::SEQUENCE_LEN..block) != Some(compress::SNAPPY_MAGIC) {
                    self.counters.compressed_bad += 1;
                    return None;
                }
                if let Ok(v) = compress::snappy(&data[block..]) {
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
        // The server stamps every packet of the connection with our challenge; one
        // without it would let a spoofer skip the check by clearing the flag.
        if let Some(ours) = self.challenge
            && header.challenge != Some(ours)
        {
            self.counters.bad_challenge += 1;
            return None;
        }
        if header.sequence <= self.in_seq {
            self.counters.stale += 1;
            return None;
        }
        if self.in_seq != 0 {
            let gap = u64::try_from(header.sequence - self.in_seq - 1).unwrap_or(0);
            self.counters.lost += gap.saturating_sub(u64::from(header.choked.unwrap_or(0)));
        }
        self.in_seq = header.sequence;
        self.counters.packets += 1;

        let mut streams = Vec::new();
        let mut subchannel = None;
        let mut reliable_ok = false;
        let mut tail = Some(header.body_offset * 8);
        if header.reliable() {
            if let Ok(region) = self.reasm.feed(data, header.body_offset) {
                reliable_ok = true;
                subchannel = Some(region.subchannel);
                for payload in region.completed {
                    streams.push(MsgStream { bits: BitBuf::from_bytes(payload), reliable: true });
                }
                tail = Some(region.end_bit);
            } else {
                subchannel = reliable::latch_subchannel(data, header.body_offset);
                self.counters.reliable_errors += 1;
                tail = None;
            }
        }
        if let Some(bit) = tail
            && bit < data.len() * 8
        {
            streams.push(MsgStream { bits: BitReader::at(data, bit).read_remaining(), reliable: false });
        }
        Some(Received { header, subchannel, reliable_ok, streams })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::messages;
    use crate::net::netchan::{OutHeader, build_packet};

    fn packet(sequence: i32, challenge: u32, reliable: Option<&BitBuf>, unreliable: Option<&BitBuf>) -> Vec<u8> {
        build_packet(&OutHeader { sequence, ack: 0, challenge, reliable_state: 0 }, reliable, unreliable)
    }

    #[test]
    fn reliable_then_unreliable_streams() {
        let body = messages::signonstate(messages::Signon::PreSpawn, 9);
        let region = messages::reliable_region(&body, 2);
        let tick = messages::tick(1000);
        let pkt = packet(5, 77, Some(&region), Some(&tick));
        let mut rx = Receiver::new(Some(77));
        let got = rx.feed(&pkt, Instant::now()).unwrap();
        assert!(got.reliable_ok);
        assert_eq!(got.subchannel, Some(2));
        assert_eq!(got.streams.len(), 2);
        let mut r = BitReader::new(got.streams[0].bits.bytes());
        assert_eq!(r.read_bits(6).unwrap(), messages::NET_SIGNONSTATE);
        let mut r = BitReader::new(got.streams[1].bits.bytes());
        assert_eq!(r.read_bits(6).unwrap(), messages::NET_TICK);
        assert_eq!(r.read_i32().unwrap(), 1000);
    }

    #[test]
    fn stale_wrong_challenge_and_corrupt_are_dropped() {
        let tick = messages::tick(1);
        let mut rx = Receiver::new(Some(77));
        let p5 = packet(5, 77, None, Some(&tick));
        assert!(rx.feed(&p5, Instant::now()).is_some());
        assert!(rx.feed(&p5, Instant::now()).is_none(), "duplicate");
        let other = packet(6, 78, None, Some(&tick));
        assert!(rx.feed(&other, Instant::now()).is_none(), "other challenge");
        let mut bad = packet(7, 77, None, Some(&tick));
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(rx.feed(&bad, Instant::now()).is_none());
        assert_eq!(rx.counters.stale, 1);
        assert_eq!(rx.counters.bad_checksum, 1);
        let p9 = packet(9, 77, None, Some(&tick));
        assert!(rx.feed(&p9, Instant::now()).is_some());
        assert_eq!(rx.counters.lost, 3);
    }

    #[test]
    fn packet_without_challenge_is_dropped() {
        let tick = messages::tick(1);
        let mut pkt = packet(5, 77, None, Some(&tick));
        // Clear CHALLENGE, drop the 4 challenge bytes and fix up the checksum.
        pkt[8] &= !netchan::FLAG_CHALLENGE;
        pkt.drain(12..16);
        let sum = netchan::fold_checksum(&pkt[netchan::CHECKSUM_START..]);
        pkt[9..11].copy_from_slice(&sum.to_le_bytes());
        assert!(netchan::verify_checksum(&pkt));
        let mut rx = Receiver::new(Some(77));
        assert!(rx.feed(&pkt, Instant::now()).is_none());
        assert_eq!(rx.counters.bad_challenge, 1);
        assert_eq!(rx.in_seq, 0, "a dropped packet must not move the sequence");
    }

    #[test]
    fn snap_compressed_split_datagram() {
        let tick = messages::tick(42);
        let inner = packet(3, 1, None, Some(&tick));
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
        let mut r = BitReader::new(got.streams[0].bits.bytes());
        assert_eq!(r.read_bits(6).unwrap(), messages::NET_TICK);
        assert_eq!(r.read_i32().unwrap(), 42);
    }
}
