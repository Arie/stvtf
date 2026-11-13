//! Who is let in: challenges, connects and `A2S_INFO`, the packets outside a connection.

use super::client::Client;
use super::{MAX_SLOTS, RelayConfig, masked_addr};
use crate::net::handshake::{self, FromClient};
use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::net::SocketAddr;
use std::time::Instant;

pub(super) struct Admission {
    password: String,
    slots: usize,
    /// Keys the challenge of each address, so none need be stored before it connects.
    challenge_key: RandomState,
    /// What `A2S_INFO` says is played, and on which build.
    pub(super) map: String,
    pub(super) build: String,
    /// A client was let in, ever.
    pub(super) seen_client: bool,
}

impl Admission {
    pub(super) fn new(config: &RelayConfig) -> Self {
        Admission {
            password: config.password.clone(),
            slots: config.slots.min(MAX_SLOTS),
            challenge_key: RandomState::new(),
            map: String::new(),
            build: crate::CURRENT_BUILD.to_string(),
            seen_client: false,
        }
    }

    /// The challenge `addr` must give.
    pub(super) fn challenge(&self, addr: SocketAddr) -> u32 {
        self.challenge_key.hash_one(addr) as u32
    }

    /// The reply to connectionless packet `data` from `from`, if it gets one; a client let in
    /// joins `clients`. `port` says which relay it is in the log.
    pub(super) fn reply(
        &mut self,
        from: SocketAddr,
        data: &[u8],
        clients: &mut HashMap<SocketAddr, Client>,
        port: u16,
        now: Instant,
    ) -> Option<Vec<u8>> {
        let expected = self.challenge(from);
        Some(match handshake::parse_from_client(data)? {
            FromClient::GetChallenge { nonce } => handshake::build_challenge(expected, nonce),
            FromClient::Connect { protocol, challenge, nonce, name, password } => {
                if challenge != expected {
                    // Not from the address that got the challenge: no reply at all.
                    return None;
                }
                if let Some(c) = clients.get(&from)
                    && c.nonce == nonce
                {
                    // Our accept was lost and the client asks again.
                    handshake::build_accept(nonce)
                } else if protocol != handshake::PROTOCOL_VERSION {
                    handshake::build_reject(
                        nonce,
                        &format!("This relay speaks protocol {}", handshake::PROTOCOL_VERSION),
                    )
                } else if !self.password.is_empty() && password != self.password {
                    log!(":{port} {} ({}) gave a wrong password", name.escape_debug(), masked_addr(from));
                    handshake::build_reject(nonce, "#GameUI_ServerRejectBadPassword")
                } else if clients.len() >= self.slots && !clients.contains_key(&from) {
                    handshake::build_reject(nonce, "#GameUI_ServerRejectServerFull")
                } else {
                    log!(":{port} {} ({}) connected", name.escape_debug(), masked_addr(from));
                    clients.insert(from, Client::new(name, challenge, nonce, now));
                    self.seen_client = true;
                    handshake::build_accept(nonce)
                }
            }
            FromClient::Info { challenge } if challenge == Some(expected) => handshake::build_info(&handshake::Info {
                name: "stvtf relay",
                map: &self.map,
                players: clients.len() as u8,
                max_players: self.slots as u8,
                build: &self.build,
            }),
            FromClient::Info { .. } => handshake::build_info_challenge(expected),
        })
    }
}
