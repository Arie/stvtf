//! A relay on its port: its settings, and what it serves.

use std::fmt;
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use super::client::Phase;
pub use super::handover::Handover;
use super::hub::Hub;
use super::tuning::{FULL_UPDATE_GAP, READ_POLL};
use crate::Result;
use crate::capture::{self, Record};
use crate::decode::{Lifecycle, Tables};
use crate::error::{Context, bail};
use crate::net::MAX_DATAGRAM;
use crate::net::bits::BitBuf;
use crate::output::{Line, Status};
use crate::session::{self, Options, Tap};
use crate::sync::lock;
use crate::watch::{Watch, WatchError};

/// Clients a relay takes unless told otherwise.
pub const DEFAULT_SLOTS: usize = 10;

/// The most clients a relay takes: a slot count fits the `A2S_INFO` reply's byte.
pub const MAX_SLOTS: usize = 255;

/// Characters of a generated password.
pub(super) const PASSWORD_LEN: usize = 10;

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
    pub(super) sock: UdpSocket,
    /// The port clients connect to: log lines name the relay by it.
    pub(super) port: u16,
    pub(super) hub: Mutex<Hub>,
    /// Clients were taken over (see [`Relay::take_over`]), and where their playback was: the
    /// player goes on from there.
    pub(super) taken_over: Mutex<Option<super::handover::TakenOver>>,
}

impl Relay {
    /// Listens for clients as `config` says.
    pub fn bind(config: &RelayConfig) -> Result<Relay> {
        let sock = UdpSocket::bind(&config.listen).with_context(|| format!("bind {}", config.listen))?;
        let port = sock.local_addr().map_or(0, |a| a.port());
        let hub = Mutex::new(Hub::new(sock.try_clone()?, config));
        Ok(Relay { sock, port, hub, taken_over: Mutex::new(None) })
    }

    /// Takes in the clients a relay on this address handed over
    /// ([`Playback::hand_over`](super::Playback::hand_over)),
    /// before [`Relay::play`]: they go on where they were with a full update, or failing that
    /// sign on again, connected still. False if the handover cannot be decoded: none is taken.
    pub fn take_over(&self, h: &Handover) -> bool {
        if self.hub().take_over(h, Instant::now()).is_none() {
            return false;
        }
        *lock(&self.taken_over) = Some(super::handover::TakenOver(h.played.clone()));
        true
    }

    /// The address clients connect to.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.sock.local_addr()
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// How many clients are in.
    pub fn clients(&self) -> usize {
        self.hub().clients.len()
    }

    pub(super) fn hub(&self) -> MutexGuard<'_, Hub> {
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
        let Some(first) = sessions.next() else { bail!("cannot find a session in the capture") };
        let challenge = u32::from_le_bytes(first.data.try_into().context("read the session's 4-byte challenge")?);
        let datagrams: Vec<(u64, &[u8])> = sessions
            .take_while(|r| r.kind != capture::Kind::Session as u8)
            .filter(|r| r.kind == capture::Kind::Datagram as u8)
            .map(|r| (r.t_us, r.data))
            .collect();
        let opt = Options { snapshot_hz: 0.0, ..Options::default() };
        self.announce(emit)?;
        let wait = || self.hub().replay_waits();
        self.serving(stop, || {
            session::replay_paced(challenge, &datagrams, tables, &opt, speed, self, &wait, stop, emit)
        })
    }

    pub(super) fn announce(&self, emit: &mut dyn FnMut(Line)) -> Result<()> {
        emit(Line::Status(Status::Relay { listen: self.local_addr()?.to_string() }));
        Ok(())
    }

    /// Serves clients on another thread while `upstream` runs.
    pub(super) fn serving<T>(&self, stop: &AtomicBool, upstream: impl FnOnce() -> T) -> T {
        let done = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| self.serve_clients(stop, &done));
            let out = upstream();
            done.store(true, Ordering::Relaxed);
            out
        })
    }

    pub(super) fn serve_clients(&self, stop: &AtomicBool, done: &AtomicBool) {
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
        if !self.hub().silent {
            self.hub().shutdown("Relay shutting down");
        }
    }
}

impl Tap for Relay {
    fn stream(&self, bits: &BitBuf, reliable: bool, life: &Lifecycle) {
        let part = if reliable { super::hub::Part::Reliable } else { super::hub::Part::Unreliable };
        self.hub().upstream_stream(bits, part, life);
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
        self.hub().admission.build = build.to_string();
    }
}
