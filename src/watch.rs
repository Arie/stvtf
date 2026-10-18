//! Watching one SourceTV server: connect, follow the match, reconnect with backoff.

use crate::capture::Recorder;
use crate::decode::Tables;
use crate::net::handshake::{self, Rejected};
use crate::session::{self, End, Fatal, Options};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::fmt;
use std::hash::{BuildHasher, RandomState};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const BACKOFF_MIN: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// What to watch and how. `Debug` is implemented by hand to keep the password out of logs.
pub struct Watch {
    /// The SourceTV HOST:PORT.
    pub target: String,
    /// The server's `tv_password`.
    pub password: String,
    /// Spectator name shown on the server.
    pub name: String,
    /// Snapshot rate cap; 0 disables snapshots.
    pub snapshot_hz: f32,
    /// Overrides for builds that are not compiled in.
    pub tables: Option<PathBuf>,
    pub crc: Option<u32>,
    pub build: Option<String>,
    /// Stop after this long.
    pub duration: Option<Duration>,
    /// Save the raw inbound datagrams here (see `capture`). The file must not exist yet.
    pub record: Option<PathBuf>,
}

impl Watch {
    pub fn new(target: &str, password: &str) -> Self {
        Watch {
            target: target.to_string(),
            password: password.to_string(),
            name: "stvmap".to_string(),
            snapshot_hz: 20.0,
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
    /// The server refused us for a reason reconnecting cannot fix. `reason` is the
    /// server's text.
    Rejected { kind: Fatal, reason: String },
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
/// The process can have only one Ctrl-C handler, so this fails if one is already set.
pub fn stop_on_ctrl_c(stop: &'static AtomicBool) -> Result<()> {
    ctrlc::set_handler(move || {
        if stop.swap(true, Ordering::Relaxed) {
            std::process::exit(130);
        }
    })?;
    Ok(())
}

/// Runs until `stop`, the duration, or a rejection that reconnecting cannot fix,
/// passing every event and snapshot to `emit`. Blocks the calling thread.
pub fn watch(w: &Watch, stop: &AtomicBool, emit: &mut dyn FnMut(Value)) -> Result<(), WatchError> {
    let server: SocketAddr = w
        .target
        .to_socket_addrs()
        .with_context(|| format!("resolving {}", w.target))?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| anyhow!("no IPv4 address for {}", w.target))?;
    let build = match &w.build {
        Some(b) => b.clone(),
        None => {
            handshake::a2s_version(server, Duration::from_secs(3)).unwrap_or_else(|_| crate::CURRENT_BUILD.to_string())
        }
    };
    let Some(crc) = w.crc.or_else(|| crate::builtin(&build).map(|b| b.crc)) else {
        return Err(anyhow!("no known SendTable CRC for build {build}; pass --crc").into());
    };
    let tables = tables(&build, w.tables.as_deref())?;
    let opt = Options { name: w.name.clone(), crc, snapshot_hz: w.snapshot_hz, ..Options::default() };
    let deadline = w.duration.map(|d| Instant::now() + d);
    let mut recorder = w.record.as_deref().map(Recorder::create).transpose()?;
    let mut backoff = BACKOFF_MIN;
    loop {
        if deadline.is_some_and(|d| Instant::now() >= d) || stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        emit(json!({"type": "status", "state": "connecting", "server": server.to_string(), "build": build}));
        let streamed = match handshake::connect(server, &opt.name, &w.password, &build, Duration::from_secs(6)) {
            Ok(conn) => match session::run(&conn, &tables, &opt, recorder.as_mut(), stop, deadline, emit)? {
                End::Stopped => return Ok(()),
                End::Broken(b) => {
                    if let Some(kind) = b.fatal {
                        return Err(WatchError::Rejected { kind, reason: b.reason });
                    }
                    b.streamed
                }
            },
            Err(e) => {
                emit(json!({"type": "status", "state": "connect_failed", "reason": e.to_string()}));
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
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// ±20 %, so watchers of many servers that dropped together do not retry in lockstep.
fn jitter(d: Duration) -> Duration {
    let r = RandomState::new().hash_one(Instant::now()) % 401;
    d.mul_f64(0.8 + r as f64 / 1000.0)
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
