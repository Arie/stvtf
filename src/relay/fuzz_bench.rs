//! A relay with one client in, for `crate::fuzzing`.

use std::net::{SocketAddr, UdpSocket};
use std::time::Instant;

use super::RelayConfig;
use super::client::Client;
use super::hub::Hub;

/// A relay with one client in, for `crate::fuzzing`.
pub(crate) struct FuzzBench {
    pub(super) hub: Hub,
    pub(super) client: SocketAddr,
    pub(super) challenge: u32,
    pub(super) now: Instant,
}

impl FuzzBench {
    pub(crate) fn new() -> Self {
        let sock = UdpSocket::bind("127.0.0.1:0").expect("internal error: no local UDP port for the fuzz bench");
        let client: SocketAddr = SocketAddr::from(([127, 0, 0, 1], 9));
        let config = RelayConfig { password: String::new(), ..RelayConfig::new("127.0.0.1:0") };
        let mut hub = Hub::new(sock, &config);
        let now = Instant::now();
        let challenge = hub.admission.challenge(client);
        hub.clients.insert(client, Client::new("fuzz".to_string(), challenge, 1, now));
        FuzzBench { hub, client, challenge, now }
    }

    pub(crate) fn challenge(&self) -> u32 {
        self.challenge
    }

    pub(crate) fn datagram(&mut self, data: &[u8]) {
        self.hub.datagram(self.client, data, self.now);
    }

    pub(crate) fn service(&mut self) {
        self.hub.service(self.now);
    }
}
