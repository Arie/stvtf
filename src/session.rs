//! One `SourceTV` session: sign on up to FULL, keep acking, decode.
//!
//! The server pushes each signon state; the client echoes `net_SignonState(state, spawncount)`
//! for every state from CONNECTED up, quoting the spawncount of the latest
//! `svc_ServerInfo`. NEW is answered with `clc_ClientInfo` carrying the `SendTable` CRC.
//! FULL is not pushed: the client promotes itself once world data flows.
//!
//! A map change re-runs signon on the same connection; the session keeps its
//! netchannel state and starts a fresh decoder for the new map.
//!
//! The same driver runs live (a socket) and offline (a recorded capture, nothing sent),
//! so replays and tests exercise exactly the code a live session runs.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

use crate::Result;
use crate::capture::Recorder;
use crate::decode::{Decoder, Lifecycle, Tables};
use crate::error::{bail, invalid};
use crate::net::bits::BitBuf;
use crate::net::handshake::{Connection, Wait};
use crate::net::messages::{self, Signon};
use crate::net::netchan::{self, OutHeader};
use crate::net::receiver::{MsgStream, Receiver};
use crate::net::reliable;
use crate::output::{Line, Status};

/// How a session behaves.
#[derive(Clone, Debug)]
pub struct Options {
    /// Spectator name shown on the server.
    pub name: String,
    /// The server build's `SendTable` CRC.
    pub crc: u32,
    /// Snapshot rate cap; 0 disables snapshots.
    pub snapshot_hz: f32,
    /// Give up on a connection after this long without a valid packet.
    pub silence: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options { name: DEFAULT_NAME.to_string(), crc: 0, snapshot_hz: DEFAULT_SNAPSHOT_HZ, silence: DEFAULT_SILENCE }
    }
}

/// The spectator name shown on the server unless asked otherwise.
pub(crate) const DEFAULT_NAME: &str = "stv.tf";
const DEFAULT_SILENCE: Duration = Duration::from_secs(15);

/// Snapshots per second unless asked otherwise.
pub const DEFAULT_SNAPSHOT_HZ: f32 = 20.0;

/// Seconds per server tick (TF2's 66.67 per second) until the server's own value arrives in
/// [`crate::output::Server::tick_interval`].
pub const DEFAULT_TICK_INTERVAL: f32 = 0.015;

/// A snapshot rate as written: a number, 0 or more (0 disables snapshots).
pub fn parse_snapshot_hz(hz: &str) -> Result<f32> {
    let parsed: f32 = hz.parse().map_err(|_| invalid!("cannot read a number"))?;
    if !parsed.is_finite() || parsed < 0.0 {
        bail!("cannot be below 0 or infinite");
    }
    Ok(parsed)
}
/// The spawncount a client quotes before it was told one.
const NO_SPAWNCOUNT: i32 = -1;
/// How long a read waits, so tick acks go out on time.
const READ_POLL: Duration = Duration::from_millis(20);
const TICK_ACK_EVERY: Duration = Duration::from_millis(100);

struct Pending {
    region: BitBuf,
    subchannel: u8,
    /// The sequence of the packet that last carried it.
    seq: i32,
}

/// Rejections that reconnecting cannot fix.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Fatal {
    /// The `tv_password` is wrong.
    BadPassword,
    /// The server runs a build whose class tables or CRC stvtf does not have.
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

/// Sees the server's traffic as a session decodes it, to pass it on (see `relay`).
pub(crate) trait Tap {
    /// One message stream and what decoding it found, in the order the server sent them.
    fn stream(&self, bits: &BitBuf, reliable: bool, life: &Lifecycle);
    /// The streams of one datagram have all been seen.
    fn flush(&self);
    /// Tick acks ask for a full update while this holds.
    fn want_full_update(&self) -> bool;
    /// The build the server says it is, before each connection to it.
    fn server_build(&self, build: &str);
    /// The class tables the session decodes with, before each connection.
    fn tables(&self, _tables: &Tables) {}
}

/// Two taps as one.
pub(crate) struct Both<'a>(pub &'a dyn Tap, pub &'a dyn Tap);

impl Tap for Both<'_> {
    fn stream(&self, bits: &BitBuf, reliable: bool, life: &Lifecycle) {
        self.0.stream(bits, reliable, life);
        self.1.stream(bits, reliable, life);
    }

    fn flush(&self) {
        self.0.flush();
        self.1.flush();
    }

    fn want_full_update(&self) -> bool {
        self.0.want_full_update() || self.1.want_full_update()
    }

    fn server_build(&self, build: &str) {
        self.0.server_build(build);
        self.1.server_build(build);
    }

    fn tables(&self, tables: &Tables) {
        self.0.tables(tables);
        self.1.tables(tables);
    }
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
    /// The entities missed a frame: tick acks ask for a full update until one comes.
    want_full_update: bool,
    echoed: HashSet<(i32, Signon)>,
    pending: Option<Pending>,
    last_inbound: Instant,
    last_tick_ack: Instant,
    broke: Option<(String, Option<Fatal>)>,
    /// Set when the server restarts signon on this connection (a map change): the
    /// streams from that point on are replayed into a fresh decoder for the new map.
    remap: Option<Vec<MsgStream>>,
    tap: Option<&'o dyn Tap>,
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
            want_full_update: false,
            echoed: HashSet::new(),
            pending: None,
            last_inbound: Instant::now(),
            last_tick_ack: Instant::now(),
            broke: None,
            remap: None,
            tap: None,
        }
    }

    /// The tick to ack: the latest seen, so the server deltas from it, or `FULL_UPDATE_TICK` for a
    /// full update, as a game client does when its entities missed a frame.
    fn ack_tick(&self) -> i32 {
        let tap_wants = self.tap.is_some_and(Tap::want_full_update);
        if self.want_full_update || tap_wants { messages::FULL_UPDATE_TICK } else { self.server_tick }
    }

    /// Records why the session ends; the first reason wins.
    fn break_with(&mut self, reason: String, fatal: Option<Fatal>) {
        self.broke.get_or_insert((reason, fatal));
    }

    /// Sends a packet and returns its sequence.
    fn send(&mut self, reliable: Option<&BitBuf>, unreliable: Option<&BitBuf>) -> i32 {
        let sequence = self.out_seq;
        self.out_seq += 1;
        let Some(sock) = self.sock else { return sequence };
        let header = OutHeader {
            sequence,
            ack: self.rx.in_seq,
            challenge: self.challenge,
            reliable_state: self.in_reliable_state,
        };
        let pkt = netchan::build_packet(&header, reliable, unreliable);
        if let Err(e) = sock.send(&pkt) {
            self.break_with(format!("socket error: {e}"), None);
        }
        sequence
    }

    fn send_reliable(&mut self, body: &BitBuf, subchannel: Option<u8>) {
        let sub = subchannel.unwrap_or_else(|| {
            let s = self.next_subchannel;
            self.next_subchannel = (self.next_subchannel + 1) % reliable::SUBCHANNELS;
            s
        });
        let region = messages::reliable_region(body, sub);
        self.out_reliable_state ^= 1 << sub;
        let seq = self.send(Some(&region), None);
        self.pending = Some(Pending { region, subchannel: sub, seq });
    }

    fn transmit_pending(&mut self) {
        if let Some(p) = self.pending.take() {
            let seq = self.send(Some(&p.region), None);
            self.pending = Some(Pending { seq, ..p });
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

    fn advance(&mut self, state: Signon, emit: &mut dyn FnMut(Line)) {
        if state == Signon::Connected && self.spawncount.is_none() {
            // A reconnect asked before any `ServerInfo` (a relay restarting its signon): answered
            // as a game client does.
            if self.pending.is_none() {
                self.send_reliable(&messages::signonstate(state, NO_SPAWNCOUNT), None);
            }
            return;
        }
        if state == Signon::ChangeLevel {
            emit(Line::Status(Status::Changelevel));
            // Answer the next reconnect, even at the same spawncount.
            self.echoed.clear();
            return;
        }
        if self.state >= Signon::Full && state <= Signon::New {
            // The server re-runs signon in place for a new map; a relay keeps its connection.
            emit(Line::Status(Status::MapChange));
            self.state = state;
            self.echoed.clear();
            self.remap = Some(Vec::new());
            self.echo(state);
            return;
        }
        if state > self.state {
            self.state = state;
            if let Some(status) = self.status(state) {
                emit(Line::Status(status));
            }
        }
        self.echo(state);
    }

    /// The status line for reaching a signon state; states below CONNECTED are never reached
    /// from here (a session starts at CONNECTED).
    fn status(&self, state: Signon) -> Option<Status> {
        match state {
            Signon::New => Some(Status::New),
            Signon::PreSpawn => Some(Status::Prespawn),
            Signon::Spawn => Some(Status::Spawn),
            Signon::Full => Some(Status::Full { map: self.map.clone() }),
            Signon::ChangeLevel => Some(Status::Changelevel),
            Signon::None | Signon::Challenge | Signon::Connected => None,
        }
    }

    fn lifecycle(&mut self, life: Lifecycle, emit: &mut dyn FnMut(Line)) {
        if let Some(t) = life.tick {
            self.server_tick = self.server_tick.max(t);
        }
        if life.lost_frame {
            self.want_full_update = true;
        } else if life.full_update {
            self.want_full_update = false;
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
    fn promote(&mut self, emit: &mut dyn FnMut(Line)) {
        if self.state == Signon::Spawn && self.server_tick > 0 && (self.pending.is_none() || self.sock.is_none()) {
            self.state = Signon::Full;
            self.reached_full = true;
            emit(Line::Status(Status::Full { map: self.map.clone() }));
            self.echo(Signon::Full);
        }
    }

    fn datagram(&mut self, data: &[u8], dec: &mut Decoder, emit: &mut dyn FnMut(Line)) {
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
                rest.push(stream);
                continue;
            }
            self.feed(&stream, dec, emit);
            if let Some(rest) = &mut self.remap {
                rest.push(stream);
            }
        }
        self.flush(dec, emit);
        if let Some(p) = &self.pending {
            let want = (self.out_reliable_state >> p.subchannel) & 1;
            if (pkt.header.reliable_state >> p.subchannel) & 1 == want {
                self.pending = None;
                self.echo(self.state);
            } else if pkt.header.ack >= p.seq {
                // The server takes every copy it gets, so another goes only once this one is lost.
                self.transmit_pending();
            }
        }
        if self.sock.is_none() {
            self.promote(emit);
        }
        self.send(None, None);
    }

    fn poll(&mut self, emit: &mut dyn FnMut(Line)) {
        let now = Instant::now();
        if now.duration_since(self.last_inbound) > self.opt.silence {
            self.break_with(format!("no data for {:?}", self.opt.silence), None);
            return;
        }
        self.promote(emit);
        if self.state >= Signon::Spawn && now.duration_since(self.last_tick_ack) > TICK_ACK_EVERY {
            self.last_tick_ack = now;
            let body = messages::tick(self.ack_tick());
            self.send(None, Some(&body));
        }
    }

    fn feed(&mut self, stream: &MsgStream, dec: &mut Decoder, emit: &mut dyn FnMut(Line)) {
        let life = dec.feed(stream.bits.bytes(), stream.bits.nbits());
        if let Some(tap) = self.tap {
            tap.stream(life.bits(&stream.bits), stream.reliable, &life);
        }
        self.lifecycle(life, emit);
    }

    /// Ends a datagram's worth of streams: the tap's and the decoder's output go out.
    fn flush(&self, dec: &mut Decoder, emit: &mut dyn FnMut(Line)) {
        if let Some(tap) = self.tap {
            tap.flush();
        }
        for line in dec.take_output() {
            emit(line);
        }
    }

    /// Feeds the streams held since signon restarted into the new map's decoder. False when
    /// signon restarted once more: the rest is held again, for the map after this one.
    fn replay_streams(&mut self, streams: Vec<MsgStream>, dec: &mut Decoder, emit: &mut dyn FnMut(Line)) -> bool {
        let mut rest = streams.into_iter();
        for stream in rest.by_ref() {
            self.feed(&stream, dec, emit);
            if let Some(next_map) = &mut self.remap {
                next_map.push(stream);
                break;
            }
        }
        self.flush(dec, emit);
        match &mut self.remap {
            Some(next_map) => {
                next_map.extend(rest);
                false
            }
            None => true,
        }
    }

    /// Follows the current map until the session stops or breaks, or None when the map changes.
    fn follow(
        &mut self,
        dec: &mut Decoder,
        first: Option<&[u8]>,
        next: &mut dyn FnMut(&mut [u8]) -> Result<Next>,
        buf: &mut [u8],
        emit: &mut dyn FnMut(Line),
    ) -> Result<Option<End>> {
        if let Some(p) = first {
            self.datagram(p, dec, emit);
        }
        loop {
            match next(buf)? {
                Next::Datagram(n) => self.datagram(&buf[..n], dec, emit),
                Next::Idle => {}
                Next::Failed(why) => self.break_with(why, None),
                Next::Stop => return Ok(Some(End::Stopped)),
            }
            if self.sock.is_some() {
                self.poll(emit);
            }
            if dec.panicked() {
                self.break_with("decoder failed".to_string(), None);
            }
            if let Some((reason, fatal)) = self.broke.take() {
                return Ok(Some(End::Broken(Broken { reason, fatal, streamed: self.reached_full })));
            }
            if self.remap.is_some() {
                return Ok(None);
            }
        }
    }
}

/// What the datagram source has next; a datagram is the start of the buffer it was given.
enum Next {
    Datagram(usize),
    Idle,
    /// The datagram source failed (a socket error other than a timeout).
    Failed(String),
    Stop,
}

impl Next {
    /// A recorded datagram, copied into the buffer.
    fn recorded(d: &[u8], buf: &mut [u8]) -> Self {
        // Recorded from a buffer of the same size, so a datagram always fits.
        let n = d.len().min(buf.len());
        buf[..n].copy_from_slice(&d[..n]);
        Next::Datagram(n)
    }
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
    next: &mut dyn FnMut(&mut [u8]) -> Result<Next>,
    emit: &mut dyn FnMut(Line),
) -> Result<Outcome> {
    let mut decode_errors = 0;
    let mut first = first;
    let mut buf = vec![0u8; crate::net::MAX_DATAGRAM];
    let end = loop {
        // One decoder per map; the netchannel state survives map changes.
        let mut dec = Decoder::new(tables, s.opt.snapshot_hz)?;
        let replayed = s.remap.take().is_none_or(|streams| s.replay_streams(streams, &mut dec, emit));
        let end = if replayed { s.follow(&mut dec, first.take(), next, &mut buf, emit)? } else { None };
        decode_errors += dec.parse_errors();
        if let Some(end) = end {
            break end;
        }
    };
    Ok(Outcome { end, decode_errors })
}

fn finish(s: &Session<'_>, outcome: &Outcome, emit: &mut dyn FnMut(Line)) {
    emit(Line::Status(Status::Disconnected {
        reason: match &outcome.end {
            End::Broken(b) => b.reason.clone(),
            End::Stopped => "stopped".to_string(),
        },
        decode_errors: outcome.decode_errors,
        packets: s.rx.counters.packets,
        lost: s.rx.counters.lost,
    }));
}

pub fn run(
    conn: &Connection,
    tables: &Tables,
    opt: &Options,
    mut recorder: Option<&mut Recorder>,
    wait: Wait<'_>,
    tap: Option<&dyn Tap>,
    emit: &mut dyn FnMut(Line),
) -> Result<End> {
    let mut s = Session::new(Some(&conn.sock), conn.challenge, opt);
    s.tap = tap;
    if let Some(r) = recorder.as_deref_mut() {
        r.session(conn.challenge)?;
        if let Some(p) = &conn.pending {
            r.datagram(p)?;
        }
    }
    conn.sock.set_read_timeout(Some(READ_POLL))?;
    s.send_reliable(&messages::connected_reply(&opt.name), Some(0));
    let mut next = |buf: &mut [u8]| -> Result<Next> {
        if wait.over() {
            return Ok(Next::Stop);
        }
        match conn.sock.recv(buf) {
            Ok(n) => {
                if let Some(r) = recorder.as_deref_mut() {
                    r.datagram(&buf[..n])?;
                }
                Ok(Next::Datagram(n))
            }
            // Ctrl-C or SIGTERM during a read: the stop is noticed on the next turn. Reported as
            // a socket error, it would end the session with the wrong reason.
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {
                Ok(Next::Idle)
            }
            // E.g. ConnectionRefused: an ICMP port unreachable, the server is gone.
            Err(e) => Ok(Next::Failed(format!("socket error: {e}"))),
        }
    };
    let outcome = drive(&mut s, tables, conn.pending.as_deref(), &mut next, emit)?;
    // Always say goodbye: if the server already dropped us this is ignored, and if
    // we misjudged a break it frees the SourceTV slot instead of holding it 300 s.
    let bye = messages::goodbye();
    s.send(None, Some(&bye));
    s.send(None, Some(&bye));
    finish(&s, &outcome, emit);
    Ok(outcome.end)
}

mod replay;
pub use self::replay::replay;
pub(crate) use self::replay::replay_paced;

#[cfg(test)]
mod tests;
