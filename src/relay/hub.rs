//! The hub: the clients, the backlog and the datagrams, shared by the upstream and client
//! threads.

use super::admission::Admission;
use super::backlog::{Backlog, Delivery};
use super::client::{Client, Phase};
use super::out::{Empty, Out};
use super::server_msg::{is_table_change, set_pause, signon_state, table_changes, vgui_menu};
use super::{
    CLIENT_TIMEOUT, KEEPALIVE, KEY_WAIT, MAX_SINCE_FULL, NOTES_AFTER_RESET, RelayConfig, SIGNON_HOLD, SIGNON_TIMEOUT,
    SPECTATORS, TRACE, TRACE_EVERY, masked_addr,
};
use crate::decode::Lifecycle;
use crate::net::bits::{BitBuf, BitReader, BitWriter};
use crate::net::messages::{self, SVC_PACKETENTITIES, SVC_SERVERINFO, Signon};
use crate::net::netchan;
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::Instant;

/// What a client dropped for a signon it did not finish in time is told, in its console.
const SLOW_SIGNON: &str = "stv.tf: took too long to load, retry";

/// Everything the upstream and client threads share.
pub(super) struct Hub {
    pub(super) out: Out,
    /// The port it listens on: which relay a log line is of.
    port: u16,
    pub(super) admission: Admission,
    pub(super) clients: HashMap<SocketAddr, Client>,
    pub(super) backlog: Backlog,
    /// This datagram's unreliable part, and whether it holds a full update.
    tail: Option<BitBuf>,
    tail_full: bool,
    /// The unreliable parts since the last full update, while there are few.
    pub(super) since_full: Option<Vec<BitBuf>>,
    /// Messages a client gets again, reliably, after every full update: what one drops (a room's
    /// note bubbles).
    pub(super) notes: Vec<BitBuf>,
    /// Sent once with the next full update.
    with_next: Vec<BitBuf>,
    /// When the last full update came, to space out asking for more.
    pub(super) last_full: Option<Instant>,
    /// Passing a server on, or playing a demo (see [`play`](super::play)), paused or not.
    pub(super) mode: Mode,
    /// Since when another signon waits for the clients in one.
    signon_held: Option<Instant>,
    traced: Instant,
    /// The clients were handed over: nothing more is sent or read.
    pub(super) silent: bool,
    /// The frame played in this datagram, as each client can take it.
    frame: Option<Played>,
}

/// A frame played, for clients that acked different frames: a game client sent a delta from a
/// frame it lacks crashes, and any datagram may be lost.
pub(super) struct Played {
    pub(super) tick: u32,
    /// The frame its entities build on; none for entities in full.
    pub(super) base: Option<u32>,
    /// Its stream with the entities as a delta from another frame, by that frame's tick.
    pub(super) from: HashMap<u32, BitBuf>,
    /// Its stream without the entities, for a client that can take none of those.
    pub(super) bare: BitBuf,
}

impl Played {
    /// The frame as `c` can take it: its entities from the frame `c` acked. With none it acked,
    /// or none written from it, without them: one not waiting for its full update's ack any more
    /// (see [`KEY_WAIT`]) waits for another.
    fn for_client<'a>(&'a self, c: &mut Client, played: Option<&'a BitBuf>, now: Instant) -> Option<&'a BitBuf> {
        let stream = match (self.base, c.acked) {
            (None, _) => {
                c.keyed_at(self.tick, now);
                return played;
            }
            (Some(base), Some(acked)) if base == acked => played,
            (Some(_), Some(acked)) => self.from.get(&acked),
            (Some(_), None) => None,
        };
        if stream.is_some() {
            c.sent_frame(self.tick);
            return stream;
        }
        if c.keyed.is_none_or(|at| now.duration_since(at) > KEY_WAIT) {
            trace!("{} cannot take frame {} from {:?}: a full update", c.name.escape_debug(), self.tick, c.acked);
            c.set_phase(Phase::WaitingFull, now);
        }
        Some(&self.bare)
    }
}

/// Which part of a datagram a stream is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Part {
    Reliable,
    Unreliable,
}

/// What feeds a hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// A server, which sends reliable data again until acked.
    Live,
    /// A demo, which has each message once: what a client must not lose goes reliably too.
    Play { paused: bool },
}

impl Mode {
    pub(super) fn is_play(self) -> bool {
        matches!(self, Mode::Play { .. })
    }
}

impl Hub {
    pub(super) fn new(sock: UdpSocket, config: &RelayConfig) -> Self {
        Hub {
            port: sock.local_addr().map_or(0, |a| a.port()),
            out: Out { sock, split_group: 0 },
            admission: Admission::new(config),
            clients: HashMap::new(),
            backlog: Backlog::new(),
            tail: None,
            tail_full: false,
            since_full: None,
            notes: Vec::new(),
            with_next: Vec::new(),
            last_full: None,
            mode: Mode::Live,
            signon_held: None,
            traced: Instant::now(),
            silent: false,
            frame: None,
        }
    }

    /// The server started a signon (connected, or a new map): so does every client. A reconnect
    /// asked of a client (see [`Hub::reconnect_all`]) still goes first.
    pub(super) fn restart_signon(&mut self, now: Instant) {
        self.start_over();
        let reconnect = messages::signonstate(Signon::Connected, -1);
        let play = self.mode.is_play();
        for c in self.clients.values_mut().filter(|c| !c.held) {
            if play && c.mid_signon() && !c.reconnecting {
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

    /// The clients past their signon keep what they have while the next demo's signon makes the
    /// backlog anew: it is one of the same map load, so they have all of it already.
    pub(super) fn hold_in_game(&mut self) {
        for c in self.clients.values_mut().filter(|c| c.phase != Phase::Signon && !c.reconnecting) {
            c.held = true;
        }
    }

    /// Those held have the backlog as it is now, and wait for a full update.
    pub(super) fn release_held(&mut self, now: Instant) {
        let end = self.backlog.len();
        for c in self.clients.values_mut().filter(|c| c.held) {
            c.held = false;
            c.cursor = end;
            c.set_phase(Phase::WaitingFull, now);
        }
    }

    /// Every client into a signon is told to reconnect (`net_SignonState(CONNECTED)`, as a server's
    /// `Reconnect` does, and as a live server does on a new map), and gets the next signon once it
    /// says it has: a game client sent a new signon in the middle of a game crashes.
    pub(super) fn reconnect_all(&mut self) {
        let reconnect = messages::signonstate(Signon::Connected, -1);
        for c in self.clients.values_mut().filter(|c| c.past_connected || c.phase != Phase::Signon) {
            c.ask_to_reconnect(&reconnect);
        }
    }

    pub(super) fn upstream_stream(&mut self, bits: &BitBuf, part: Part, life: &Lifecycle) {
        let now = Instant::now();
        if let Some((_, map)) = &life.server_info {
            self.admission.map.clone_from(map);
        }
        // The server sends the first full update after signon reliably (`net_Tick` and the
        // entities): like any entities, it goes to the clients in the datagram's unreliable
        // part, so it is never in the backlog and reaches a client in order with the deltas after it.
        let entities = life.spans.iter().any(|s| u32::from(s.id) == SVC_PACKETENTITIES);
        if part == Part::Unreliable || entities {
            if entities {
                self.backlog.open = false;
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
                self.add_unreliable_table_change(&msg);
            }
            return;
        }
        for span in &life.spans {
            let msg = BitReader::slice(bits.bytes(), span.bits.clone());
            if u32::from(span.id) == SVC_SERVERINFO {
                self.restart_signon(now);
            }
            if self.backlog.open || is_table_change(span.id) {
                let gate =
                    signon_state(span.id, &msg).filter(|s| matches!(s, Signon::New | Signon::PreSpawn | Signon::Spawn));
                self.backlog.push(msg, gate);
                for c in self.clients.values_mut() {
                    c.pump(&self.backlog);
                }
            } else {
                for c in self.clients.values_mut().filter(|c| c.caught_up(&self.backlog)) {
                    c.queue.push(msg.clone());
                }
            }
        }
    }

    /// Adds a string table change from a datagram's unreliable part to the backlog: a server's
    /// once (it comes again until acked), a demo's every time (it has each once).
    fn add_unreliable_table_change(&mut self, msg: &BitBuf) {
        let queue = self.mode.is_play();
        if !queue && self.backlog.has_table_change(msg) {
            return;
        }
        self.backlog.add_table_change(msg, &mut self.clients, Delivery::InDatagrams { queue });
        trace!("a string table change of {} bits from a datagram into the backlog", msg.nbits());
    }

    /// A stream passed over on a jump: only its string table changes count, which every client
    /// needs, so they go into the backlog and reliably to the clients already in.
    pub(super) fn skim(&mut self, bits: &BitBuf, life: &Lifecycle) {
        let tables: Vec<BitBuf> = table_changes(bits, life).collect();
        self.add_table_changes(&tables);
    }

    /// Playback went back past the string table changes `later`: they are added again when it
    /// comes to them, and `back` (what sets the clients' tables back) goes to every client, even
    /// if an earlier jump back sent the same.
    pub(super) fn set_tables_back(&mut self, later: &[BitBuf], back: &[BitBuf]) {
        self.backlog.forget_table_changes(later.iter().chain(back));
        self.add_table_changes(back);
    }

    /// String table changes passed over (a jump), into the backlog and reliably to the clients
    /// already in.
    pub(super) fn add_table_changes(&mut self, tables: &[BitBuf]) {
        self.since_full = None;
        for msg in tables {
            self.backlog.add_table_change(msg, &mut self.clients, Delivery::Reliably);
        }
    }

    /// Adds `msg` for every client, those in and those to come, unless it is already.
    pub(super) fn add_table_change_once(&mut self, msg: &BitBuf) {
        if !self.backlog.has_table_change(msg) {
            self.add_table_changes(std::slice::from_ref(msg));
        }
    }

    /// Whether a client is in the game.
    pub(super) fn any_in_game(&self) -> bool {
        self.clients.values().any(|c| c.phase == Phase::Active)
    }

    /// Whether a client is ready for entities and waits for a full update.
    pub(super) fn wants_keyframe(&self) -> bool {
        self.clients.values().any(|c| c.takes_full_update(&self.backlog))
    }

    /// Every client in the game waits for a keyframe: playback jumped, paused or went on, or came
    /// to a frame they cannot follow.
    pub(super) fn rekey_all(&mut self, now: Instant) {
        self.since_full = None;
        for c in self.clients.values_mut().filter(|c| c.phase == Phase::Active) {
            c.set_phase(Phase::WaitingFull, now);
        }
    }

    /// A full update (`net_Tick` and the entities) and the frames played since the one it is of,
    /// for the clients waiting for one, who are told if playback is `paused`. They are kept, with
    /// the frames after, for clients that come soon after.
    pub(super) fn send_keyframe(&mut self, tick: u32, key: &BitBuf, since: &[BitBuf], paused: bool) {
        let now = Instant::now();
        self.backlog.open = false;
        // While a demo plays, a client coming in gets a full update of its own, which it acks
        // before any delta.
        let kept = || std::iter::once(key.clone()).chain(since.iter().cloned()).collect();
        self.since_full = (!self.mode.is_play()).then(kept);
        // Always: one shown "paused" before a jump may not be any more.
        let pause = set_pause(paused);
        // TF2's player bar freezes until its clock passes its last draw; re-showing resets that.
        let again = std::mem::take(&mut self.with_next);
        for (addr, c) in &mut self.clients {
            if c.takes_full_update(&self.backlog) {
                trace!("{addr} gets a keyframe of {} bits", key.nbits());
                c.set_phase(Phase::Active, now);
                c.queue.push(pause.clone());
                c.queue.extend(again.iter().cloned());
                c.enter_notes(&self.notes, now);
                c.keyed_at(tick, now);
                self.out.send(*addr, c, Some(key), Empty::Send, now);
                for f in since {
                    self.out.send(*addr, c, Some(f), Empty::Send, now);
                }
            }
        }
    }

    /// The frames the clients in the game acked: what a played frame's entities are written from.
    pub(super) fn acked(&self) -> Vec<u32> {
        let mut acked: Vec<u32> =
            self.clients.values().filter(|c| c.phase == Phase::Active).filter_map(|c| c.acked).collect();
        acked.sort_unstable();
        acked.dedup();
        acked
    }

    /// The datagram about to be flushed is a frame played, with entities.
    pub(super) fn play(&mut self, frame: Played) {
        self.frame = Some(frame);
    }

    /// The next full update sets clients' clocks back: it shows the spectator panel again, which
    /// TF2's player bar needs.
    pub(super) fn reshow_panel_with_next_keyframe(&mut self) {
        self.with_next = vec![vgui_menu(SPECTATORS, false), vgui_menu(SPECTATORS, true)];
    }

    /// What a client gets after every full update from now on, in place of what it got before.
    pub(super) fn set_notes(&mut self, notes: Vec<BitBuf>) {
        self.notes = notes;
    }

    /// The clients in the game had their HUD reset, which drops the notes: they get them again in
    /// a moment, past it (a reset in the same packet is read after its reliable part).
    pub(super) fn reshow_notes(&mut self, now: Instant) {
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

    /// Plays a demo from here on, not paused.
    pub(super) fn start_playing(&mut self) {
        self.mode = Mode::Play { paused: false };
    }

    /// Tells the clients in the game that playback paused or went on, reliably.
    pub(super) fn queue_pause(&mut self, paused: bool) {
        self.mode = Mode::Play { paused };
        self.queue_to_active(&set_pause(paused));
    }

    /// A message for the clients in the game now, unreliably: what is sent again at a pace, which
    /// must not wait for the reliable stream.
    pub(super) fn send_to_active(&mut self, msg: &BitBuf) {
        let now = Instant::now();
        for (addr, c) in self.clients.iter_mut().filter(|(_, c)| c.phase == Phase::Active) {
            self.out.send(*addr, c, Some(msg), Empty::Skip, now);
        }
    }

    /// A message for the clients waiting for a full update, unreliably.
    pub(super) fn send_to_waiting(&mut self, msg: &BitBuf) {
        let now = Instant::now();
        for (addr, c) in &mut self.clients {
            if c.takes_full_update(&self.backlog) {
                self.out.send(*addr, c, Some(msg), Empty::Skip, now);
            }
        }
    }

    /// A message for the clients in the game, reliably.
    pub(super) fn queue_to_active(&mut self, msg: &BitBuf) {
        for c in self.clients.values_mut().filter(|c| c.phase == Phase::Active) {
            c.queue.push(msg.clone());
        }
    }

    /// The backlog goes: the next signon starts afresh.
    pub(super) fn start_over(&mut self) {
        self.backlog.clear();
        // Of the last demo's entities: the player gives them again.
        self.notes.clear();
        self.since_full = None;
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
                self.since_full = Some(vec![t.clone()]);
            } else if let Some(k) = &mut self.since_full {
                if k.len() < MAX_SINCE_FULL {
                    k.push(t.clone());
                } else {
                    self.since_full = None;
                }
            }
        }
        let frame = self.frame.take();
        for (addr, c) in &mut self.clients {
            if full && c.takes_full_update(&self.backlog) {
                trace!("{addr} gets a full update of {} bits", tail.as_ref().map_or(0, BitBuf::nbits));
                c.set_phase(Phase::Active, now);
                c.enter_notes(&self.notes, now);
            }
            if c.phase == Phase::Active {
                // One packet per server datagram, as the server sends: a packet of our own
                // would show in a client's net_graph as one without entities.
                let tail = match &frame {
                    Some(f) => f.for_client(c, tail.as_ref(), now),
                    None => tail.as_ref(),
                };
                self.out.send(*addr, c, tail, Empty::Send, now);
            } else {
                self.out.burst(*addr, c, Empty::Skip, now);
            }
        }
    }

    /// Starts a client ready for entities on them: from the kept datagrams if there are, or the
    /// next full update.
    pub(super) fn start_entities(&mut self, addr: SocketAddr, now: Instant) {
        let Some(c) = self.clients.get_mut(&addr) else { return };
        if let Some(frames) = &self.since_full
            && !self.mode.is_play()
            && c.through_backlog(&self.backlog)
        {
            trace!("{addr} gets entities from the {} datagrams kept since the last full update", frames.len());
            c.set_phase(Phase::Active, now);
            c.enter_notes(&self.notes, now);
            for t in frames {
                self.out.send(addr, c, Some(t), Empty::Skip, now);
            }
        } else {
            trace!("{addr} waits for a full update");
            c.set_phase(Phase::WaitingFull, now);
        }
    }

    pub(super) fn datagram(&mut self, from: SocketAddr, data: &[u8], now: Instant) {
        if self.silent {
            return;
        }
        if netchan::classify(data) == netchan::Kind::Oob {
            self.connectionless(from, data, now);
            return;
        }
        let Some(c) = self.clients.get_mut(&from) else { return };
        let Some(pkt) = c.rx.feed(data, now) else { return };
        let ready = c.read_packet(from, &pkt, now);
        if let Some(reason) = &c.gone {
            log!(":{} {} ({}) left: {}", self.port, c.name.escape_debug(), masked_addr(from), reason.escape_debug());
            self.clients.remove(&from);
            return;
        }
        // Not started on a signon the next demo waits to replace: it would not get through it.
        if self.signon_held.is_none() || c.mid_signon() || c.phase != Phase::Signon {
            c.pump(&self.backlog);
        }
        // An active client hears from us with the next server datagram. During signon its acks
        // free subchannels for more of the transfer, and its reliable data is acked at once.
        if c.phase != Phase::Active {
            self.out.burst(from, c, Empty::send_if(pkt.header.reliable()), now);
        }
        if ready {
            self.start_entities(from, now);
        }
    }

    fn connectionless(&mut self, from: SocketAddr, data: &[u8], now: Instant) {
        trace!(
            "{from} out of band {:?} ({} bytes): {:?}",
            data.get(4).map(|&b| char::from(b)),
            data.len(),
            String::from_utf8_lossy(&data[5.min(data.len())..data.len().min(40)])
        );
        let Some(reply) = self.admission.reply(from, data, &mut self.clients, self.port, now) else { return };
        let _ = self.out.sock.send_to(&reply, from);
    }

    /// Resends, keepalives and timeouts.
    pub(super) fn service(&mut self, now: Instant) {
        if self.silent {
            return;
        }
        let port = self.port;
        self.clients.retain(|addr, c| {
            let loading = c.phase == Phase::Signon || c.reconnecting;
            let alive = now.duration_since(c.last_heard) < if loading { SIGNON_TIMEOUT } else { CLIENT_TIMEOUT };
            if !alive {
                log!(
                    ":{port} {} ({}) timed out, silent for {:.0} s, {:?} for {:.1} s",
                    c.name.escape_debug(),
                    masked_addr(*addr),
                    now.duration_since(c.last_heard).as_secs_f32(),
                    c.phase,
                    now.duration_since(c.since).as_secs_f32()
                );
            }
            alive
        });
        for (addr, c) in &mut self.clients {
            let keepalive = now.duration_since(c.last_sent) > KEEPALIVE;
            if c.phase == Phase::Active {
                if keepalive {
                    self.out.send(*addr, c, None, Empty::Send, now);
                }
            } else {
                self.out.burst(*addr, c, Empty::send_if(keepalive), now);
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
                self.out.send(*addr, c, Some(&bye), Empty::Send, now);
            }
        }
        self.clients.clear();
    }

    /// Holds another signon while a client is in one, for [`SIGNON_HOLD`] at most; then those still
    /// in one are dropped, told why, and it goes. Whether it is held.
    pub(super) fn hold_for_signon(&mut self, now: Instant) -> bool {
        if !self.clients.values().any(Client::mid_signon) {
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
            if !c.mid_signon() {
                return true;
            }
            log!(":{port} {} ({}) still in its signon: dropped", c.name.escape_debug(), masked_addr(*addr));
            // Only the goodbye: the rest of the signon is of no use to it.
            c.queue.clear();
            c.outgoing = None;
            for _ in 0..2 {
                out.send(*addr, c, Some(&bye), Empty::Send, now);
            }
            false
        });
        false
    }

    /// No signon is held: one let go of starts over the next time, and those that came in during
    /// it get this signon.
    pub(super) fn release_signon_hold(&mut self) {
        if self.signon_held.take().is_some() {
            for c in self.clients.values_mut() {
                c.pump(&self.backlog);
            }
        }
    }

    /// A paced replay waits for a client, and for clients signing on.
    pub(super) fn replay_waits(&self) -> bool {
        !self.admission.seen_client || self.clients.values().any(Client::has_signon_pending)
    }
}
