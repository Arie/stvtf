//! One SourceTV session: climb the signon ladder to FULL, keep acking, decode.
//!
//! The server pushes each signon state; we echo `net_SignonState(state, spawncount)`
//! for every state from CONNECTED up, quoting the spawncount of the latest
//! svc_ServerInfo. NEW is answered with clc_ClientInfo carrying the SendTable CRC.
//! FULL is not pushed: the client promotes itself once world data flows.
//!
//! A map change re-runs signon on the same connection; the session keeps its
//! netchannel state and starts a fresh decoder for the new map.
//!
//! The same driver runs live (a socket) and offline (a recorded capture, nothing sent),
//! so replays and tests exercise exactly the code a live session runs.

use crate::capture::Recorder;
use crate::decode::{Decoder, Lifecycle, Tables};
use crate::net::bits::BitBuf;
use crate::net::handshake::Connection;
use crate::net::messages::{self, Signon};
use crate::net::netchan::{self, OutHeader};
use crate::net::receiver::Receiver;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::ErrorKind;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use typed_arena::Arena;

/// How a session behaves.
#[derive(Debug, Clone)]
pub struct Options {
    /// Spectator name shown on the server.
    pub name: String,
    /// The server build's SendTable CRC.
    pub crc: u32,
    /// Snapshot rate cap; 0 disables snapshots.
    pub snapshot_hz: f32,
    /// Give up on a connection after this long without a valid packet.
    pub silence: Duration,
    /// Reconnect once the decoder holds this many bytes of one map's messages (it can
    /// only release them at a map change), so a map that never ends cannot exhaust memory.
    pub max_decoder_bytes: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            name: "stv.tf".to_string(),
            crc: 0,
            snapshot_hz: 20.0,
            silence: Duration::from_secs(15),
            max_decoder_bytes: 1 << 30,
        }
    }
}

const RESEND_AFTER: Duration = Duration::from_millis(500);
const TICK_ACK_EVERY: Duration = Duration::from_millis(100);

struct Pending {
    region: BitBuf,
    subchannel: u8,
    sent_at: Instant,
}

/// Rejections that reconnecting cannot fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fatal {
    /// The `tv_password` is wrong.
    BadPassword,
    /// The server runs a build whose class tables or CRC we do not have.
    ClassTables,
}

impl Fatal {
    /// Recognizes the server's exact texts for these (a refusal token or a disconnect
    /// reason). A kick message that merely mentions a password is not one.
    pub fn from_reason(reason: &str) -> Option<Self> {
        match reason.trim().to_ascii_lowercase().as_str() {
            "#gameui_serverrejectbadpassword" | "bad password" | "bad spectator password" => Some(Fatal::BadPassword),
            "server uses different class tables" => Some(Fatal::ClassTables),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Broken {
    pub reason: String,
    /// Set when reconnecting cannot help.
    pub fatal: Option<Fatal>,
    /// The session reached FULL: the server works, so a reconnect need not back off.
    pub streamed: bool,
}

#[derive(Debug)]
pub enum End {
    /// The server dropped us or went silent; worth reconnecting unless `fatal`.
    Broken(Broken),
    Stopped,
}

struct Session<'o> {
    /// None when replaying a capture: everything runs, nothing is sent.
    sock: Option<&'o UdpSocket>,
    challenge: u32,
    opt: &'o Options,
    rx: Receiver,
    out_seq: i32,
    in_reliable_state: u8,
    out_reliable_state: u8,
    next_subchannel: u8,
    state: Signon,
    reached_full: bool,
    spawncount: Option<i32>,
    map: Option<String>,
    server_tick: i32,
    echoed: HashSet<(i32, Signon)>,
    pending: Option<Pending>,
    last_inbound: Instant,
    last_tick_ack: Instant,
    broke: Option<(String, Option<Fatal>)>,
    /// Set when the server restarts signon on this connection (a map change): the
    /// streams from that point on are replayed into a fresh decoder for the new map.
    remap: Option<Vec<BitBuf>>,
}

impl<'o> Session<'o> {
    fn new(sock: Option<&'o UdpSocket>, challenge: u32, opt: &'o Options) -> Self {
        Session {
            sock,
            challenge,
            opt,
            rx: Receiver::new(Some(challenge)),
            out_seq: 1,
            in_reliable_state: 0,
            out_reliable_state: 0,
            next_subchannel: 1,
            state: Signon::Connected,
            reached_full: false,
            spawncount: None,
            map: None,
            server_tick: 0,
            echoed: HashSet::new(),
            pending: None,
            last_inbound: Instant::now(),
            last_tick_ack: Instant::now(),
            broke: None,
            remap: None,
        }
    }

    /// Records why the session ends; the first reason wins.
    fn break_with(&mut self, reason: String, fatal: Option<Fatal>) {
        self.broke.get_or_insert((reason, fatal));
    }

    fn send(&mut self, reliable: Option<&BitBuf>, unreliable: Option<&BitBuf>) {
        self.out_seq += 1;
        let Some(sock) = self.sock else { return };
        let header = OutHeader {
            sequence: self.out_seq - 1,
            ack: self.rx.in_seq,
            challenge: self.challenge,
            reliable_state: self.in_reliable_state,
        };
        let pkt = netchan::build_packet(&header, reliable, unreliable);
        if let Err(e) = sock.send(&pkt) {
            self.break_with(format!("socket error: {e}"), None);
        }
    }

    fn send_reliable(&mut self, body: &BitBuf, subchannel: Option<u8>) {
        let sub = subchannel.unwrap_or_else(|| {
            let s = self.next_subchannel;
            self.next_subchannel = (self.next_subchannel + 1) % 8;
            s
        });
        let region = messages::reliable_region(body, sub);
        self.out_reliable_state ^= 1 << sub;
        self.pending = Some(Pending { region, subchannel: sub, sent_at: Instant::now() });
        self.transmit_pending();
    }

    fn transmit_pending(&mut self) {
        if let Some(p) = self.pending.take() {
            self.send(Some(&p.region), None);
            self.pending = Some(Pending { sent_at: Instant::now(), ..p });
        }
    }

    fn echo(&mut self, state: Signon) {
        let Some(spawncount) = self.spawncount else { return };
        if state < Signon::Connected || self.pending.is_some() || !self.echoed.insert((spawncount, state)) {
            return;
        }
        let body = if state == Signon::New {
            messages::client_info(spawncount, self.opt.crc, true)
        } else {
            messages::signonstate(state, spawncount)
        };
        self.send_reliable(&body, None);
    }

    fn advance(&mut self, state: Signon, emit: &mut dyn FnMut(Value)) {
        if state == Signon::ChangeLevel {
            emit(json!({"type": "status", "state": "changelevel"}));
            return;
        }
        if self.state >= Signon::Full && state <= Signon::New {
            // The server re-runs signon in place for a new map; a relay keeps its connection.
            emit(json!({"type": "status", "state": "map_change"}));
            self.state = state;
            self.echoed.clear();
            self.remap = Some(Vec::new());
            self.echo(state);
            return;
        }
        if state > self.state {
            self.state = state;
            emit(json!({"type": "status", "state": state.name()}));
        }
        self.echo(state);
    }

    fn lifecycle(&mut self, life: Lifecycle, emit: &mut dyn FnMut(Value)) {
        if let Some(t) = life.tick {
            self.server_tick = self.server_tick.max(t);
        }
        if let Some(reason) = life.disconnect {
            let fatal = Fatal::from_reason(&reason);
            self.break_with(format!("disconnected by server: {reason}"), fatal);
        }
        if let Some((count, map)) = life.server_info {
            if self.spawncount.is_some_and(|c| c != count) {
                self.echoed.clear();
                // A new spawncount starts a new map; its tick counter restarts.
                self.server_tick = 0;
            }
            self.spawncount = Some(count);
            self.map = Some(map);
        }
        for s in life.signon {
            self.advance(s, emit);
        }
    }

    /// FULL is the client's own promotion once ticks flow. Live it waits for our
    /// last reliable echo to be acknowledged; offline there are no acks to wait for.
    fn promote(&mut self, emit: &mut dyn FnMut(Value)) {
        if self.state == Signon::Spawn && self.server_tick > 0 && (self.pending.is_none() || self.sock.is_none()) {
            self.state = Signon::Full;
            self.reached_full = true;
            emit(json!({"type": "status", "state": "full", "map": self.map}));
            self.echo(Signon::Full);
        }
    }

    fn datagram(&mut self, data: &[u8], dec: &mut Decoder<'_>, emit: &mut dyn FnMut(Value)) {
        let now = Instant::now();
        let Some(pkt) = self.rx.feed(data, now) else { return };
        // Only a packet that passed the checksum, challenge and sequence checks proves
        // the connection is alive; junk must not hold off the silence timeout.
        self.last_inbound = now;
        if pkt.header.reliable()
            && pkt.reliable_ok
            && let Some(sub) = pkt.subchannel
        {
            self.in_reliable_state ^= 1 << sub;
        }
        for stream in pkt.streams {
            if let Some(rest) = &mut self.remap {
                rest.push(stream.bits);
                continue;
            }
            let life = dec.feed(stream.bits.bytes(), stream.bits.nbits());
            self.lifecycle(life, emit);
            if let Some(rest) = &mut self.remap {
                rest.push(stream.bits);
            }
        }
        for v in dec.take_output() {
            emit(v);
        }
        if let Some(p) = &self.pending {
            let want = (self.out_reliable_state >> p.subchannel) & 1;
            if (pkt.header.reliable_state >> p.subchannel) & 1 == want {
                self.pending = None;
                self.echo(self.state);
            }
        }
        if self.sock.is_none() {
            self.promote(emit);
        }
        self.send(None, None);
    }

    fn poll(&mut self, emit: &mut dyn FnMut(Value)) {
        let now = Instant::now();
        if now.duration_since(self.last_inbound) > self.opt.silence {
            self.break_with(format!("no data for {:?}", self.opt.silence), None);
            return;
        }
        if self.pending.as_ref().is_some_and(|p| now.duration_since(p.sent_at) > RESEND_AFTER) {
            self.transmit_pending();
        }
        self.promote(emit);
        if self.state >= Signon::Spawn && now.duration_since(self.last_tick_ack) > TICK_ACK_EVERY {
            self.last_tick_ack = now;
            let body = messages::tick(self.server_tick);
            self.send(None, Some(&body));
        }
    }
}

enum Next {
    Datagram(Vec<u8>),
    Idle,
    /// The datagram source failed (a socket error other than a timeout).
    Failed(String),
    Stop,
}

struct Outcome {
    end: End,
    decode_errors: u64,
}

/// Runs a session over a datagram source until it stops or breaks.
fn drive(
    s: &mut Session<'_>,
    tables: &Tables,
    first: Option<&[u8]>,
    next: &mut dyn FnMut() -> Result<Next>,
    emit: &mut dyn FnMut(Value),
) -> Result<Outcome> {
    let mut decode_errors = 0;
    let mut first = first;
    let end = 'maps: loop {
        // One decoder (and arena) per map; the netchannel state survives map changes.
        let arena = Arena::new();
        let mut dec = Decoder::new(&arena, tables, s.opt.snapshot_hz)?;
        if let Some(streams) = s.remap.take() {
            let mut rest = streams.into_iter();
            for bits in rest.by_ref() {
                let life = dec.feed(bits.bytes(), bits.nbits());
                s.lifecycle(life, emit);
                if let Some(next_map) = &mut s.remap {
                    // Signon restarted again: the rest belongs to the map after this one.
                    next_map.push(bits);
                    break;
                }
            }
            for v in dec.take_output() {
                emit(v);
            }
            if let Some(next_map) = &mut s.remap {
                next_map.extend(rest);
                decode_errors += dec.parse_errors();
                continue 'maps;
            }
        }
        if let Some(p) = first.take() {
            s.datagram(p, &mut dec, emit);
        }
        let end = loop {
            match next()? {
                Next::Datagram(d) => s.datagram(&d, &mut dec, emit),
                Next::Idle => {}
                Next::Failed(why) => s.break_with(why, None),
                Next::Stop => break Some(End::Stopped),
            }
            if s.sock.is_some() {
                s.poll(emit);
            }
            if dec.panicked() {
                s.break_with("decoder failed".to_string(), None);
            } else if dec.bytes() > s.opt.max_decoder_bytes {
                s.break_with(format!("decoder holds over {} MiB for this map", s.opt.max_decoder_bytes >> 20), None);
            }
            if let Some((reason, fatal)) = s.broke.take() {
                break Some(End::Broken(Broken { reason, fatal, streamed: s.reached_full }));
            }
            if s.remap.is_some() {
                break None;
            }
        };
        decode_errors += dec.parse_errors();
        if let Some(end) = end {
            break 'maps end;
        }
    };
    Ok(Outcome { end, decode_errors })
}

fn finish(s: &Session<'_>, outcome: &Outcome, emit: &mut dyn FnMut(Value)) {
    emit(json!({
        "type": "status",
        "state": "disconnected",
        "reason": match &outcome.end { End::Broken(b) => b.reason.as_str(), End::Stopped => "stopped" },
        "decode_errors": outcome.decode_errors,
        "packets": s.rx.counters.packets,
        "lost": s.rx.counters.lost,
    }));
}

pub fn run(
    conn: &Connection,
    tables: &Tables,
    opt: &Options,
    mut recorder: Option<&mut Recorder>,
    stop: &AtomicBool,
    deadline: Option<Instant>,
    emit: &mut dyn FnMut(Value),
) -> Result<End> {
    let mut s = Session::new(Some(&conn.sock), conn.challenge, opt);
    if let Some(r) = recorder.as_deref_mut() {
        r.session(conn.challenge)?;
        if let Some(p) = &conn.pending {
            r.datagram(p)?;
        }
    }
    conn.sock.set_read_timeout(Some(Duration::from_millis(20)))?;
    s.send_reliable(&messages::connected_reply(&opt.name), Some(0));
    let mut buf = vec![0u8; 65536];
    let mut next = || -> Result<Next> {
        if stop.load(Ordering::Relaxed) || deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(Next::Stop);
        }
        match conn.sock.recv(&mut buf) {
            Ok(n) => {
                if let Some(r) = recorder.as_deref_mut() {
                    r.datagram(&buf[..n])?;
                }
                Ok(Next::Datagram(buf[..n].to_vec()))
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(Next::Idle),
            // E.g. ConnectionRefused: an ICMP port unreachable, the server is gone.
            Err(e) => Ok(Next::Failed(format!("socket error: {e}"))),
        }
    };
    let outcome = drive(&mut s, tables, conn.pending.as_deref(), &mut next, emit)?;
    // Always say goodbye: if the server already dropped us this is ignored, and if
    // we misjudged a break it frees the SourceTV slot instead of holding it 300 s.
    let bye = messages::disconnect("Disconnect by user.");
    s.send(None, Some(&bye));
    s.send(None, Some(&bye));
    finish(&s, &outcome, emit);
    Ok(outcome.end)
}

/// Replays one recorded session through the same driver, sending nothing.
pub fn replay(
    challenge: u32,
    datagrams: &[&[u8]],
    tables: &Tables,
    opt: &Options,
    emit: &mut dyn FnMut(Value),
) -> Result<()> {
    let mut s = Session::new(None, challenge, opt);
    let mut it = datagrams.iter();
    let mut next = || -> Result<Next> { Ok(it.next().map_or(Next::Stop, |d| Next::Datagram(d.to_vec()))) };
    let outcome = drive(&mut s, tables, None, &mut next, emit)?;
    finish(&s, &outcome, emit);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Fatal;

    #[test]
    fn only_unfixable_rejections_are_fatal() {
        assert_eq!(Fatal::from_reason("Bad spectator password"), Some(Fatal::BadPassword));
        assert_eq!(Fatal::from_reason("#GameUI_ServerRejectBadPassword"), Some(Fatal::BadPassword));
        assert_eq!(Fatal::from_reason("Server uses different class tables\n"), Some(Fatal::ClassTables));
        assert_eq!(Fatal::from_reason("#GameUI_ServerRejectServerFull"), None);
        assert_eq!(Fatal::from_reason("Kicked: password changed, reconnect"), None);
    }
}
