//! The hub: the clients, the signon log and the datagrams, shared by the upstream and client
//! threads.

use super::client::{Client, Logged, Phase};
use super::out::Out;
use super::server_msg::{is_table_change, set_pause, show_panel, signon_state, table_changes};
use super::{
    CLIENT_TIMEOUT, ClientMsg, FULL_UPDATE_GRACE, KEEPALIVE, MAX_KEYFRAME, MAX_SLOTS, NOTES_AFTER_RESET, RelayConfig,
    SIGNON_HOLD, SIGNON_TIMEOUT, SPECTATORS, TRACE, TRACE_EVERY, client_messages, whence,
};
use crate::decode::Lifecycle;
use crate::net::bits::{BitBuf, BitReader, BitWriter};
use crate::net::handshake::{self, FromClient};
use crate::net::messages::{self, SVC_PACKETENTITIES, SVC_SERVERINFO, Signon};
use crate::net::netchan;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};
use std::net::{SocketAddr, UdpSocket};
use std::time::Instant;

/// What a client dropped for a signon it did not finish in time is told, in its console.
const SLOW_SIGNON: &str = "stv.tf: took too long to load, retry";

/// Everything the upstream and client threads share.
pub(super) struct Hub {
    out: Out,
    /// The port it listens on: which relay a log line is of.
    port: u16,
    password: String,
    slots: usize,
    /// Keys the challenge of each address, so none need be stored before it connects.
    pub(super) challenge_key: RandomState,
    pub(super) clients: HashMap<SocketAddr, Client>,
    pub(super) log: Vec<Logged>,
    /// The string table messages logged from unreliable parts, which come again until acked.
    logged_unreliable: HashSet<BitBuf>,
    /// Every reliable message joins the log until the first entities after a signon starts.
    pub(super) signon_open: bool,
    /// This datagram's unreliable part, and whether it holds a full update.
    tail: Option<BitBuf>,
    tail_full: bool,
    /// The unreliable parts since the last full update, while there are few.
    pub(super) keyframe: Option<Vec<BitBuf>>,
    /// Messages a client gets again, reliably, after every full update: what one drops (a room's
    /// note bubbles).
    pub(super) notes: Vec<BitBuf>,
    /// Sent once with the next full update.
    with_next: Vec<BitBuf>,
    map: String,
    pub(super) build: String,
    seen_client: bool,
    /// When the last full update came, to space out asking for more.
    pub(super) last_full: Option<Instant>,
    /// Passing a server on, or playing a demo (see [`play`](super::play)), paused or not.
    pub(super) mode: Mode,
    /// Since when another signon waits for the clients in one.
    signon_held: Option<Instant>,
    traced: Instant,
}

/// What feeds a hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// A server, which sends reliable data again until acked.
    Live,
    /// A demo, which has each message once: what a client must not lose goes reliably too.
    Play { paused: bool },
}

impl Hub {
    pub(super) fn new(sock: UdpSocket, config: &RelayConfig) -> Self {
        Hub {
            port: sock.local_addr().map_or(0, |a| a.port()),
            out: Out { sock, split_group: 0 },
            password: config.password.clone(),
            slots: config.slots.min(MAX_SLOTS),
            challenge_key: RandomState::new(),
            clients: HashMap::new(),
            log: Vec::new(),
            logged_unreliable: HashSet::new(),
            signon_open: true,
            tail: None,
            tail_full: false,
            keyframe: None,
            notes: Vec::new(),
            with_next: Vec::new(),
            map: String::new(),
            build: crate::CURRENT_BUILD.to_string(),
            seen_client: false,
            last_full: None,
            mode: Mode::Live,
            signon_held: None,
            traced: Instant::now(),
        }
    }

    /// The server started a signon (connected, or a new map): so does every client. A reconnect
    /// asked of a client (see [`Hub::reconnect_all`]) still goes first.
    pub(super) fn restart_signon(&mut self, now: Instant) {
        self.forget();
        let reconnect = messages::signonstate(Signon::Connected, -1);
        let play = matches!(self.mode, Mode::Play { .. });
        for c in self.clients.values_mut() {
            if play && c.in_signon() && !c.reconnecting {
                // Part-way through the last demo's, come in as the switch went: it reconnects too.
                c.ask_to_reconnect(&reconnect);
            } else {
                c.queue.retain(|m| *m == reconnect);
                c.gate = c.gate.filter(|g| *g == Signon::Connected);
            }
            c.cursor = 0;
            c.set_phase(Phase::Signon, now);
        }
    }

    /// Every client into a signon is told to reconnect (`net_SignonState(CONNECTED)`, as a server's
    /// `Reconnect` does, and as a live server does on a new map), and gets the next signon once it
    /// says it has: a game client sent a new signon in the middle of a game crashes.
    pub(super) fn reconnect_all(&mut self) {
        let reconnect = messages::signonstate(Signon::Connected, -1);
        for c in self.clients.values_mut().filter(|c| c.signing || c.phase != Phase::Signon) {
            c.ask_to_reconnect(&reconnect);
        }
    }

    pub(super) fn upstream_stream(&mut self, bits: &BitBuf, reliable: bool, life: &Lifecycle) {
        let now = Instant::now();
        if let Some((_, map)) = &life.server_info {
            self.map.clone_from(map);
        }
        // The server sends the first full update after signon reliably (`net_Tick` and the
        // entities): like any entities, it goes to the clients in the datagram's unreliable
        // part, so it is never in the log and reaches a client in order with the deltas after it.
        let entities = life.spans.iter().any(|s| u32::from(s.id) == SVC_PACKETENTITIES);
        if !reliable || entities {
            if entities {
                self.signon_open = false;
            }
            self.tail_full |= life.full_update;
            let mut w = BitWriter::new();
            if let Some(earlier) = &self.tail {
                w.write_buf(earlier);
            }
            w.write_buf(&life.cut(bits));
            self.tail = Some(w.finish());
            // The server sends string table changes (new instance baselines among them) in the
            // unreliable part, again in each datagram until acked. A client that joins later
            // needs them: without an entity class's baseline, a game client stops with
            // "GetDynamicBaseline: FindStringIndex failed".
            for msg in table_changes(bits, life) {
                self.log_unreliable(&msg);
            }
            return;
        }
        for span in &life.spans {
            let msg = BitReader::slice(bits.bytes(), span.bits.clone());
            if u32::from(span.id) == SVC_SERVERINFO {
                self.restart_signon(now);
            }
            if self.signon_open || is_table_change(span.id) {
                let gate =
                    signon_state(span.id, &msg).filter(|s| matches!(s, Signon::New | Signon::PreSpawn | Signon::Spawn));
                self.log.push(Logged { bits: msg, gate });
                for c in self.clients.values_mut() {
                    c.pump(&self.log);
                }
            } else {
                for c in self.clients.values_mut().filter(|c| c.caught_up(&self.log)) {
                    c.queue.push(msg.clone());
                }
            }
        }
    }

    /// Logs a string table message from a datagram's unreliable part, once. A client already
    /// getting every datagram has it from there, so its cursor only moves past it.
    fn log_unreliable(&mut self, msg: &BitBuf) {
        if !self.logged_unreliable.insert(msg.clone()) {
            return;
        }
        trace!("logged a string table change of {} bits from a datagram", msg.nbits());
        self.log.push(Logged { bits: msg.clone(), gate: None });
        let play = matches!(self.mode, Mode::Play { .. });
        for c in self.clients.values_mut() {
            if c.phase == Phase::Active && c.cursor + 1 == self.log.len() {
                c.cursor += 1;
                // A demo has it once: a lost datagram would lose it for good.
                if play {
                    c.queue.push(msg.clone());
                }
            } else {
                c.pump(&self.log);
            }
        }
    }

    /// A stream passed over on a jump: only its string table changes count, which every client
    /// needs, so they go into the log and reliably to the clients already in.
    pub(super) fn skim(&mut self, bits: &BitBuf, life: &Lifecycle) {
        let tables: Vec<BitBuf> = table_changes(bits, life).collect();
        self.log_tables(&tables);
    }

    /// Playback went back past the string table changes `later`: they are logged again when it
    /// comes to them, and `back` (what sets the clients' tables back) goes to every client, even
    /// if an earlier jump back sent the same.
    pub(super) fn set_tables_back(&mut self, later: &[BitBuf], back: &[BitBuf]) {
        for t in later.iter().chain(back) {
            self.logged_unreliable.remove(t);
        }
        self.log_tables(back);
    }

    /// String table changes passed over (a jump), into the log and reliably to the clients
    /// already in; those logged already are not again.
    pub(super) fn log_tables(&mut self, tables: &[BitBuf]) {
        self.keyframe = None;
        for msg in tables {
            if !self.logged_unreliable.insert(msg.clone()) {
                continue;
            }
            self.log.push(Logged { bits: msg.clone(), gate: None });
            for c in self.clients.values_mut() {
                if c.phase != Phase::Signon && c.cursor + 1 == self.log.len() {
                    c.queue.push(msg.clone());
                    c.cursor += 1;
                } else {
                    c.pump(&self.log);
                }
            }
        }
    }

    /// Logs `msg` for every client, those in and those to come, unless it is already.
    pub(super) fn log_once(&mut self, msg: &BitBuf) {
        if !self.logged_unreliable.contains(msg) {
            self.log_tables(std::slice::from_ref(msg));
        }
    }

    /// Whether a client is in the game.
    pub(super) fn any_in_game(&self) -> bool {
        self.clients.values().any(|c| c.phase == Phase::Active)
    }

    /// Whether a client is ready for entities and waits for a full update.
    pub(super) fn wants_keyframe(&self) -> bool {
        self.clients.values().any(|c| c.takes_full_update(&self.log))
    }

    /// Every client in the game needs a full update: playback jumped, paused or went on, or came
    /// to a frame they cannot follow.
    pub(super) fn all_want_keyframe(&mut self, now: Instant) {
        self.keyframe = None;
        for c in self.clients.values_mut().filter(|c| c.phase == Phase::Active) {
            c.set_phase(Phase::WaitingFull, now);
        }
    }

    /// A full update (`net_Tick` and the entities) and the frames played since the one it is of,
    /// for the clients waiting for one, who are told if playback is `paused`. They are kept, with
    /// the frames after, for clients that come soon after.
    pub(super) fn send_keyframe(&mut self, key: &BitBuf, since: &[BitBuf], paused: bool) {
        let now = Instant::now();
        self.signon_open = false;
        self.keyframe = Some(std::iter::once(key.clone()).chain(since.iter().cloned()).collect());
        // Always: one shown "paused" before a jump may not be any more.
        let pause = set_pause(paused);
        // TF2's player bar freezes until its clock passes its last draw; re-showing resets that.
        let again = std::mem::take(&mut self.with_next);
        for (addr, c) in &mut self.clients {
            if c.takes_full_update(&self.log) {
                trace!("{addr} gets a keyframe of {} bits", key.nbits());
                c.set_phase(Phase::Active, now);
                c.queue.push(pause.clone());
                c.queue.extend(again.iter().cloned());
                c.enter_notes(&self.notes, now);
                self.out.send(*addr, c, Some(key), true, now);
                for f in since {
                    self.out.send(*addr, c, Some(f), true, now);
                }
            }
        }
    }

    /// The next full update sets clients' clocks back.
    pub(super) fn goes_back(&mut self) {
        self.with_next = vec![show_panel(SPECTATORS, false), show_panel(SPECTATORS, true)];
    }

    /// What a client gets after every full update from now on, in place of what it got before.
    pub(super) fn set_notes(&mut self, notes: Vec<BitBuf>) {
        self.notes = notes;
    }

    /// The clients in the game had their HUD reset, which drops the notes: they get them again in
    /// a moment, past it (a reset in the same packet is read after its reliable part).
    pub(super) fn notes_again(&mut self, now: Instant) {
        for c in self.clients.values_mut().filter(|c| c.phase == Phase::Active) {
            c.notes_reset = Some(now + NOTES_AFTER_RESET);
        }
    }

    /// The notes, reliably, to the clients in the game due them again.
    pub(super) fn send_due_notes(&mut self, now: Instant) {
        for c in self.clients.values_mut().filter(|c| c.phase == Phase::Active) {
            for at in [&mut c.notes_entered, &mut c.notes_reset] {
                if at.is_some_and(|t| now >= t) {
                    *at = None;
                    c.queue.extend(self.notes.iter().cloned());
                }
            }
        }
    }

    /// Tells the clients in the game that playback paused or went on, reliably.
    pub(super) fn set_paused(&mut self, paused: bool) {
        self.mode = Mode::Play { paused };
        self.tell_active(&set_pause(paused));
    }

    /// A message for the clients in the game now, unreliably: what is sent again at a pace, which
    /// must not wait for the reliable stream.
    pub(super) fn tell_active_now(&mut self, msg: &BitBuf) {
        let now = Instant::now();
        for (addr, c) in self.clients.iter_mut().filter(|(_, c)| c.phase == Phase::Active) {
            self.out.send(*addr, c, Some(msg), false, now);
        }
    }

    /// A message for the clients waiting for a full update, unreliably.
    pub(super) fn tell_waiting_now(&mut self, msg: &BitBuf) {
        let now = Instant::now();
        for (addr, c) in &mut self.clients {
            if c.takes_full_update(&self.log) {
                self.out.send(*addr, c, Some(msg), false, now);
            }
        }
    }

    /// A message for the clients in the game, reliably.
    pub(super) fn tell_active(&mut self, msg: &BitBuf) {
        for c in self.clients.values_mut().filter(|c| c.phase == Phase::Active) {
            c.queue.push(msg.clone());
        }
    }

    /// What was logged goes: the next signon starts afresh.
    pub(super) fn forget(&mut self) {
        self.log.clear();
        self.logged_unreliable.clear();
        // Of the last demo's entities: the player gives them again.
        self.notes.clear();
        self.signon_open = true;
        self.keyframe = None;
    }

    /// Sends what the upstream datagram brought.
    pub(super) fn upstream_flush(&mut self) {
        let now = Instant::now();
        let tail = self.tail.take();
        let full = std::mem::take(&mut self.tail_full);
        if full {
            self.last_full = Some(now);
        }
        if let Some(t) = &tail {
            if full {
                self.keyframe = Some(vec![t.clone()]);
            } else if let Some(k) = &mut self.keyframe {
                if k.len() < MAX_KEYFRAME {
                    k.push(t.clone());
                } else {
                    self.keyframe = None;
                }
            }
        }
        for (addr, c) in &mut self.clients {
            if full && c.takes_full_update(&self.log) {
                trace!("{addr} gets a full update of {} bits", tail.as_ref().map_or(0, BitBuf::nbits));
                c.set_phase(Phase::Active, now);
                c.enter_notes(&self.notes, now);
            }
            if c.phase == Phase::Active {
                // One packet per server datagram, as the server sends: a packet of our own
                // would show in a client's net_graph as one without entities.
                self.out.send(*addr, c, tail.as_ref(), true, now);
            } else {
                self.out.burst(*addr, c, false, now);
            }
        }
    }

    /// A client is ready for entities: from the kept datagrams if there are, or a full update.
    pub(super) fn wants_entities(&mut self, addr: SocketAddr, now: Instant) {
        let Some(c) = self.clients.get_mut(&addr) else { return };
        if let Some(frames) = &self.keyframe
            && c.has_log(&self.log)
        {
            trace!("{addr} gets entities from the {} datagrams kept since the last full update", frames.len());
            c.set_phase(Phase::Active, now);
            if let Mode::Play { paused } = self.mode {
                c.queue.push(set_pause(paused));
            }
            c.enter_notes(&self.notes, now);
            for t in frames {
                self.out.send(addr, c, Some(t), false, now);
            }
        } else {
            trace!("{addr} waits for a full update");
            c.set_phase(Phase::WaitingFull, now);
        }
    }

    pub(super) fn datagram(&mut self, from: SocketAddr, data: &[u8], now: Instant) {
        if netchan::classify(data) == netchan::Kind::Oob {
            self.connectionless(from, data, now);
            return;
        }
        let Some(c) = self.clients.get_mut(&from) else { return };
        let Some(pkt) = c.rx.feed(data, now) else { return };
        c.last_heard = now;
        if pkt.header.reliable()
            && pkt.reliable_ok
            && let Some(sub) = pkt.subchannel
        {
            c.in_reliable_state ^= 1 << sub;
        }
        c.acked(&pkt.header);
        let mut ready = false;
        for stream in &pkt.streams {
            for m in client_messages(&stream.bits) {
                match m {
                    ClientMsg::Signon(s) => {
                        trace!("{from} signon {s:?} at {} ms", now.duration_since(c.connected).as_millis());
                        c.signing |= s > Signon::Connected;
                        c.reconnecting &= s < Signon::Connected;
                        if c.gate.is_some_and(|g| s >= g) {
                            c.gate = None;
                        }
                        ready |= s >= Signon::Spawn && c.phase == Phase::Signon && c.gate.is_none();
                    }
                    ClientMsg::Tick(messages::FULL_UPDATE_TICK) => {
                        ready |= c.phase == Phase::Active && now.duration_since(c.since) > FULL_UPDATE_GRACE;
                    }
                    ClientMsg::Tick(_) => {}
                    ClientMsg::Disconnect(reason) => c.gone = Some(reason),
                }
            }
        }
        if let Some(reason) = &c.gone {
            log!(":{} {} ({}) left: {}", self.port, c.name.escape_debug(), whence(from), reason.escape_debug());
            self.clients.remove(&from);
            return;
        }
        // Not started on a signon the next demo waits to replace: it would not get through it.
        if self.signon_held.is_none() || c.in_signon() || c.phase != Phase::Signon {
            c.pump(&self.log);
        }
        // An active client hears from us with the next server datagram. During signon its acks
        // free subchannels for more of the transfer, and its reliable data is acked at once.
        if c.phase != Phase::Active {
            self.out.burst(from, c, pkt.header.reliable(), now);
        }
        if ready {
            self.wants_entities(from, now);
        }
    }

    fn connectionless(&mut self, from: SocketAddr, data: &[u8], now: Instant) {
        trace!(
            "{from} out of band {:?} ({} bytes): {:?}",
            data.get(4).map(|&b| char::from(b)),
            data.len(),
            String::from_utf8_lossy(&data[5.min(data.len())..data.len().min(40)])
        );
        let expected = self.challenge_key.hash_one(from) as u32;
        let reply = match handshake::parse_from_client(data) {
            Some(FromClient::GetChallenge { nonce }) => handshake::build_challenge(expected, nonce),
            Some(FromClient::Connect { protocol, challenge, nonce, name, password }) => {
                if challenge != expected {
                    // Not from the address that got the challenge: no reply at all.
                    return;
                }
                if let Some(c) = self.clients.get(&from)
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
                    log!(":{} {} ({}) gave a wrong password", self.port, name.escape_debug(), whence(from));
                    handshake::build_reject(nonce, "#GameUI_ServerRejectBadPassword")
                } else if self.clients.len() >= self.slots && !self.clients.contains_key(&from) {
                    handshake::build_reject(nonce, "#GameUI_ServerRejectServerFull")
                } else {
                    log!(":{} {} ({}) connected", self.port, name.escape_debug(), whence(from));
                    self.clients.insert(from, Client::new(name, challenge, nonce, now));
                    self.seen_client = true;
                    handshake::build_accept(nonce)
                }
            }
            Some(FromClient::Info { challenge }) if challenge == Some(expected) => {
                handshake::build_info(&handshake::Info {
                    name: "stvtf relay",
                    map: &self.map,
                    players: self.clients.len() as u8,
                    max_players: self.slots as u8,
                    build: &self.build,
                })
            }
            Some(FromClient::Info { .. }) => handshake::build_info_challenge(expected),
            None => return,
        };
        let _ = self.out.sock.send_to(&reply, from);
    }

    /// Resends, keepalives and timeouts.
    pub(super) fn service(&mut self, now: Instant) {
        let port = self.port;
        self.clients.retain(|addr, c| {
            let loading = c.phase == Phase::Signon || c.reconnecting;
            let alive = now.duration_since(c.last_heard) < if loading { SIGNON_TIMEOUT } else { CLIENT_TIMEOUT };
            if !alive {
                log!(
                    ":{port} {} ({}) timed out, {:?} for {:.1} s when last heard",
                    c.name.escape_debug(),
                    whence(*addr),
                    c.phase,
                    c.last_heard.saturating_duration_since(c.since).as_secs_f32()
                );
            }
            alive
        });
        for (addr, c) in &mut self.clients {
            let keepalive = now.duration_since(c.last_sent) > KEEPALIVE;
            if c.phase == Phase::Active {
                if keepalive {
                    self.out.send(*addr, c, None, true, now);
                }
            } else {
                self.out.burst(*addr, c, keepalive, now);
            }
        }
        if *TRACE && now.duration_since(self.traced) > TRACE_EVERY {
            self.traced = now;
            for (addr, c) in &mut self.clients {
                trace!(
                    "{addr} {:?}: sent {} packets with server data, {} without",
                    c.phase,
                    std::mem::take(&mut c.sent_with_tail),
                    std::mem::take(&mut c.sent_bare)
                );
            }
        }
    }

    pub(super) fn shutdown(&mut self, reason: &str) {
        let bye = messages::disconnect(reason);
        let now = Instant::now();
        for (addr, c) in &mut self.clients {
            for _ in 0..2 {
                self.out.send(*addr, c, Some(&bye), true, now);
            }
        }
        self.clients.clear();
    }

    /// Whether another signon waits: a client is in one. For [`SIGNON_HOLD`] at most; then those
    /// still in one are dropped, told why, and it goes.
    pub(super) fn signon_waits(&mut self, now: Instant) -> bool {
        if !self.clients.values().any(Client::in_signon) {
            self.signon_held = None;
            return false;
        }
        let since = *self.signon_held.get_or_insert(now);
        if now.duration_since(since) < SIGNON_HOLD {
            return true;
        }
        self.signon_held = None;
        let bye = messages::disconnect(SLOW_SIGNON);
        let port = self.port;
        let out = &mut self.out;
        self.clients.retain(|addr, c| {
            if !c.in_signon() {
                return true;
            }
            log!(":{port} {} ({}) still in its signon: dropped", c.name.escape_debug(), whence(*addr));
            // Only the goodbye: the rest of the signon is of no use to it.
            c.queue.clear();
            c.outgoing = None;
            for _ in 0..2 {
                out.send(*addr, c, Some(&bye), true, now);
            }
            false
        });
        false
    }

    /// No signon waits: a hold let go of starts over the next time, and those that came in during
    /// it get this signon.
    pub(super) fn no_signon_waits(&mut self) {
        if self.signon_held.take().is_some() {
            for c in self.clients.values_mut() {
                c.pump(&self.log);
            }
        }
    }

    /// A paced replay waits for a client, and for clients signing on.
    pub(super) fn hold(&self) -> bool {
        !self.seen_client || self.clients.values().any(Client::signing_on)
    }
}
