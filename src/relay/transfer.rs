//! A reliable transfer to a client: messages packed into fragments, sent a few at a time.

use crate::net::bits::{BitBuf, BitWriter};
use crate::net::compress;
use crate::net::netchan::Header;
use crate::net::reliable::{
    FRAGMENT_COUNT_BITS, FRAGMENT_SIZE, SIZE_BITS, START_FRAGMENT_BITS, SUBCHANNEL_BITS, SUBCHANNELS,
};

/// Reliable fragments sent per packet.
const FRAGMENTS_PER_PACKET: usize = 4;
/// Message bytes per reliable transfer, well under the engine's `NET_MAX_PAYLOAD`.
const MAX_TRANSFER: usize = 60_000;

/// One reliable transfer to a client, a few fragments per packet, on up to all eight
/// subchannels at once (as the engine sends one).
pub(super) struct Outgoing {
    data: Vec<u8>,
    /// Set when `data` is compressed: the size it inflates to.
    uncompressed: Option<u32>,
    frags: usize,
    /// The first fragment not sent yet.
    next: usize,
    /// The block in flight on each subchannel.
    inflight: [Option<InFlight>; SUBCHANNELS as usize],
    /// Blocks lost on the way (first fragment, count), to send again.
    lost: Vec<(usize, usize)>,
}

struct InFlight {
    start: usize,
    num: usize,
    /// The sequence of the packet that carried them.
    seq: i32,
}

impl Outgoing {
    #[cfg(test)]
    pub(super) fn data(&self) -> &[u8] {
        &self.data
    }

    /// A transfer of the messages at the front of `queue`, up to `MAX_TRANSFER` bytes.
    pub(super) fn take(queue: &mut Vec<BitBuf>) -> Self {
        let mut w = BitWriter::new();
        let mut taken = 0;
        for m in queue.iter() {
            if taken > 0 && (w.nbits() + m.nbits()).div_ceil(8) > MAX_TRANSFER {
                break;
            }
            w.write_buf(m);
            taken += 1;
        }
        queue.drain(..taken);
        // The bits after the last message are zeros: net_NOP, which a client skips.
        let mut data = w.into_bytes();
        let mut uncompressed = None;
        if data.len() > FRAGMENT_SIZE {
            let packed = compress::snappy_transfer(&data);
            if packed.len() < data.len() {
                uncompressed = Some(data.len() as u32);
                data = packed;
            }
        }
        let frags = data.len().div_ceil(FRAGMENT_SIZE);
        trace!("transfer of {} bytes ({:?} uncompressed), {frags} fragments", data.len(), uncompressed);
        Outgoing { data, uncompressed, frags, next: 0, inflight: Default::default(), lost: Vec::new() }
    }

    pub(super) fn done(&self) -> bool {
        self.next == self.frags && self.lost.is_empty() && self.inflight.iter().all(Option::is_none)
    }

    /// The next block to send in the packet of sequence `seq`, if one can go now. The block
    /// with the first fragment opens the transfer at the client, which refuses any other
    /// that comes before it: until it is acked, it goes alone.
    pub(super) fn next_block(&mut self, seq: i32) -> Option<BitBuf> {
        let sub = self.inflight.iter().position(Option::is_none)?;
        let opening = self.inflight.iter().flatten().any(|f| f.start == 0) || self.lost.iter().any(|b| b.0 == 0);
        let (start, num) = if let Some(i) = self.lost.iter().position(|b| !opening || b.0 == 0) {
            self.lost.remove(i)
        } else if !opening && self.next < self.frags {
            let block = (self.next, FRAGMENTS_PER_PACKET.min(self.frags - self.next));
            self.next += block.1;
            block
        } else {
            return None;
        };
        self.inflight[sub] = Some(InFlight { start, num, seq });
        Some(self.region(sub as u8, start, num))
    }

    /// Reads the acks of the blocks in flight from a packet header; `state` has a subchannel's
    /// bit flipped for each block acked (the engine's way).
    pub(super) fn acked(&mut self, h: &Header, state: &mut u8) {
        for (sub, slot) in self.inflight.iter_mut().enumerate() {
            let Some(f) = slot.take_if(|f| h.ack >= f.seq) else { continue };
            let bit = 1 << sub;
            if (*state ^ h.reliable_state) & bit == 0 {
                // The client got a later packet but not this block.
                self.lost.push((f.start, f.num));
            } else {
                *state ^= bit;
            }
        }
    }

    #[cfg(test)]
    pub(super) fn in_flight(&self, sub: usize) -> bool {
        self.inflight[sub].is_some()
    }

    /// The reliable region carrying fragments `start..start + num` on subchannel `sub`.
    fn region(&self, sub: u8, start: usize, num: usize) -> BitBuf {
        let single = self.frags <= FRAGMENTS_PER_PACKET;
        let mut w = BitWriter::new();
        w.write_bits(u32::from(sub), SUBCHANNEL_BITS);
        w.write_bit(true);
        w.write_bit(!single);
        if !single {
            w.write_bits(start as u32, START_FRAGMENT_BITS);
            w.write_bits(num as u32, FRAGMENT_COUNT_BITS);
        }
        if start == 0 {
            if !single {
                w.write_bit(false); // not a file
            }
            w.write_bit(self.uncompressed.is_some());
            if let Some(size) = self.uncompressed {
                w.write_bits(size, SIZE_BITS);
            }
            if single {
                w.write_varint32(self.data.len() as u32);
            } else {
                w.write_bits(self.data.len() as u32, SIZE_BITS);
            }
        }
        let end = ((start + num) * FRAGMENT_SIZE).min(self.data.len());
        w.write_bytes(&self.data[start * FRAGMENT_SIZE..end]);
        w.write_bit(false); // nothing on the file stream
        w.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::reliable::Reassembler;

    #[test]
    fn transfers_reassemble_a_few_fragments_at_a_time() {
        // Incompressible, so it goes out raw in five packets.
        let mut x = 1u32;
        let mut noise = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        };
        let mut queue: Vec<BitBuf> = (0..1300)
            .map(|_| {
                let mut w = BitWriter::new();
                w.write_bits(noise(), 32);
                w.finish()
            })
            .collect();
        let want: Vec<u8> = queue.iter().flat_map(|b| b.bytes().to_vec()).collect();
        let mut o = Outgoing::take(&mut queue);
        assert_eq!(queue.len(), 0);
        assert!(o.uncompressed.is_none());
        let mut r = Reassembler::default();
        let mut got = Vec::new();
        while o.next < o.frags {
            let num = FRAGMENTS_PER_PACKET.min(o.frags - o.next);
            got.extend(r.feed(o.region(0, o.next, num).bytes(), 0).unwrap().completed);
            o.next += num;
        }
        assert_eq!(got, [want]);
    }

    #[test]
    fn compressed_transfer_reassembles() {
        let mut queue = vec![BitBuf::from_bytes(vec![7u8; 5000])];
        let o = Outgoing::take(&mut queue);
        assert!(o.uncompressed.is_some() && o.frags <= FRAGMENTS_PER_PACKET, "{} fragments", o.frags);
        let got = Reassembler::default().feed(o.region(0, 0, o.frags).bytes(), 0).unwrap().completed;
        assert_eq!(got, [vec![7u8; 5000]]);
    }
}
