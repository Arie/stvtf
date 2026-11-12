//! Sending clients their packets.

use super::client::Client;
use crate::net::bits::BitBuf;
use crate::net::netchan::{self, OutHeader};
use crate::net::reliable::SUBCHANNELS;
use crate::net::{compress, split};
use std::net::{SocketAddr, UdpSocket};
use std::time::Instant;

/// The socket the clients are sent from, and the id of the last split packet sent on it.
pub(super) struct Out {
    pub(super) sock: UdpSocket,
    pub(super) split_group: i32,
}

impl Out {
    /// Sends a client a packet: `tail` and a transfer block if one can go, or nothing when
    /// neither is there and `always` is not set. Whether it sent one.
    pub(super) fn send(
        &mut self,
        addr: SocketAddr,
        c: &mut Client,
        tail: Option<&BitBuf>,
        always: bool,
        now: Instant,
    ) -> bool {
        let region = c.next_region(c.out_seq);
        if region.is_none() && tail.is_none() && !always {
            return false;
        }
        if tail.is_some() {
            c.sent_with_tail += 1;
        } else {
            c.sent_bare += 1;
        }
        let header = OutHeader {
            sequence: c.out_seq,
            ack: c.rx.in_seq,
            challenge: c.challenge,
            reliable_state: c.in_reliable_state,
        };
        c.out_seq += 1;
        c.last_sent = now;
        let mut pkt = netchan::build_packet(&header, region.as_ref(), tail);
        if pkt.len() > split::SPLIT_SIZE {
            pkt = compress::compress_datagram(&pkt);
        }
        // A send error is a client gone or unreachable: its timeout drops it.
        if pkt.len() > split::SPLIT_SIZE {
            self.split_group = self.split_group.wrapping_add(1);
            for part in split::split(&pkt, self.split_group) {
                let _ = self.sock.send_to(&part, addr);
            }
        } else {
            let _ = self.sock.send_to(&pkt, addr);
        }
        true
    }

    /// Sends a client every transfer block that can go now, one packet each; at least one packet
    /// when `always` is set.
    pub(super) fn burst(&mut self, addr: SocketAddr, c: &mut Client, always: bool, now: Instant) {
        let mut always = always;
        for _ in 0..SUBCHANNELS {
            if !self.send(addr, c, None, always, now) {
                break;
            }
            always = false;
        }
    }
}
