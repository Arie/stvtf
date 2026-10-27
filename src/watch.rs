//! Watching one `SourceTV` server: connect, follow the match, reconnect with backoff.

use crate::capture::Recorder;
use crate::decode::Tables;
use crate::net::handshake::{self, GamePort, Rejected};
use crate::output::{Line, Status};
use crate::session::DEFAULT_SNAPSHOT_HZ;
use crate::session::{self, End, Fatal, Options};
use anyhow::{Context, Result, anyhow, bail};
use std::fmt;

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const BACKOFF_MIN: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Retry waits vary this much either way.
const JITTER: f64 = 0.2;
/// How often a wait checks for a stop.
const STOP_POLL: Duration = Duration::from_millis(100);
/// `SourceTV`'s default port: the game port + 5.
const TV_PORT_OFFSET: u16 = 5;
/// The exit status of a process interrupted by SIGINT (128 + 2).
const EXIT_INTERRUPTED: i32 = 130;
/// How long to wait for the server's build (`A2S_INFO`), and for it to accept us.
const A2S_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);

/// What to watch and how. `Debug` is implemented by hand to keep the password out of logs.
pub struct Watch {
    /// The `SourceTV` HOST:PORT.
    pub target: String,
    /// The server's `tv_password`.
    pub password: String,
    /// Spectator name shown on the server.
    pub name: String,
    /// Snapshot rate cap; 0 disables snapshots.
    pub snapshot_hz: f32,
    /// A demo of the server's build, for builds whose tables are not compiled in.
    pub tables: Option<PathBuf>,
    /// The build's `SendTable` CRC, for builds not compiled in.
    pub crc: Option<u32>,
    /// The server build; asked from the server (`A2S_INFO`) when not set.
    pub build: Option<String>,
    /// Stop after this long.
    pub duration: Option<Duration>,
    /// Save the raw inbound datagrams here (see `capture`). The file must not exist yet.
    pub record: Option<PathBuf>,
}

impl Watch {
    /// Defaults for everything but the address and password.
    pub fn new(target: &str, password: &str) -> Self {
        Watch {
            target: target.to_string(),
            password: password.to_string(),
            name: session::DEFAULT_NAME.to_string(),
            snapshot_hz: DEFAULT_SNAPSHOT_HZ,
            tables: None,
            crc: None,
            build: None,
            duration: None,
            record: None,
        }
    }
}

impl fmt::Debug for Watch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watch")
            .field("target", &self.target)
            .field("password", &"<redacted>")
            .field("name", &self.name)
            .field("snapshot_hz", &self.snapshot_hz)
            .field("tables", &self.tables)
            .field("crc", &self.crc)
            .field("build", &self.build)
            .field("duration", &self.duration)
            .field("record", &self.record)
            .finish()
    }
}

/// Why [`watch`] gave up.
#[derive(Debug)]
pub enum WatchError {
    /// The server refused us for a reason reconnecting cannot fix.
    Rejected {
        /// What kind of rejection.
        kind: Fatal,
        /// The server's text.
        reason: String,
    },
    /// Setup failed (address, build, tables, capture file) or local I/O failed.
    Other(anyhow::Error),
}

impl fmt::Display for WatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Server text: escape control characters so it cannot drive the terminal.
            WatchError::Rejected { reason, .. } => write!(f, "{}", reason.escape_debug()),
            WatchError::Other(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for WatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WatchError::Rejected { .. } => None,
            WatchError::Other(e) => Some(e.as_ref()),
        }
    }
}

impl From<anyhow::Error> for WatchError {
    fn from(e: anyhow::Error) -> Self {
        WatchError::Other(e)
    }
}

/// The IPv4 address of a `HOST:PORT` (`SourceTV` is IPv4 only).
pub fn resolve(target: &str) -> Result<SocketAddr> {
    target
        .to_socket_addrs()
        .with_context(|| format!("resolving {target}"))?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| anyhow!("no IPv4 address for {target}"))
}

/// Send tables for a build: the given demo, or the compiled-in ones.
pub fn tables(build: &str, demo: Option<&Path>) -> Result<Tables> {
    match (demo, crate::builtin(build)) {
        (Some(p), _) => Tables::from_demo(p),
        (None, Some(b)) => Tables::from_demo_bytes(b.tables),
        (None, None) => bail!("no built-in tables for build {build}; pass --tables SAME_BUILD.dem"),
    }
}

/// Sets `stop` on the first Ctrl-C so the session can say goodbye (otherwise STV
/// holds the slot for 300 s); a second Ctrl-C exits at once.
///
/// SIGTERM (`docker stop`, a deploy) counts as Ctrl-C too.
///
/// The process can have only one Ctrl-C handler, so this fails if one is already set.
pub fn stop_on_ctrl_c(stop: &'static AtomicBool) -> Result<()> {
    ctrlc::set_handler(move || {
        if stop.swap(true, Ordering::Relaxed) {
            std::process::exit(EXIT_INTERRUPTED);
        }
    })?;
    Ok(())
}

/// Runs until `stop`, the duration, or a rejection that reconnecting cannot fix,
/// passing every event and snapshot to `emit`. Blocks the calling thread.
pub fn watch(w: &Watch, stop: &AtomicBool, emit: &mut dyn FnMut(Line)) -> Result<(), WatchError> {
    watch_with(w, stop, None, emit)
}

/// [`watch`], passing the server's traffic to a tap as well.
pub(crate) fn watch_with(
    w: &Watch,
    stop: &AtomicBool,
    tap: Option<&dyn session::Tap>,
    emit: &mut dyn FnMut(Line),
) -> Result<(), WatchError> {
    let mut server = resolve(&w.target)?;
    // A2S_INFO is unauthenticated (and taken from any address, see handshake), so a
    // build we cannot use falls back to the current one: a forged reply must not end the
    // watch, and a server really on another build refuses our class tables anyway.
    let usable = |b: &str| crate::builtin(b).is_some() || (w.crc.is_some() && w.tables.is_some());
    let mut server_build = None;
    let build = match &w.build {
        Some(b) => b.clone(),
        None => match handshake::a2s_version(server, A2S_TIMEOUT) {
            Ok(b) if usable(&b) => b,
            Ok(b) => {
                server_build = Some(b);
                crate::CURRENT_BUILD.to_string()
            }
            Err(_) => crate::CURRENT_BUILD.to_string(),
        },
    };
    let Some(crc) = w.crc.or_else(|| crate::builtin(&build).map(|b| b.crc)) else {
        return Err(anyhow!("no known SendTable CRC for build {build}; pass --crc").into());
    };
    let tables = tables(&build, w.tables.as_deref())?;
    let opt = Options { name: w.name.clone(), crc, snapshot_hz: w.snapshot_hz, ..Options::default() };
    let deadline = w.duration.map(|d| Instant::now() + d);
    let mut recorder = w.record.as_deref().map(Recorder::create).transpose()?;
    let mut backoff = BACKOFF_MIN;
    let mut tried_tv_port = false;
    loop {
        if deadline.is_some_and(|d| Instant::now() >= d) || stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        emit(Line::Status(Status::Connecting {
            server: server.to_string(),
            build: build.clone(),
            server_build: server_build.clone(),
        }));
        let streamed = match handshake::connect(server, &opt.name, &w.password, &build, CONNECT_TIMEOUT) {
            Ok(conn) => match session::run(&conn, &tables, &opt, recorder.as_mut(), stop, deadline, tap, emit)? {
                End::Stopped => return Ok(()),
                End::Broken(b) => {
                    if let Some(kind) = b.fatal {
                        return Err(WatchError::Rejected { kind, reason: b.reason });
                    }
                    b.streamed
                }
            },
            Err(e) => {
                emit(Line::Status(Status::ConnectFailed { reason: e.to_string() }));
                // Given the game port, try SourceTV's default place next to it: game port + 5.
                if e.downcast_ref::<GamePort>().is_some()
                    && !tried_tv_port
                    && let Some(port) = server.port().checked_add(TV_PORT_OFFSET)
                {
                    tried_tv_port = true;
                    server.set_port(port);
                    continue;
                }
                if let Some(r) = e.downcast_ref::<Rejected>()
                    && let Some(kind) = Fatal::from_reason(&r.0)
                {
                    return Err(WatchError::Rejected { kind, reason: e.to_string() });
                }
                false
            }
        };
        // Only a session that streamed proves the server works; one that failed or was
        // dropped during signon backs off further, so a server that accepts and drops
        // us at once is not retried every 2 s forever.
        let wait = if streamed {
            backoff = BACKOFF_MIN;
            BACKOFF_MIN
        } else {
            let w = backoff;
            backoff = (backoff * 2).min(BACKOFF_MAX);
            w
        };
        let mut until = Instant::now() + jitter(wait);
        if let Some(d) = deadline {
            until = until.min(d);
        }
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(STOP_POLL);
        }
    }
}

/// ± `JITTER`, so watchers of many servers that dropped together do not retry in lockstep.
fn jitter(d: Duration) -> Duration {
    const STEPS: u64 = 1000;
    let spread = (crate::net::random_u64() % (STEPS + 1)) as f64 / STEPS as f64;
    d.mul_f64(1.0 - JITTER + 2.0 * JITTER * spread)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_within_twenty_percent() {
        for _ in 0..100 {
            let j = jitter(Duration::from_secs(10));
            assert!(j >= Duration::from_secs(8) && j <= Duration::from_secs(12), "{j:?}");
        }
    }

    #[test]
    fn debug_hides_the_password() {
        let w = Watch::new("127.0.0.1:27020", "hunter2");
        assert!(!format!("{w:?}").contains("hunter2"));
    }
}
