//! Watching one SourceTV server: connect, follow the match, reconnect with backoff.

use crate::capture::Recorder;
use crate::decode::Tables;
use crate::net::handshake;
use crate::session::{self, End, Options};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub struct Watch {
    /// The SourceTV HOST:PORT.
    pub target: String,
    pub password: String,
    pub name: String,
    pub snapshot_hz: f32,
    /// Overrides for builds that are not compiled in.
    pub tables: Option<PathBuf>,
    pub crc: Option<u32>,
    pub build: Option<String>,
    pub seconds: Option<f64>,
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
            seconds: None,
            record: None,
        }
    }
}

/// Send tables for a build: the given demo, or the compiled-in ones.
pub fn tables(build: &str, demo: Option<&PathBuf>) -> Result<Tables> {
    match (demo, crate::builtin(build)) {
        (Some(p), _) => Tables::from_demo(p),
        (None, Some(b)) => Tables::from_demo_bytes(b.tables),
        (None, None) => bail!("no built-in tables for build {build}; pass --tables SAME_BUILD.dem"),
    }
}

/// Sets `stop` on the first Ctrl-C so the session can say goodbye (otherwise STV
/// holds the slot for 300 s); a second Ctrl-C exits at once.
pub fn stop_on_ctrl_c(stop: &'static AtomicBool) -> Result<()> {
    ctrlc::set_handler(move || {
        if stop.swap(true, std::sync::atomic::Ordering::Relaxed) {
            std::process::exit(130);
        }
    })?;
    Ok(())
}

/// Runs until `stop`, the deadline, or a rejection that reconnecting cannot fix.
pub fn watch(w: &Watch, stop: &AtomicBool, emit: &mut dyn FnMut(Value)) -> Result<()> {
    let server: SocketAddr = w
        .target
        .to_socket_addrs()
        .with_context(|| format!("resolving {}", w.target))?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| anyhow!("no IPv4 address for {}", w.target))?;
    let build = match &w.build {
        Some(b) => b.clone(),
        None => handshake::a2s_info(server, Duration::from_secs(3))
            .map_or_else(|_| crate::CURRENT_BUILD.to_string(), |i| i.version),
    };
    let Some(crc) = w.crc.or_else(|| crate::builtin(&build).map(|b| b.crc)) else {
        bail!("no known SendTable CRC for build {build}; pass --crc");
    };
    let tables = tables(&build, w.tables.as_ref())?;
    let opt = Options { name: w.name.clone(), crc, snapshot_hz: w.snapshot_hz, silence: Duration::from_secs(15) };
    let deadline = w.seconds.map(|s| Instant::now() + Duration::from_secs_f64(s));
    let mut recorder = w.record.as_ref().map(|p| Recorder::create(p)).transpose()?;
    let mut backoff = Duration::from_secs(2);
    loop {
        if deadline.is_some_and(|d| Instant::now() >= d) || stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        emit(json!({"type": "status", "state": "connecting", "server": server.to_string(), "build": build}));
        match handshake::connect(server, &opt.name, &w.password, &build, Duration::from_secs(6)) {
            Ok(conn) => {
                backoff = Duration::from_secs(2);
                match session::run(&conn, &tables, &opt, recorder.as_mut(), stop, deadline, emit)? {
                    End::Stopped => return Ok(()),
                    End::Broken(why) if session::fatal(&why) => bail!(why),
                    End::Broken(_) => {}
                }
            }
            Err(e) => {
                emit(json!({"type": "status", "state": "connect_failed", "reason": e.to_string()}));
                if session::fatal(&e.to_string()) {
                    return Err(e);
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
        let until = Instant::now() + backoff;
        while Instant::now() < until && !stop.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
