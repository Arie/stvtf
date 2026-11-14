//! A playing relay's clients handed to the process after it: where each connection is and all
//! they were sent since their signon, so the next relay goes on with a full update and no signon.

use super::backlog::Backlog;
use super::client::{Client, Phase};
use super::hub::Hub;
use super::out::Empty;
use crate::decode::ModelsSent;
use crate::net::bits::{BitBuf, BitReader};
use crate::net::messages::{self, Signon};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Instant;

/// What a client left out of a handover is told, in its console.
const RESTARTING: &str = "stv.tf: restarting, retry";

/// A relay's clients and what they have, for [`Relay::take_over`](super::Relay::take_over).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Handover {
    clients: Vec<HandedClient>,
    backlog: Vec<HandedEntry>,
    /// What `A2S_INFO` said was played, and on which build.
    map: String,
    build: String,
    /// Where playback was; none when no demo played.
    pub(super) played: Option<Played>,
}

impl Handover {
    /// How many clients are handed over.
    pub fn clients(&self) -> usize {
        self.clients.len()
    }
}

/// A client's connection: who it is and where its packets and ours are.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HandedClient {
    addr: SocketAddr,
    name: String,
    challenge: u32,
    nonce: u32,
    out_seq: i32,
    in_seq: i32,
    in_reliable: u8,
    out_reliable: u8,
}

/// A backlog entry: its bits in base64, how many, and the signon state it waits for.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HandedEntry {
    bits: String,
    nbits: usize,
    gate: Option<u8>,
}

/// Where the player was: the demo the clients are signed on to, the tick of its last frame, and
/// the models they have.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Played {
    pub(super) demo: PathBuf,
    pub(super) tick: i64,
    pub(super) laser: Option<u16>,
    pub(super) models: ModelsSent,
    pub(super) match_laser: Option<PathBuf>,
}

/// Clients were taken over, and where their playback was, if a demo played.
pub(super) struct TakenOver(pub(super) Option<Played>);

impl Client {
    /// In the game with nothing of ours on its way: the next relay can go on with it.
    fn at_rest(&self, backlog: &Backlog) -> bool {
        self.phase != Phase::Signon && !self.reconnecting && self.gone.is_none() && self.through_backlog(backlog)
    }
}

impl Hub {
    /// Whether every client is at rest (see [`Hub::hand_over`]).
    pub(super) fn all_at_rest(&self) -> bool {
        self.clients.values().all(|c| c.at_rest(&self.backlog))
    }

    /// Hands the clients at rest over, with `played`, and goes silent: nothing more is sent or
    /// read. A client still in its signon or a transfer is told to retry.
    pub(super) fn hand_over(&mut self, played: Option<Played>) -> Handover {
        let now = Instant::now();
        let b64 = base64::engine::general_purpose::STANDARD;
        let bye = messages::disconnect(RESTARTING);
        let mut clients = Vec::new();
        for (addr, c) in &mut self.clients {
            if c.at_rest(&self.backlog) {
                clients.push(HandedClient {
                    addr: *addr,
                    name: c.name.clone(),
                    challenge: c.challenge,
                    nonce: c.nonce,
                    out_seq: c.out_seq,
                    in_seq: c.rx.in_seq,
                    in_reliable: c.in_reliable_state,
                    out_reliable: c.out_reliable_state(),
                });
            } else {
                self.out.send(*addr, c, Some(&bye), Empty::Send, now);
            }
        }
        self.clients.clear();
        self.silent = true;
        let backlog = self
            .backlog
            .entries()
            .map(|e| HandedEntry {
                bits: b64.encode(e.bits.bytes()),
                nbits: e.bits.nbits(),
                gate: e.gate.map(|g| g as u8),
            })
            .collect();
        Handover { clients, backlog, map: self.admission.map.clone(), build: self.admission.build.clone(), played }
    }

    /// Takes the clients of `h` in, waiting for a full update with all of its backlog had. None
    /// if the backlog does not read.
    pub(super) fn take_over(&mut self, h: &Handover, now: Instant) -> Option<()> {
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut backlog = Backlog::new();
        for e in &h.backlog {
            let bytes = b64.decode(&e.bits).ok().filter(|b| e.nbits <= b.len() * 8)?;
            let gate = match e.gate {
                Some(g) => Some(Signon::from_u8(g)?),
                None => None,
            };
            backlog.restore(BitReader::slice(&bytes, 0..e.nbits), gate);
        }
        backlog.open = false;
        self.backlog = backlog;
        self.admission.map.clone_from(&h.map);
        self.admission.build.clone_from(&h.build);
        for hc in &h.clients {
            let mut c = Client::new(hc.name.clone(), hc.challenge, hc.nonce, now);
            c.out_seq = hc.out_seq;
            c.rx.in_seq = hc.in_seq;
            c.in_reliable_state = hc.in_reliable;
            c.set_out_reliable_state(hc.out_reliable);
            c.cursor = self.backlog.len();
            c.gate = None;
            c.past_connected = true;
            c.set_phase(Phase::WaitingFull, now);
            self.clients.insert(hc.addr, c);
        }
        Some(())
    }
}

/// A table change's bits, for the backlog to know it by.
pub(super) fn is_table_change(bits: &BitBuf) -> bool {
    BitReader::new(bits.bytes())
        .read_bits(messages::TYPE_BITS)
        .is_ok_and(|id| u8::try_from(id).is_ok_and(super::server_msg::is_table_change))
}
