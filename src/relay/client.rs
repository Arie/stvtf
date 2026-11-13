//! A client of the relay: where it is in signon and what it is still to be sent.

use super::transfer::Outgoing;
use super::{ClientMsg, FULL_UPDATE_GRACE, NOTES_AFTER_ENTERING, client_messages};
use crate::net::bits::BitBuf;
use crate::net::messages::{self, Signon};
use crate::net::netchan::Header;
use crate::net::receiver::{Received, Receiver};
use std::net::SocketAddr;
use std::time::Instant;

/// A reliable message a client signing on needs, and the signon state it waits for after it.
pub(super) struct Logged {
    pub(super) bits: BitBuf,
    pub(super) gate: Option<Signon>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    /// Still taking the signon log.
    Signon,
    /// Ready for entities, waiting for a full update.
    WaitingFull,
    /// Getting every datagram.
    Active,
}

pub(super) struct Client {
    pub(super) name: String,
    pub(super) challenge: u32,
    /// The nonce of the client's connect: another one from the same address is a new connection.
    pub(super) nonce: u32,
    pub(super) rx: Receiver,
    pub(super) out_seq: i32,
    /// Our ack bits for the client's reliable subchannels.
    pub(super) in_reliable_state: u8,
    /// A subchannel's bit flips when the client acks a block of ours on it (the engine's way).
    out_reliable_state: u8,
    /// When the client connected, and what it was sent since the last trace.
    pub(super) connected: Instant,
    pub(super) sent_with_tail: u32,
    pub(super) sent_bare: u32,
    pub(super) phase: Phase,
    /// When `phase` last changed.
    pub(super) since: Instant,
    /// The next log entry to send.
    pub(super) cursor: usize,
    /// The signon state the client must echo before it gets more of the log.
    pub(super) gate: Option<Signon>,
    /// Reliable messages not yet in a transfer.
    pub(super) queue: Vec<BitBuf>,
    /// When to give it the notes again: once its HUD is up, and past a HUD reset (see
    /// [`Hub::notes`](super::hub::Hub::notes)). Each its own: a reset must not put off or bring
    /// forward the other.
    pub(super) notes_entered: Option<Instant>,
    pub(super) notes_reset: Option<Instant>,
    pub(super) outgoing: Option<Outgoing>,
    pub(super) last_heard: Instant,
    pub(super) last_sent: Instant,
    /// The client said goodbye.
    pub(super) gone: Option<String>,
    /// It went on past CONNECTED in the current signon: a new signon must make it reconnect.
    pub(super) signing: bool,
    /// Told to reconnect, and not yet said it has.
    pub(super) reconnecting: bool,
}

impl Client {
    pub(super) fn new(name: String, challenge: u32, nonce: u32, now: Instant) -> Self {
        Client {
            name,
            challenge,
            nonce,
            rx: Receiver::new(Some(challenge)),
            out_seq: 1,
            in_reliable_state: 0,
            out_reliable_state: 0,
            connected: now,
            sent_with_tail: 0,
            sent_bare: 0,
            phase: Phase::Signon,
            since: now,
            cursor: 0,
            // The client opens with net_SignonState(CONNECTED); then the log starts.
            gate: Some(Signon::Connected),
            queue: Vec::new(),
            notes_entered: None,
            notes_reset: None,
            outgoing: None,
            last_heard: now,
            last_sent: now,
            gone: None,
            signing: false,
            reconnecting: false,
        }
    }

    pub(super) fn set_phase(&mut self, phase: Phase, now: Instant) {
        self.phase = phase;
        self.since = now;
    }

    /// Queues the log from the cursor up to the next signon state the client must echo.
    pub(super) fn pump(&mut self, log: &[Logged]) {
        while self.gate.is_none()
            && let Some(m) = log.get(self.cursor)
        {
            self.queue.push(m.bits.clone());
            self.cursor += 1;
            self.gate = m.gate;
        }
    }

    /// Whether the client is through the log and gets messages as they come.
    pub(super) fn caught_up(&self, log: &[Logged]) -> bool {
        self.phase != Phase::Signon && self.cursor == log.len()
    }

    /// Whether the client has acked the whole log. Only then can it take entities: those of a
    /// class whose baseline it lacks stop a game client.
    pub(super) fn has_log(&self, log: &[Logged]) -> bool {
        self.cursor == log.len() && self.queue.is_empty() && self.outgoing.is_none()
    }

    /// The client goes into the game: it gets `notes`, and again once its HUD is up (sent with its
    /// first entities they may come before it).
    pub(super) fn enter_notes(&mut self, notes: &[BitBuf], now: Instant) {
        self.queue.extend(notes.iter().cloned());
        self.notes_entered = Some(now + NOTES_AFTER_ENTERING);
    }

    /// Whether the client waits for a full update and can take one.
    pub(super) fn takes_full_update(&self, log: &[Logged]) -> bool {
        self.phase == Phase::WaitingFull && self.has_log(log)
    }

    /// The region to send in the packet of sequence `seq`, if a transfer block can go.
    pub(super) fn next_region(&mut self, seq: i32) -> Option<BitBuf> {
        if self.outgoing.is_none() && !self.queue.is_empty() {
            self.outgoing = Some(Outgoing::take(&mut self.queue));
        }
        self.outgoing.as_mut()?.next_block(seq)
    }

    /// Takes a packet from the client: its acks, its signon states and its goodbye. Whether it
    /// is now ready for entities: through its signon, or in the game and asking for a full update
    /// again after [`FULL_UPDATE_GRACE`].
    pub(super) fn read_packet(&mut self, from: SocketAddr, pkt: &Received, now: Instant) -> bool {
        self.last_heard = now;
        if pkt.header.reliable()
            && pkt.reliable_ok
            && let Some(sub) = pkt.subchannel
        {
            self.in_reliable_state ^= 1 << sub;
        }
        self.acked(&pkt.header);
        let mut ready = false;
        for stream in &pkt.streams {
            for m in client_messages(&stream.bits) {
                match m {
                    ClientMsg::Signon(s) => {
                        trace!("{from} signon {s:?} at {} ms", now.duration_since(self.connected).as_millis());
                        self.signing |= s > Signon::Connected;
                        self.reconnecting &= s < Signon::Connected;
                        if self.gate.is_some_and(|g| s >= g) {
                            self.gate = None;
                        }
                        ready |= s >= Signon::Spawn && self.phase == Phase::Signon && self.gate.is_none();
                    }
                    ClientMsg::Tick(messages::FULL_UPDATE_TICK) => {
                        ready |= self.phase == Phase::Active && now.duration_since(self.since) > FULL_UPDATE_GRACE;
                    }
                    ClientMsg::Tick(_) => {}
                    ClientMsg::Disconnect(reason) => self.gone = Some(reason),
                }
            }
        }
        ready
    }

    /// Reads the client's acks of our transfer blocks from a packet header.
    pub(super) fn acked(&mut self, h: &Header) {
        let Some(o) = &mut self.outgoing else { return };
        o.acked(h, &mut self.out_reliable_state);
        if o.done() {
            self.outgoing = None;
        }
    }

    /// Tells the client to reconnect with `reconnect` (`net_SignonState(CONNECTED)`) in place of
    /// whatever it was still to be sent; it gets no more of the log until it says it has.
    pub(super) fn ask_to_reconnect(&mut self, reconnect: &BitBuf) {
        self.queue.clear();
        self.queue.push(reconnect.clone());
        self.gate = Some(Signon::Connected);
        self.signing = false;
        self.reconnecting = true;
    }

    /// Told to reconnect, or sent part of a signon, and not through it: another signon would crash
    /// it.
    pub(super) fn in_signon(&self) -> bool {
        self.reconnecting
            || (self.phase == Phase::Signon
                && (self.signing || self.cursor > 0 || !self.queue.is_empty() || self.outgoing.is_some()))
    }

    /// Still signing on, with more to send or an echo to wait for: a paced replay holds for it.
    pub(super) fn signing_on(&self) -> bool {
        self.phase == Phase::Signon && (self.gate.is_some() || !self.queue.is_empty() || self.outgoing.is_some())
    }
}
