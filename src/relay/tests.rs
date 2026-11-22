use std::net::{SocketAddr, UdpSocket};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::addr::masked_addr;
#[cfg(fuzzing)]
use super::client::Client;
use super::client::{Client, Phase};
use super::client_msg::{ClientMsg, client_messages};
use super::hub::{Hub, Mode, Part};
use super::serve::{Relay, RelayConfig};
use super::server_msg::{is_reset_hud, reset_hud, set_pause, vgui_menu};
use super::tuning::{
    CLIENT_TIMEOUT, FULL_UPDATE_GAP, FULL_UPDATE_GRACE, KEEPALIVE, NOTES_AFTER_ENTERING, NOTES_AFTER_RESET,
    SIGNON_HOLD, SIGNON_TIMEOUT,
};
use crate::decode::{Lifecycle, Span};
use crate::net::bits::{BitBuf, BitReader, BitWriter};
use crate::net::messages::{self, SVC_SETPAUSE, SVC_UPDATESTRINGTABLE, SVC_USERMESSAGE, Signon, TYPE_BITS};
use crate::net::netchan::{self, Header, OutHeader};
use crate::net::reliable::{Reassembler, SUBCHANNEL_BITS, SUBCHANNELS};
use crate::net::{MAX_DATAGRAM, handshake};
use crate::session::Tap;

/// A string table change the server sends in a datagram's unreliable part, and again in
/// the next ones until acked.
fn datagram_with_table_change() -> (BitBuf, Lifecycle) {
    let mut w = BitWriter::new();
    w.write_bits(SVC_UPDATESTRINGTABLE, TYPE_BITS);
    w.write_bits(0xabcd, 16);
    let bits = w.finish();
    let life = Lifecycle {
        spans: vec![Span { id: u8::try_from(SVC_UPDATESTRINGTABLE).unwrap(), bits: 0..bits.nbits() }],
        ..Lifecycle::default()
    };
    (bits, life)
}

/// A hub with a client in the game, and its address.
fn hub_with_one_in() -> (Hub, SocketAddr) {
    let config = RelayConfig::new("127.0.0.1:0");
    let mut hub = Hub::new(UdpSocket::bind("127.0.0.1:0").unwrap(), &config);
    let now = Instant::now();
    let active: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let mut c = Client::new("in".into(), 1, 1, now);
    c.gate = None;
    c.set_phase(Phase::Active, now);
    hub.clients.insert(active, c);
    (hub, active)
}

/// The relay's protocol from the client's side, without threads or clocks.
pub(super) struct Bench {
    pub(super) hub: Hub,
    pub(super) client: UdpSocket,
    pub(super) addr: SocketAddr,
    pub(super) challenge: u32,
    nonce: u32,
}

impl Bench {
    pub(super) fn new(password: &str, slots: usize) -> Self {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let config = RelayConfig { password: password.into(), slots, ..RelayConfig::new("127.0.0.1:0") };
        let hub = Hub::new(sock, &config);
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let addr = client.local_addr().unwrap();
        let challenge = hub.admission.challenge(addr);
        Bench { hub, client, addr, challenge, nonce: 7 }
    }

    fn connect_packet(&self, password: &str, protocol: i32) -> Vec<u8> {
        let mut p = handshake::build_connect(self.nonce, self.challenge, "me", password, "1");
        p[5..9].copy_from_slice(&protocol.to_le_bytes());
        p
    }

    fn connect_with(&mut self, packet: &[u8]) -> Vec<u8> {
        self.hub.datagram(self.addr, packet, Instant::now());
        self.reply()
    }

    pub(super) fn connect(&mut self) {
        let packet = self.connect_packet("", handshake::PROTOCOL_VERSION);
        assert_eq!(self.connect_with(&packet)[4], b'B', "accepted");
    }

    fn reply(&self) -> Vec<u8> {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        let n = self.client.recv(&mut buf).unwrap();
        buf.truncate(n);
        buf
    }

    pub(super) fn client_state(&mut self) -> &mut Client {
        self.hub.clients.get_mut(&self.addr).unwrap()
    }

    /// A channel packet from the client carrying `messages`.
    fn client_sends(&mut self, sequence: i32, messages: &BitBuf) {
        let header = OutHeader { sequence, ack: 0, challenge: self.challenge, reliable_state: 0 };
        let packet = netchan::build_packet(&header, None, Some(messages));
        self.hub.datagram(self.addr, &packet, Instant::now());
    }
}

/// What the bench's client is sent, read as a client reads it: each stream's messages.
fn sent_to(b: &Bench) -> Vec<ClientMsg> {
    b.client.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    let mut rx = crate::net::receiver::Receiver::new(Some(b.challenge));
    let mut buf = vec![0u8; MAX_DATAGRAM];
    let mut sent = Vec::new();
    while let Ok(n) = b.client.recv(&mut buf) {
        let Some(pkt) = rx.feed(&buf[..n], Instant::now()) else { continue };
        for s in &pkt.streams {
            sent.extend(client_messages(&s.bits));
        }
    }
    sent
}

mod clients;
mod sending;
mod signon;
