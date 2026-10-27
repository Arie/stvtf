//! A `SourceTV` relay: the connection a watch already has, passed on to the TF2 clients (or
//! stvtf watchers) that connect to us, as `tv_relay` does. It costs the server no slot.
//!
//! Nothing is re-encoded: every client gets the server's own message bits.
//!
//! - Signon: the reliable messages of the server's signon are logged and replayed to each
//!   client, stopping after each `net_SignonState` until the client echoes it, as a server
//!   waits. After signon only string table messages join the log, so a late client still gets
//!   the tables as they are now; other reliable messages go to the clients already in.
//! - Entities: the server deltas each frame from the one we last acked, so a client can only
//!   follow from a full update on. The datagrams since the last full update are kept while
//!   few, and a client ready for entities gets them at once; once there are too many, it makes
//!   us ask the server for a full update (tick ack -1). From then on every datagram's
//!   unreliable part goes to the client unchanged. A client that loses a frame asks for a full
//!   update, and we ask the server.
//! - A map change re-runs the server's signon on our connection: the log starts again, and
//!   every client signs on again with it.
//! - A client in the game gets one packet per server datagram, as from the server itself.
//!
//! Connectionless replies are never larger than what asked for them, unless the address proved
//! it is real by answering a challenge, so the relay cannot be used to reflect traffic.

use crate::capture::{self, Record};
use crate::decode::{Lifecycle, Tables};
use crate::net::bits::{BitBuf, BitReader, BitWriter};
use crate::net::handshake::{self, FromClient};
use crate::net::messages::{self, NET_DISCONNECT, NET_SIGNONSTATE, NET_TICK, Signon, TYPE_BITS};
use crate::net::netchan::{self, Header, OutHeader};
use crate::net::receiver::Receiver;
use crate::net::reliable::{
    FRAGMENT_COUNT_BITS, FRAGMENT_SIZE, SIZE_BITS, START_FRAGMENT_BITS, SUBCHANNEL_BITS, SUBCHANNELS,
};
use crate::net::{MAX_DATAGRAM, compress, split};
use crate::output::{Line, Status};
use crate::session::{self, Options, Tap};
use crate::watch::{Watch, WatchError};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::fmt;
use std::hash::{BuildHasher, RandomState};
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// `STVTF_DEBUG=relay` traces handshakes, signon timing and what each client is sent.
static TRACE: LazyLock<bool> = LazyLock::new(|| std::env::var("STVTF_DEBUG").as_deref() == Ok("relay"));

macro_rules! trace {
    ($($arg:tt)*) => {
        if *TRACE {
            eprintln!("relay: {}", format_args!($($arg)*));
        }
    };
}

/// How often the trace sums up what each client was sent.
const TRACE_EVERY: Duration = Duration::from_secs(5);

const SVC_SERVERINFO: u8 = 8;
const SVC_CREATESTRINGTABLE: u8 = 12;
const SVC_UPDATESTRINGTABLE: u8 = 13;
const SVC_PACKETENTITIES: u8 = 26;

/// Clients a relay takes unless told otherwise.
pub const DEFAULT_SLOTS: usize = 10;
/// A slot count fits the `A2S_INFO` reply's byte.
const MAX_SLOTS: usize = 255;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(30);
/// A client hears from us at least this often, so its connection does not time out.
const KEEPALIVE: Duration = Duration::from_millis(100);
const READ_POLL: Duration = Duration::from_millis(5);
/// A client acks -1 until the full update it asked for arrives; only after this long is it
/// asking for another.
const FULL_UPDATE_GRACE: Duration = Duration::from_secs(1);
/// Datagrams kept since the last full update, for clients that join after it.
const MAX_KEYFRAME: usize = 64;
/// Reliable fragments sent per packet.
const FRAGMENTS_PER_PACKET: usize = 4;
/// Message bytes per reliable transfer, well under the engine's `NET_MAX_PAYLOAD`.
const MAX_TRANSFER: usize = 60_000;
/// After a full update, the next is asked of the server no sooner than this, however many
/// clients want one: each costs the server a full snapshot.
const FULL_UPDATE_GAP: Duration = Duration::from_secs(2);
/// Characters of a generated password.
const PASSWORD_LEN: usize = 10;
/// Longest string read from a client message.
const MAX_STRING: usize = 4096;

/// A reliable message a client signing on needs, and the signon state it waits for after it.
struct Logged {
    bits: BitBuf,
    gate: Option<Signon>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Still taking the signon log.
    Signon,
    /// Ready for entities, waiting for a full update.
    WaitingFull,
    /// Getting every datagram.
    Active,
}

/// One reliable transfer to a client, a few fragments per packet, on up to all eight
/// subchannels at once (as the engine sends one).
struct Outgoing {
    data: Vec<u8>,
    /// Set when `data` is compressed: the size it inflates to.
    uncompressed: Option<u32>,
    frags: usize,
    /// The first fragment not sent yet.
    next: usize,
    /// The block in flight on each subchannel.
    inflight: [Option<InFlight>; SUBCHANNELS as usize],
    /// Blocks lost on the way (first fragment, count), to send again.
    lost: Vec<(usize, usize)>,
}

struct InFlight {
    start: usize,
    num: usize,
    /// The sequence of the packet that carried them.
    seq: i32,
}

impl Outgoing {
    /// A transfer of the messages at the front of `queue`, up to `MAX_TRANSFER` bytes.
    fn take(queue: &mut Vec<BitBuf>) -> Self {
        let mut w = BitWriter::new();
        let mut taken = 0;
        for m in queue.iter() {
            if taken > 0 && (w.nbits() + m.nbits()).div_ceil(8) > MAX_TRANSFER {
                break;
            }
            w.write_buf(m);
            taken += 1;
        }
        queue.drain(..taken);
        // The bits after the last message are zeros: net_NOP, which a client skips.
        let mut data = w.into_bytes();
        let mut uncompressed = None;
        if data.len() > FRAGMENT_SIZE {
            let packed = compress::snappy_transfer(&data);
            if packed.len() < data.len() {
                uncompressed = Some(data.len() as u32);
                data = packed;
            }
        }
        let frags = data.len().div_ceil(FRAGMENT_SIZE);
        trace!("transfer of {} bytes ({:?} uncompressed), {frags} fragments", data.len(), uncompressed);
        Outgoing { data, uncompressed, frags, next: 0, inflight: Default::default(), lost: Vec::new() }
    }

    fn done(&self) -> bool {
        self.next == self.frags && self.lost.is_empty() && self.inflight.iter().all(Option::is_none)
    }

    /// The next block to send in the packet of sequence `seq`, if one can go now. The block
    /// with the first fragment opens the transfer at the client, which refuses any other
    /// that comes before it: until it is acked, it goes alone.
    fn next_block(&mut self, seq: i32) -> Option<BitBuf> {
        let sub = self.inflight.iter().position(Option::is_none)?;
        let opening = self.inflight.iter().flatten().any(|f| f.start == 0) || self.lost.iter().any(|b| b.0 == 0);
        let (start, num) = if let Some(i) = self.lost.iter().position(|b| !opening || b.0 == 0) {
            self.lost.remove(i)
        } else if !opening && self.next < self.frags {
            let block = (self.next, FRAGMENTS_PER_PACKET.min(self.frags - self.next));
            self.next += block.1;
            block
        } else {
            return None;
        };
        self.inflight[sub] = Some(InFlight { start, num, seq });
        Some(self.region(sub as u8, start, num))
    }

    /// The reliable region carrying fragments `start..start + num` on subchannel `sub`.
    fn region(&self, sub: u8, start: usize, num: usize) -> BitBuf {
        let single = self.frags <= FRAGMENTS_PER_PACKET;
        let mut w = BitWriter::new();
        w.write_bits(u32::from(sub), SUBCHANNEL_BITS);
        w.write_bit(true);
        w.write_bit(!single);
        if !single {
            w.write_bits(start as u32, START_FRAGMENT_BITS);
            w.write_bits(num as u32, FRAGMENT_COUNT_BITS);
        }
        if start == 0 {
            if !single {
                w.write_bit(false); // not a file
            }
            w.write_bit(self.uncompressed.is_some());
            if let Some(size) = self.uncompressed {
                w.write_bits(size, SIZE_BITS);
            }
            if single {
                w.write_varint32(self.data.len() as u32);
            } else {
                w.write_bits(self.data.len() as u32, SIZE_BITS);
            }
        }
        let end = ((start + num) * FRAGMENT_SIZE).min(self.data.len());
        w.write_bytes(&self.data[start * FRAGMENT_SIZE..end]);
        w.write_bit(false); // nothing on the file stream
        w.finish()
    }
}

struct Client {
    name: String,
    challenge: u32,
    /// The nonce of the client's connect: another one from the same address is a new connection.
    nonce: u32,
    rx: Receiver,
    out_seq: i32,
    /// Our ack bits for the client's reliable subchannels.
    in_reliable_state: u8,
    /// A subchannel's bit flips when the client acks a block of ours on it (the engine's way).
    out_reliable_state: u8,
    /// When the client connected, and what it was sent since the last trace.
    connected: Instant,
    sent_with_tail: u32,
    sent_bare: u32,
    phase: Phase,
    /// When `phase` last changed.
    since: Instant,
    /// The next log entry to send.
    cursor: usize,
    /// The signon state the client must echo before it gets more of the log.
    gate: Option<Signon>,
    /// Reliable messages not yet in a transfer.
    queue: Vec<BitBuf>,
    outgoing: Option<Outgoing>,
    last_heard: Instant,
    last_sent: Instant,
    /// The client said goodbye.
    gone: Option<String>,
}

impl Client {
    fn new(name: String, challenge: u32, nonce: u32, now: Instant) -> Self {
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
            outgoing: None,
            last_heard: now,
            last_sent: now,
            gone: None,
        }
    }

    fn set_phase(&mut self, phase: Phase, now: Instant) {
        self.phase = phase;
        self.since = now;
    }

    /// Queues the log from the cursor up to the next signon state the client must echo.
    fn pump(&mut self, log: &[Logged]) {
        while self.gate.is_none()
            && let Some(m) = log.get(self.cursor)
        {
            self.queue.push(m.bits.clone());
            self.cursor += 1;
            self.gate = m.gate;
        }
    }

    /// Whether the client is through the log and gets messages as they come.
    fn caught_up(&self, log: &[Logged]) -> bool {
        self.phase != Phase::Signon && self.cursor == log.len()
    }

    /// The region to send in the packet of sequence `seq`, if a transfer block can go.
    fn next_region(&mut self, seq: i32) -> Option<BitBuf> {
        if self.outgoing.is_none() && !self.queue.is_empty() {
            self.outgoing = Some(Outgoing::take(&mut self.queue));
        }
        self.outgoing.as_mut()?.next_block(seq)
    }

    /// Reads the client's acks of our transfer blocks from a packet header.
    fn acked(&mut self, h: &Header) {
        let Some(o) = &mut self.outgoing else { return };
        for (sub, slot) in o.inflight.iter_mut().enumerate() {
            let Some(f) = slot.take_if(|f| h.ack >= f.seq) else { continue };
            let bit = 1 << sub;
            if (self.out_reliable_state ^ h.reliable_state) & bit == 0 {
                // The client got a later packet but not this block.
                o.lost.push((f.start, f.num));
            } else {
                self.out_reliable_state ^= bit;
            }
        }
        if o.done() {
            self.outgoing = None;
        }
    }

    /// Still signing on, with more to send or an echo to wait for: a paced replay holds for it.
    fn signing_on(&self) -> bool {
        self.phase == Phase::Signon && (self.gate.is_some() || !self.queue.is_empty() || self.outgoing.is_some())
    }
}

/// A client's messages the relay acts on.
#[derive(Debug, PartialEq)]
enum ClientMsg {
    Signon(Signon),
    Tick(i32),
    Disconnect(String),
}

/// The messages of one client stream, up to the first it cannot skip.
fn client_messages(bits: &BitBuf) -> Vec<ClientMsg> {
    let mut out = Vec::new();
    let mut r = BitReader::new(bits.bytes());
    let _ = read_client_messages(&mut r, bits.nbits(), &mut out);
    out
}

fn read_client_messages(r: &mut BitReader<'_>, nbits: usize, out: &mut Vec<ClientMsg>) -> Result<()> {
    while nbits.saturating_sub(r.pos()) >= TYPE_BITS {
        match r.read_bits(TYPE_BITS)? {
            0 => {} // net_NOP
            NET_DISCONNECT => out.push(ClientMsg::Disconnect(r.read_string(MAX_STRING)?)),
            2 => {
                // net_File: transfer id, file name, requested.
                r.skip(32)?;
                r.read_string(MAX_STRING)?;
                r.skip(1)?;
            }
            NET_TICK => {
                out.push(ClientMsg::Tick(r.read_i32()?));
                r.skip(32)?; // frame time and its deviation
            }
            4 => {
                r.read_string(MAX_STRING)?; // net_StringCmd
            }
            messages::NET_SETCONVAR => {
                for _ in 0..2 * usize::from(r.read_u8()?) {
                    r.read_string(MAX_STRING)?;
                }
            }
            NET_SIGNONSTATE => {
                let state = r.read_u8()?;
                r.skip(32)?; // spawncount
                if let Some(s) = Signon::from_u8(state) {
                    out.push(ClientMsg::Signon(s));
                }
            }
            messages::CLC_CLIENTINFO => {
                r.skip(32 + 32 + 1 + 32)?; // spawncount, CRC, is HLTV, friends id
                r.read_string(MAX_STRING)?;
                for _ in 0..4 {
                    if r.read_bit()? {
                        r.skip(32)?; // custom file CRC
                    }
                }
                r.skip(1)?; // is replay
            }
            9 => {
                // clc_Move: new and backup command counts, then the commands.
                r.skip(4 + 3)?;
                let len = r.read_bits(16)? as usize;
                r.skip(len)?;
            }
            10 => {
                let len = r.read_bits(16)? as usize; // clc_VoiceData
                r.skip(len)?;
            }
            11 => r.skip(32 + 1)?, // clc_BaselineAck
            12 => r.skip(512)?,    // clc_ListenEvents: a bit per game event
            13 => {
                // clc_RespondCvarValue: cookie, status, name, value.
                r.skip(32 + 4)?;
                r.read_string(MAX_STRING)?;
                r.read_string(MAX_STRING)?;
            }
            16 => {
                let len = r.read_bits(32)? as usize; // clc_CmdKeyValues
                r.skip(len.saturating_mul(8))?;
            }
            id => bail!("client message {id} not understood"),
        }
    }
    Ok(())
}

/// Everything the upstream and client threads share.
struct Hub {
    sock: UdpSocket,
    password: String,
    slots: usize,
    /// Keys the challenge of each address, so none need be stored before it connects.
    challenge_key: RandomState,
    clients: HashMap<SocketAddr, Client>,
    log: Vec<Logged>,
    /// Every reliable message joins the log until the first entities after a signon starts.
    signon_open: bool,
    /// This datagram's unreliable part, and whether it holds a full update.
    tail: Option<BitBuf>,
    tail_full: bool,
    /// The unreliable parts since the last full update, while there are few.
    keyframe: Option<Vec<BitBuf>>,
    map: String,
    build: String,
    split_group: i32,
    seen_client: bool,
    /// When the last full update came, to space out asking for more.
    last_full: Option<Instant>,
    traced: Instant,
}

impl Hub {
    fn new(sock: UdpSocket, config: &RelayConfig) -> Self {
        Hub {
            sock,
            password: config.password.clone(),
            slots: config.slots.min(MAX_SLOTS),
            challenge_key: RandomState::new(),
            clients: HashMap::new(),
            log: Vec::new(),
            signon_open: true,
            tail: None,
            tail_full: false,
            keyframe: None,
            map: String::new(),
            build: crate::CURRENT_BUILD.to_string(),
            split_group: 0,
            seen_client: false,
            last_full: None,
            traced: Instant::now(),
        }
    }

    /// The server started a signon (connected, or a new map): so does every client.
    fn restart_signon(&mut self, now: Instant) {
        self.log.clear();
        self.signon_open = true;
        self.keyframe = None;
        for c in self.clients.values_mut() {
            c.cursor = 0;
            c.queue.clear();
            c.gate = c.gate.filter(|g| *g == Signon::Connected);
            c.set_phase(Phase::Signon, now);
        }
    }

    fn upstream_stream(&mut self, bits: &BitBuf, reliable: bool, life: &Lifecycle) {
        let now = Instant::now();
        if let Some((_, map)) = &life.server_info {
            self.map.clone_from(map);
        }
        // The server sends the first full update after signon reliably (`net_Tick` and the
        // entities): like any entities, it goes to the clients in the datagram's unreliable
        // part, so it is never in the log and reaches a client in order with the deltas after it.
        let entities = life.spans.iter().any(|s| s.id == SVC_PACKETENTITIES);
        if !reliable || entities {
            if entities {
                self.signon_open = false;
            }
            self.tail_full |= life.full_update;
            // Cut off the padding after the last message: followed by more, it would be read as one.
            let end = life.spans.last().map_or(0, |s| s.bits.end);
            let mut w = BitWriter::new();
            if let Some(earlier) = &self.tail {
                w.write_buf(earlier);
            }
            w.write_buf(&BitReader::slice(bits.bytes(), 0..end));
            self.tail = Some(w.finish());
            return;
        }
        for span in &life.spans {
            let msg = BitReader::slice(bits.bytes(), span.bits.clone());
            if span.id == SVC_SERVERINFO {
                self.restart_signon(now);
            }
            if self.signon_open || matches!(span.id, SVC_CREATESTRINGTABLE | SVC_UPDATESTRINGTABLE) {
                let gate = (u32::from(span.id) == NET_SIGNONSTATE)
                    .then(|| BitReader::at(msg.bytes(), TYPE_BITS).read_u8().ok().and_then(Signon::from_u8))
                    .flatten()
                    .filter(|s| matches!(s, Signon::New | Signon::PreSpawn | Signon::Spawn));
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

    /// Sends what the upstream datagram brought.
    fn upstream_flush(&mut self) {
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
            if full && c.phase == Phase::WaitingFull {
                c.set_phase(Phase::Active, now);
            }
            if c.phase == Phase::Active {
                // One packet per server datagram, as the server sends: a packet of our own
                // would show in a client's net_graph as one without entities.
                send(&self.sock, &mut self.split_group, *addr, c, tail.as_ref(), true, now);
            } else {
                burst(&self.sock, &mut self.split_group, *addr, c, false, now);
            }
        }
    }

    /// A client is ready for entities: from the kept datagrams if there are, or a full update.
    fn wants_entities(&mut self, addr: SocketAddr, now: Instant) {
        let Some(c) = self.clients.get_mut(&addr) else { return };
        match &self.keyframe {
            Some(frames) => {
                c.set_phase(Phase::Active, now);
                for t in frames {
                    send(&self.sock, &mut self.split_group, addr, c, Some(t), false, now);
                }
            }
            None => c.set_phase(Phase::WaitingFull, now),
        }
    }

    fn datagram(&mut self, from: SocketAddr, data: &[u8], now: Instant) {
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
                        if c.gate.is_some_and(|g| s >= g) {
                            c.gate = None;
                        }
                        ready |= s >= Signon::Spawn && c.phase == Phase::Signon && c.gate.is_none();
                    }
                    ClientMsg::Tick(-1) => {
                        ready |= c.phase == Phase::Active && now.duration_since(c.since) > FULL_UPDATE_GRACE;
                    }
                    ClientMsg::Tick(_) => {}
                    ClientMsg::Disconnect(reason) => c.gone = Some(reason),
                }
            }
        }
        if let Some(reason) = &c.gone {
            trace!("{} ({from}) left: {}", c.name.escape_debug(), reason.escape_debug());
            self.clients.remove(&from);
            return;
        }
        c.pump(&self.log);
        // An active client hears from us with the next server datagram. During signon its acks
        // free subchannels for more of the transfer, and its reliable data is acked at once.
        if c.phase != Phase::Active {
            burst(&self.sock, &mut self.split_group, from, c, pkt.header.reliable(), now);
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
                    trace!("{} ({from}) gave a wrong password", name.escape_debug());
                    handshake::build_reject(nonce, "#GameUI_ServerRejectBadPassword")
                } else if self.clients.len() >= self.slots && !self.clients.contains_key(&from) {
                    handshake::build_reject(nonce, "#GameUI_ServerRejectServerFull")
                } else {
                    trace!("{} ({from}) connected", name.escape_debug());
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
        let _ = self.sock.send_to(&reply, from);
    }

    /// Resends, keepalives and timeouts.
    fn service(&mut self, now: Instant) {
        self.clients.retain(|addr, c| {
            let alive = now.duration_since(c.last_heard) < CLIENT_TIMEOUT;
            if !alive {
                trace!("{} ({addr}) timed out", c.name.escape_debug());
            }
            alive
        });
        for (addr, c) in &mut self.clients {
            let keepalive = now.duration_since(c.last_sent) > KEEPALIVE;
            if c.phase == Phase::Active {
                if keepalive {
                    send(&self.sock, &mut self.split_group, *addr, c, None, true, now);
                }
            } else {
                burst(&self.sock, &mut self.split_group, *addr, c, keepalive, now);
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

    fn shutdown(&mut self, reason: &str) {
        let bye = messages::disconnect(reason);
        let now = Instant::now();
        for (addr, c) in &mut self.clients {
            for _ in 0..2 {
                send(&self.sock, &mut self.split_group, *addr, c, Some(&bye), true, now);
            }
        }
        self.clients.clear();
    }

    /// A paced replay waits for a client, and for clients signing on.
    fn hold(&self) -> bool {
        !self.seen_client || self.clients.values().any(Client::signing_on)
    }
}

/// Sends a client a packet: `tail` and a transfer block if one can go, or nothing when neither
/// is there and `always` is not set. Whether it sent one.
fn send(
    sock: &UdpSocket,
    split_group: &mut i32,
    addr: SocketAddr,
    c: &mut Client,
    tail: Option<&BitBuf>,
    always: bool,
    now: Instant,
) -> bool {
    let region = c.next_region(c.out_seq);
    if region.is_none() && tail.is_none() && !always {
        return false;
    }
    if tail.is_some() {
        c.sent_with_tail += 1;
    } else {
        c.sent_bare += 1;
    }
    let header = OutHeader {
        sequence: c.out_seq,
        ack: c.rx.in_seq,
        challenge: c.challenge,
        reliable_state: c.in_reliable_state,
    };
    c.out_seq += 1;
    c.last_sent = now;
    let mut pkt = netchan::build_packet(&header, region.as_ref(), tail);
    if pkt.len() > split::SPLIT_SIZE {
        pkt = compress::compress_datagram(&pkt);
    }
    // A send error is a client gone or unreachable: its timeout drops it.
    if pkt.len() > split::SPLIT_SIZE {
        *split_group = split_group.wrapping_add(1);
        for part in split::split(&pkt, *split_group) {
            let _ = sock.send_to(&part, addr);
        }
    } else {
        let _ = sock.send_to(&pkt, addr);
    }
    true
}

/// Sends a client every transfer block that can go now, one packet each; at least one packet
/// when `always` is set.
fn burst(sock: &UdpSocket, split_group: &mut i32, addr: SocketAddr, c: &mut Client, always: bool, now: Instant) {
    let mut always = always;
    for _ in 0..SUBCHANNELS {
        if !send(sock, split_group, addr, c, None, always, now) {
            break;
        }
        always = false;
    }
}

/// Where a relay listens and whom it lets in. `Debug` keeps the password out of logs.
#[derive(Clone)]
pub struct RelayConfig {
    /// HOST:PORT to listen on; port 0 picks one (the `relay` status line says which).
    pub listen: String,
    /// What clients must give as the server password; empty lets anyone in.
    pub password: String,
    /// How many clients at once, up to 255.
    pub slots: usize,
}

impl RelayConfig {
    /// [`DEFAULT_SLOTS`] slots and a new random password.
    pub fn new(listen: &str) -> Self {
        RelayConfig { listen: listen.to_string(), password: random_password(), slots: DEFAULT_SLOTS }
    }
}

impl fmt::Debug for RelayConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayConfig")
            .field("listen", &self.listen)
            .field("password", &"<redacted>")
            .field("slots", &self.slots)
            .finish()
    }
}

/// A password of letters and digits, hard to guess and easy to type in a console.
pub fn random_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    (0..PASSWORD_LEN)
        .map(|_| char::from(ALPHABET[(crate::net::random_u64() % ALPHABET.len() as u64) as usize]))
        .collect()
}

/// A relay: a socket clients connect to, fed by one upstream session.
pub struct Relay {
    sock: UdpSocket,
    hub: Mutex<Hub>,
}

impl Relay {
    /// Listens for clients as `config` says.
    pub fn bind(config: &RelayConfig) -> Result<Relay> {
        let sock = UdpSocket::bind(&config.listen).with_context(|| format!("binding {}", config.listen))?;
        let hub = Mutex::new(Hub::new(sock.try_clone()?, config));
        Ok(Relay { sock, hub })
    }

    /// The address clients connect to.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.sock.local_addr()
    }

    fn hub(&self) -> MutexGuard<'_, Hub> {
        self.hub.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Relays a `SourceTV` server, reconnecting as [`crate::watch::watch`] does, until `stop`.
    pub fn watch(&self, w: &Watch, stop: &AtomicBool, emit: &mut dyn FnMut(Line)) -> Result<(), WatchError> {
        if let Some(b) = &w.build {
            self.hub().build.clone_from(b);
        }
        self.announce(emit)?;
        self.serving(stop, || crate::watch::watch_with(w, stop, Some(self), emit))
    }

    /// Relays the first session of a capture as if it came live, `speed` times as fast. It
    /// starts when the first client connects, and pauses while a client signs on.
    pub fn replay(
        &self,
        records: &[Record<'_>],
        tables: &Tables,
        speed: f64,
        stop: &AtomicBool,
        emit: &mut dyn FnMut(Line),
    ) -> Result<()> {
        let mut sessions = records.iter().skip_while(|r| r.kind != capture::Kind::Session as u8);
        let Some(first) = sessions.next() else { bail!("no session in the capture") };
        let challenge = u32::from_le_bytes(first.data.try_into().context("session record is not a 4-byte challenge")?);
        let datagrams: Vec<(u64, &[u8])> = sessions
            .take_while(|r| r.kind != capture::Kind::Session as u8)
            .filter(|r| r.kind == capture::Kind::Datagram as u8)
            .map(|r| (r.t_us, r.data))
            .collect();
        let opt = Options { snapshot_hz: 0.0, ..Options::default() };
        self.announce(emit)?;
        let hold = || self.hub().hold();
        self.serving(stop, || {
            session::replay_paced(challenge, &datagrams, tables, &opt, speed, self, &hold, stop, emit)
        })
    }

    fn announce(&self, emit: &mut dyn FnMut(Line)) -> Result<()> {
        emit(Line::Status(Status::Relay { listen: self.local_addr()?.to_string() }));
        Ok(())
    }

    /// Serves clients on another thread while `upstream` runs.
    fn serving<T>(&self, stop: &AtomicBool, upstream: impl FnOnce() -> T) -> T {
        let done = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| self.serve_clients(stop, &done));
            let out = upstream();
            done.store(true, Ordering::Relaxed);
            out
        })
    }

    fn serve_clients(&self, stop: &AtomicBool, done: &AtomicBool) {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        let _ = self.sock.set_read_timeout(Some(READ_POLL));
        while !stop.load(Ordering::Relaxed) && !done.load(Ordering::Relaxed) {
            match self.sock.recv_from(&mut buf) {
                Ok((n, from)) => self.hub().datagram(from, &buf[..n], Instant::now()),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                // E.g. an ICMP port unreachable from a client that went away.
                Err(_) => {}
            }
            self.hub().service(Instant::now());
        }
        self.hub().shutdown("Relay shutting down");
    }
}

impl Tap for Relay {
    fn stream(&self, bits: &BitBuf, reliable: bool, life: &Lifecycle) {
        self.hub().upstream_stream(bits, reliable, life);
    }

    fn flush(&self) {
        self.hub().upstream_flush();
    }

    fn want_full_update(&self) -> bool {
        let hub = self.hub();
        hub.last_full.is_none_or(|t| t.elapsed() >= FULL_UPDATE_GAP)
            && hub.clients.values().any(|c| c.phase == Phase::WaitingFull)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::reliable::Reassembler;

    #[test]
    fn client_messages_from_stvtf_parse() {
        let mut w = BitWriter::new();
        w.write_buf(&messages::client_info(7, 0x5FCD_4733, true));
        w.write_buf(&messages::tick(-1));
        w.write_buf(&messages::goodbye());
        assert_eq!(
            client_messages(&w.finish()),
            [ClientMsg::Signon(Signon::New), ClientMsg::Tick(-1), ClientMsg::Disconnect("Disconnect by user.".into())]
        );
        let got = client_messages(&messages::connected_reply("x"));
        assert_eq!(got, [ClientMsg::Signon(Signon::Connected)]);
    }

    #[test]
    fn transfers_reassemble_a_few_fragments_at_a_time() {
        // Incompressible, so it goes out raw in five packets.
        let mut x = 1u32;
        let mut noise = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        };
        let mut queue: Vec<BitBuf> = (0..1300)
            .map(|_| {
                let mut w = BitWriter::new();
                w.write_bits(noise(), 32);
                w.finish()
            })
            .collect();
        let want: Vec<u8> = queue.iter().flat_map(|b| b.bytes().to_vec()).collect();
        let mut o = Outgoing::take(&mut queue);
        assert_eq!(queue.len(), 0);
        assert!(o.uncompressed.is_none());
        let mut r = Reassembler::default();
        let mut got = Vec::new();
        while o.next < o.frags {
            let num = FRAGMENTS_PER_PACKET.min(o.frags - o.next);
            got.extend(r.feed(o.region(0, o.next, num).bytes(), 0).unwrap().completed);
            o.next += num;
        }
        assert_eq!(got, [want]);
    }

    #[test]
    fn the_first_block_goes_alone_then_all_subchannels_at_once() {
        let mut x = 1u32;
        let noise = (0..20_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        let mut queue = vec![BitBuf::from_bytes(noise)];
        let mut c = Client::new("x".into(), 1, 2, Instant::now());
        c.queue.append(&mut queue);
        let first = c.next_region(1).unwrap();
        assert!(c.next_region(2).is_none(), "nothing before the first block is acked");
        let mut r = Reassembler::default();
        assert_eq!(r.feed(first.bytes(), 0).unwrap().completed.len(), 0);
        let ack = |seq, state| Header {
            sequence: seq,
            ack: seq,
            flags: 0,
            reliable_state: state,
            choked: None,
            challenge: None,
            body_offset: 0,
        };
        c.acked(&ack(1, 0b1));
        let blocks: Vec<BitBuf> = (2..).map_while(|seq| c.next_region(seq)).collect();
        assert_eq!(blocks.len(), usize::from(SUBCHANNELS));
        // The client gets all but the one on subchannel 2, which comes again.
        for (i, b) in blocks.iter().enumerate().filter(|(i, _)| *i != 2) {
            assert_eq!(r.feed(b.bytes(), 0).unwrap().subchannel, i as u8);
        }
        c.acked(&ack(9, 0b1111_1011 ^ 0b1));
        let again = c.next_region(10).unwrap();
        assert_eq!(
            BitReader::new(again.bytes()).read_bits(SUBCHANNEL_BITS).unwrap(),
            0,
            "lost block on the first free subchannel"
        );
        let mut done = r.feed(again.bytes(), 0).unwrap().completed;
        let mut state = 0b1111_1011 ^ 0b1 ^ 0b1;
        let mut seq = 10;
        while c.outgoing.is_some() {
            let sub = (0..SUBCHANNELS).find(|i| c.outgoing.as_ref().unwrap().inflight[usize::from(*i)].is_some());
            if let Some(sub) = sub {
                state ^= 1 << sub;
            }
            c.acked(&ack(seq, state));
            seq += 1;
            while let Some(b) = c.next_region(seq) {
                done.extend(r.feed(b.bytes(), 0).unwrap().completed);
                let sub = BitReader::new(b.bytes()).read_bits(SUBCHANNEL_BITS).unwrap();
                state ^= 1 << sub;
                c.acked(&ack(seq, state));
                seq += 1;
            }
        }
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].len(), 20_000);
    }

    #[test]
    fn compressed_transfer_reassembles() {
        let mut queue = vec![BitBuf::from_bytes(vec![7u8; 5000])];
        let o = Outgoing::take(&mut queue);
        assert!(o.uncompressed.is_some() && o.frags <= FRAGMENTS_PER_PACKET, "{} fragments", o.frags);
        let got = Reassembler::default().feed(o.region(0, 0, o.frags).bytes(), 0).unwrap().completed;
        assert_eq!(got, [vec![7u8; 5000]]);
    }
}
