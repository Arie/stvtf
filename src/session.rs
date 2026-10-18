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
use crate::net::handshake::Connection;
use crate::net::messages::{self, Bits};
use crate::net::netchan;
use crate::net::receiver::Receiver;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use typed_arena::Arena;

pub struct Options {
    pub name: String,
    pub crc: u32,
    pub snapshot_hz: f32,
    pub silence: Duration,
}

const SIGNON_CONNECTED: u8 = 2;
const SIGNON_NEW: u8 = 3;
const SIGNON_SPAWN: u8 = 5;
const SIGNON_FULL: u8 = 6;
const SIGNON_CHANGELEVEL: u8 = 7;
const RESEND_AFTER: Duration = Duration::from_millis(500);
const TICK_ACK_EVERY: Duration = Duration::from_millis(100);

struct Pending {
    region: Bits,
    subchannel: u8,
    sent_at: Instant,
}

/// Rejections that reconnecting cannot fix: a wrong password or class tables of another build.
pub fn fatal(reason: &str) -> bool {
    let r = reason.to_ascii_lowercase();
    r.contains("password") || r.contains("class tables")
}

pub enum End {
    /// The server dropped us or went silent; worth reconnecting.
    Broken(String),
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
    state: u8,
    spawncount: Option<i32>,
    map: Option<String>,
    server_tick: i32,
    echoed: HashSet<(i32, u8)>,
    pending: Option<Pending>,
    last_inbound: Instant,
    last_tick_ack: Instant,
    broke: Option<String>,
    /// Set when the server restarts signon on this connection (a map change): the
    /// streams from that point on are replayed into a fresh decoder for the new map.
    remap: Option<Vec<(Vec<u8>, usize)>>,
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
            state: SIGNON_CONNECTED,
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

    fn send(&mut self, reliable: Option<&Bits>, unreliable: Option<&Bits>) {
        self.out_seq += 1;
        let Some(sock) = self.sock else { return };
        let pkt = netchan::build_packet(
            self.out_seq - 1,
            self.rx.in_seq,
            self.challenge,
            self.in_reliable_state,
            reliable.map(|b| (b.0.as_slice(), b.1)),
            unreliable.map(|b| (b.0.as_slice(), b.1)),
        );
        if let Err(e) = sock.send(&pkt) {
            self.broke.get_or_insert(format!("socket error: {e}"));
        }
    }

    fn send_reliable(&mut self, body: &Bits, subchannel: Option<u8>) {
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

    fn echo(&mut self, state: u8) {
        let Some(spawncount) = self.spawncount else { return };
        if state < SIGNON_CONNECTED || self.pending.is_some() || !self.echoed.insert((spawncount, state)) {
            return;
        }
        let body = if state == SIGNON_NEW {
            messages::client_info(spawncount, self.opt.crc, true)
        } else {
            messages::signonstate(state, spawncount)
        };
        self.send_reliable(&body, None);
    }

    fn advance(&mut self, state: u8, emit: &mut dyn FnMut(Value)) {
        if state == SIGNON_CHANGELEVEL {
            emit(json!({"type": "status", "state": "changelevel"}));
            return;
        }
        if self.state >= SIGNON_FULL && state <= SIGNON_NEW {
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
            emit(json!({"type": "status", "state": signon_name(state)}));
        }
        self.echo(state);
    }

    fn lifecycle(&mut self, life: Lifecycle, emit: &mut dyn FnMut(Value)) {
        if let Some(t) = life.tick {
            self.server_tick = self.server_tick.max(t);
        }
        if let Some(reason) = life.disconnect {
            self.broke.get_or_insert(format!("disconnected by server: {reason}"));
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
        if self.state == SIGNON_SPAWN && self.server_tick > 0 && (self.pending.is_none() || self.sock.is_none()) {
            self.state = SIGNON_FULL;
            emit(json!({"type": "status", "state": "full", "map": self.map}));
            self.echo(SIGNON_FULL);
        }
    }

    fn datagram(&mut self, data: &[u8], dec: &mut Decoder<'_>, emit: &mut dyn FnMut(Value)) {
        self.last_inbound = Instant::now();
        let Some(pkt) = self.rx.feed(data, Instant::now()) else { return };
        if pkt.header.reliable()
            && pkt.reliable_ok
            && let Some(sub) = pkt.subchannel
        {
            self.in_reliable_state ^= 1 << sub;
        }
        for stream in &pkt.streams {
            if let Some(rest) = &mut self.remap {
                rest.push((stream.data.clone(), stream.nbits));
                continue;
            }
            let life = dec.feed(&stream.data, stream.nbits);
            self.lifecycle(life, emit);
            if let Some(rest) = &mut self.remap {
                rest.push((stream.data.clone(), stream.nbits));
            }
        }
        for v in dec.out.drain(..) {
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
            self.broke.get_or_insert(format!("no data for {:?}", self.opt.silence));
            return;
        }
        if self.pending.as_ref().is_some_and(|p| now.duration_since(p.sent_at) > RESEND_AFTER) {
            self.transmit_pending();
        }
        self.promote(emit);
        if self.state >= SIGNON_SPAWN && now.duration_since(self.last_tick_ack) > TICK_ACK_EVERY {
            self.last_tick_ack = now;
            let body = messages::tick(self.server_tick);
            self.send(None, Some(&body));
        }
    }
}

fn signon_name(s: u8) -> &'static str {
    ["none", "challenge", "connected", "new", "prespawn", "spawn", "full", "changelevel"]
        .get(s as usize)
        .copied()
        .unwrap_or("unknown")
}

enum Next {
    Datagram(Vec<u8>),
    Idle,
    Stop,
}

/// Runs a session over a datagram source until it stops or breaks.
fn drive(
    s: &mut Session<'_>,
    tables: &Tables,
    first: Option<&[u8]>,
    next: &mut dyn FnMut() -> Result<Next>,
    emit: &mut dyn FnMut(Value),
) -> Result<(End, u64)> {
    let mut decode_errors = 0;
    let mut first = first;
    let end = 'maps: loop {
        // One decoder (and arena) per map; the netchannel state survives map changes.
        let arena = Arena::new();
        let mut dec = Decoder::new(&arena, tables, s.opt.snapshot_hz)?;
        if let Some(streams) = s.remap.take() {
            for (data, nbits) in streams {
                let life = dec.feed(&data, nbits);
                s.lifecycle(life, emit);
            }
            for v in dec.out.drain(..) {
                emit(v);
            }
        }
        if let Some(p) = first.take() {
            s.datagram(p, &mut dec, emit);
        }
        loop {
            match next()? {
                Next::Datagram(d) => s.datagram(&d, &mut dec, emit),
                Next::Idle => {}
                Next::Stop => {
                    decode_errors += dec.parse_errors;
                    break 'maps End::Stopped;
                }
            }
            if s.sock.is_some() {
                s.poll(emit);
            }
            if let Some(why) = s.broke.take() {
                decode_errors += dec.parse_errors;
                break 'maps End::Broken(why);
            }
            if s.remap.is_some() {
                decode_errors += dec.parse_errors;
                continue 'maps;
            }
        }
    };
    Ok((end, decode_errors))
}

fn finish(s: &Session<'_>, end: &End, decode_errors: u64, emit: &mut dyn FnMut(Value)) {
    emit(json!({
        "type": "status",
        "state": "disconnected",
        "reason": match end { End::Broken(w) => w.as_str(), End::Stopped => "stopped" },
        "decode_errors": decode_errors,
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
            Err(_) => Ok(Next::Idle),
        }
    };
    let (end, decode_errors) = drive(&mut s, tables, conn.pending.as_deref(), &mut next, emit)?;
    // Always say goodbye: if the server already dropped us this is ignored, and if
    // we misjudged a break it frees the SourceTV slot instead of holding it 300 s.
    let bye = messages::disconnect("Disconnect by user.");
    s.send(None, Some(&bye));
    s.send(None, Some(&bye));
    finish(&s, &end, decode_errors, emit);
    Ok(end)
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
    let (end, decode_errors) = drive(&mut s, tables, None, &mut next, emit)?;
    finish(&s, &end, decode_errors, emit);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::fatal;

    #[test]
    fn only_unfixable_rejections_are_fatal() {
        assert!(fatal("disconnected by server: Bad spectator password"));
        assert!(fatal("connect refused: #GameUI_ServerRejectBadPassword"));
        assert!(fatal("disconnected by server: Server uses different class tables"));
        assert!(!fatal("connect refused: #GameUI_ServerRejectServerFull"));
        assert!(!fatal("silent for 15s"));
    }
}
