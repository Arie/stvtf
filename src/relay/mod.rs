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
//! - Reliable messages go one transfer at a time, the next when the last is acked, so they can
//!   reach a client up to about a round trip behind the datagrams: 12 ticks at most, on 8% of
//!   a capture's lines, with 200 ms of ping. Their own order stays.
//!
//! Connectionless replies are never larger than what asked for them, unless the address proved
//! it is real by answering a challenge, so the relay cannot be used to reflect traffic.

use crate::capture::{self, Record};
use crate::decode::{Lifecycle, Tables};
use crate::lock;
use crate::net::MAX_DATAGRAM;
use crate::net::bits::BitBuf;
use crate::output::{Line, Status};
use crate::session::{self, Options, Tap};
use crate::watch::{Watch, WatchError};
use anyhow::{Context, Result, bail};
#[cfg(fuzzing)]
use client::Client;
use client::Phase;
use hub::Hub;
use std::fmt;
#[cfg(fuzzing)]
use std::hash::BuildHasher;
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// `STVTF_DEBUG=relay` traces handshakes, signon timing and what each client is sent.
pub(crate) static TRACE: LazyLock<bool> = LazyLock::new(|| std::env::var("STVTF_DEBUG").as_deref() == Ok("relay"));

macro_rules! trace {
    ($($arg:tt)*) => {
        if *$crate::relay::TRACE {
            eprintln!("relay: {}", format_args!($($arg)*));
        }
    };
}

/// Where what a relay notes goes: stderr, unless a host takes it ([`notes_to`]).
static NOTES: std::sync::OnceLock<Notes> = std::sync::OnceLock::new();
type Notes = Box<dyn Fn(&str) + Send + Sync>;

/// Has `take` given what relays note from here on (clients coming and going, jumps, demos that
/// will not play), a line each, instead of stderr. Once: a second call changes nothing.
pub fn notes_to(take: impl Fn(&str) + Send + Sync + 'static) {
    let _ = NOTES.set(Box::new(take));
}

pub(crate) fn note(line: &str) {
    match NOTES.get() {
        Some(take) => take(line),
        None => eprintln!("{line}"),
    }
}

#[cfg(test)]
#[test]
fn a_clients_address_is_noted_without_its_end() {
    assert_eq!(whence("203.0.113.57:27005".parse().unwrap()), "203.0.113.x:27005");
    assert_eq!(whence("[2001:db8:1:2:3:4:5:6]:27005".parse().unwrap()), "2001:db8:1::x:27005");
}

/// What is worth keeping of a relay's life, traced or not: a line a client, a jump or a failure.
macro_rules! note {
    ($($arg:tt)*) => {
        $crate::relay::note(&format!("relay: {}", format_args!($($arg)*)))
    };
}

/// A client's address as it is noted: without the end of it, the port kept to tell two apart.
pub(crate) fn whence(addr: SocketAddr) -> String {
    match addr.ip() {
        std::net::IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            format!("{a}.{b}.{c}.x:{}", addr.port())
        }
        std::net::IpAddr::V6(ip) => {
            format!("{:x}:{:x}:{:x}::x:{}", ip.segments()[0], ip.segments()[1], ip.segments()[2], addr.port())
        }
    }
}

mod client;
mod client_msg;
mod hub;
mod out;
mod play;
mod server_msg;
mod transfer;
pub(crate) use client_msg::{ClientMsg, client_messages};
pub use play::{Beam, Camera, Light, Note, Playback, VoiceIn, Want};

/// How often the trace sums up what each client was sent.
const TRACE_EVERY: Duration = Duration::from_secs(5);

/// TF2's spectator panel.
const SPECTATORS: &str = "specgui";
/// How long after a HUD reset, and after going into the game, a client gets the notes again.
const NOTES_AFTER_RESET: Duration = Duration::from_millis(300);
const NOTES_AFTER_ENTERING: Duration = Duration::from_secs(2);

/// Clients a relay takes unless told otherwise.
pub const DEFAULT_SLOTS: usize = 10;
/// The most clients a relay takes: a slot count fits the `A2S_INFO` reply's byte.
pub const MAX_SLOTS: usize = 255;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(30);
/// A client hears from us at least this often, so its connection does not time out.
const KEEPALIVE: Duration = Duration::from_millis(100);
const READ_POLL: Duration = Duration::from_millis(5);
/// A client acks -1 until the full update it asked for arrives; only after this long is it
/// asking for another.
const FULL_UPDATE_GRACE: Duration = Duration::from_secs(1);
/// Datagrams kept since the last full update, for clients that join after it.
const MAX_KEYFRAME: usize = 64;
/// After a full update, the next is asked of the server no sooner than this, however many
/// clients want one: each costs the server a full snapshot.
const FULL_UPDATE_GAP: Duration = Duration::from_secs(2);
/// Characters of a generated password.
const PASSWORD_LEN: usize = 10;

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

    /// The port clients connect to: which relay a note is of.
    pub(crate) fn port(&self) -> u16 {
        self.sock.local_addr().map_or(0, |a| a.port())
    }

    /// How many clients are in.
    pub fn clients(&self) -> usize {
        self.hub().clients.len()
    }

    fn hub(&self) -> MutexGuard<'_, Hub> {
        lock(&self.hub)
    }

    /// Relays a `SourceTV` server, reconnecting as [`crate::watch::watch`] does, until `stop`.
    pub fn watch(&self, w: &Watch, stop: &AtomicBool, emit: &mut dyn FnMut(Line)) -> Result<(), WatchError> {
        self.watch_also(w, stop, None, emit)
    }

    /// [`Relay::watch`], passing the traffic to `also` as well.
    pub(crate) fn watch_also(
        &self,
        w: &Watch,
        stop: &AtomicBool,
        also: Option<&dyn Tap>,
        emit: &mut dyn FnMut(Line),
    ) -> Result<(), WatchError> {
        self.announce(emit)?;
        let both = also.map(|a| session::Both(self, a));
        let tap: &dyn Tap = match &both {
            Some(b) => b,
            None => self,
        };
        self.serving(stop, || crate::watch::watch_with(w, stop, Some(tap), emit))
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

    fn server_build(&self, build: &str) {
        self.hub().build = build.to_string();
    }
}

/// A relay with one client in, for `crate::fuzzing`.
#[cfg(fuzzing)]
pub(crate) struct FuzzBench {
    hub: Hub,
    client: SocketAddr,
    challenge: u32,
    now: Instant,
}

#[cfg(fuzzing)]
impl FuzzBench {
    pub(crate) fn new() -> Self {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let client: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let config = RelayConfig { password: String::new(), ..RelayConfig::new("127.0.0.1:0") };
        let mut hub = Hub::new(sock, &config);
        let now = Instant::now();
        let challenge = hub.challenge_key.hash_one(client) as u32;
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

#[cfg(test)]
mod tests;
