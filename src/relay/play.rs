//! A relay that plays a demo instead of passing a server on, at a moment set from outside: a
//! room's playback. The demo may still be being written (a live match recorded as it goes).
//!
//! - Frames go out at their own pace, as far as the wanted moment. Off from it by more than a
//!   second, playback jumps: ahead, it decodes on without sending and every client gets a full
//!   update; back, it starts the demo again and every client signs on again.
//! - A full update is written from the decoded entities (see `decode::world`), for a client that
//!   joins, loses a frame or follows a jump; the frames after it are the demo's own.
//! - Paused, no frames go out, and the clients are told (`svc_SetPause`).
//! - With nobody in, nothing is decoded; after a while what was decoded is dropped.

use super::{Hub, Relay};
use crate::decode::{Decoder, Lifecycle, Tables};
use crate::demo::{self, Next, Packet};
use crate::net::bits::BitBuf;
use crate::output::Line;
use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Off from the wanted moment by more than this, playback jumps instead of catching up.
const JUMP: Duration = Duration::from_secs(1);
/// How long the player waits for a frame that is not due, or not written yet.
const NAP: Duration = Duration::from_millis(5);
/// With nobody in for this long, the decoded demo is dropped.
const IDLE: Duration = Duration::from_secs(60);

/// Where playback should be: at `tick` (a server tick of `demo`) at the instant `at`, going on
/// from there at `speed` unless paused.
#[derive(Debug, Clone, PartialEq)]
pub struct Want {
    /// The demo to play.
    pub demo: PathBuf,
    /// The server tick wanted at `at`.
    pub tick: u32,
    /// When `tick` was wanted.
    pub at: Instant,
    /// Paused at `tick`.
    pub paused: bool,
    /// How fast playback goes on from `tick`.
    pub speed: f64,
    /// `tick` counts from the demo's first frame instead.
    pub from_start: bool,
}

impl Want {
    /// The server tick wanted now, on a server ticking every `interval` seconds.
    fn now(&self, interval: f32) -> u32 {
        if self.paused || interval <= 0.0 {
            return self.tick;
        }
        let ahead = self.at.elapsed().as_secs_f64() * self.speed / f64::from(interval);
        self.tick.saturating_add(ahead as u32)
    }
}

/// The moment a playing relay follows, set as playback goes.
pub struct Playback {
    want: Mutex<Want>,
    /// Why the demo wanted cannot be played, if it cannot.
    problem: Mutex<Option<String>>,
}

impl Playback {
    /// Playback that starts out wanting `want`.
    pub fn new(want: Want) -> Self {
        Playback { want: Mutex::new(want), problem: Mutex::new(None) }
    }

    /// Where playback should be from now on.
    pub fn set(&self, want: Want) {
        *self.want.lock().unwrap_or_else(PoisonError::into_inner) = want;
    }

    /// Why the demo wanted cannot be played (not written yet, or of another TF2 build), as of the
    /// last client that tried.
    pub fn problem(&self) -> Option<String> {
        self.problem.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn get(&self) -> Want {
        self.want.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn set_problem(&self, problem: Option<String>) {
        *self.problem.lock().unwrap_or_else(PoisonError::into_inner) = problem;
    }
}

/// A demo being played: where in the file, and its decoded state.
struct Cursor {
    path: PathBuf,
    reader: demo::Reader<File>,
    dec: Decoder,
    /// The next packet, read but not played.
    next: Option<Packet>,
    /// A frame's server tick less its demo tick, as of the last frame.
    offset: i64,
    /// A frame came: the entities are there.
    started: bool,
    /// The server tick of the first frame.
    first: Option<i64>,
}

impl Cursor {
    /// Opens a demo and reads it up to its first frame, passing its signon to `hub`.
    fn open(path: &Path, hub: &Mutex<Hub>) -> Result<Cursor> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut reader = demo::Reader::new(file)?;
        let mut signon = Vec::new();
        let mut tables = None;
        let next = loop {
            match reader.next_packet()? {
                Next::Packet(Packet::Signon { data, .. }) => signon.push(data),
                Next::Packet(Packet::DataTables { .. }) => tables = Some(tables_of(path, &reader)?),
                Next::Packet(p @ Packet::Message { .. }) => break Some(p),
                Next::Packet(_) => {}
                // A live demo can be read before its first frame is written.
                Next::More if tables.is_some() && !signon.is_empty() => break None,
                Next::More => bail!("{} has no signon yet", path.display()),
                Next::Stop => bail!("{} has no frames", path.display()),
            }
        };
        let tables = tables.context("the demo has no data tables")?;
        let ours = Tables::from_demo_bytes(crate::builtin(crate::CURRENT_BUILD).context("no tables")?.tables)?;
        if tables.sendtable_crc() != ours.sendtable_crc() {
            bail!("the demo is of a TF2 build with other class tables, which a game client cannot play");
        }
        let mut dec = Decoder::new(&tables, 0.0)?;
        dec.keep_world();
        for data in signon {
            let bits = BitBuf::from_bytes(data);
            let life = dec.feed(bits.bytes(), bits.nbits());
            let mut h = lock(hub);
            h.upstream_stream(&bits, true, &life);
            h.upstream_flush();
        }
        Ok(Cursor { path: path.to_owned(), reader, dec, next, offset: 0, started: false, first: None })
    }

    /// The next packet, read ahead; None when the demo has no more yet.
    fn peek(&mut self) -> Result<Option<&Packet>> {
        if self.next.is_none() {
            self.next = match self.reader.next_packet()? {
                Next::Packet(p) => Some(p),
                Next::More | Next::Stop => None,
            };
        }
        Ok(self.next.as_ref())
    }

    /// The server tick the next frame has, as far as the frames so far tell; None when there is
    /// none yet. Other packets come first, as due.
    fn next_tick(&mut self) -> Result<Option<i64>> {
        let offset = self.offset;
        Ok(self.peek()?.map(|p| match p {
            Packet::Message { tick, .. } => i64::from(*tick) + offset,
            _ => i64::MIN,
        }))
    }

    /// Decodes the next packet; a frame's stream and what decoding found, or None for a packet
    /// that is not one.
    fn decode(&mut self) -> Option<(BitBuf, Lifecycle)> {
        let Some(Packet::Message { tick, data }) = self.next.take() else { return None };
        let bits = BitBuf::from_bytes(data);
        let life = self.dec.feed(bits.bytes(), bits.nbits());
        self.dec.take_output();
        self.offset = i64::from(self.dec.server_tick()) - i64::from(tick);
        self.started = true;
        self.first.get_or_insert(i64::from(self.dec.server_tick()));
        Some((bits, life))
    }

    /// Where playback is: the server tick of the last frame.
    fn tick(&self) -> i64 {
        i64::from(self.dec.server_tick())
    }
}

/// The tables of the demo at `path`, read up to where `reader` is: just past its data tables.
fn tables_of(path: &Path, reader: &demo::Reader<File>) -> Result<Tables> {
    let mut prefix = vec![0u8; reader.position() as usize];
    File::open(path)?.read_exact(&mut prefix)?;
    Tables::from_demo_bytes(&prefix)
}

fn lock(hub: &Mutex<Hub>) -> std::sync::MutexGuard<'_, Hub> {
    hub.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Relay {
    /// Plays demos to the clients, where `playback` wants them, until `stop`.
    pub fn play(&self, playback: &Playback, stop: &AtomicBool, emit: &mut dyn FnMut(Line)) -> Result<()> {
        self.announce(emit)?;
        self.serving(stop, || Player { relay: self, cur: None, idle: None, paused: false }.run(playback, stop))
    }
}

struct Player<'r> {
    relay: &'r Relay,
    cur: Option<Cursor>,
    /// Since when nobody is in.
    idle: Option<Instant>,
    /// What the clients were last told.
    paused: bool,
}

impl Player<'_> {
    fn run(mut self, playback: &Playback, stop: &AtomicBool) -> Result<()> {
        while !stop.load(Ordering::Relaxed) {
            if !self.anyone_in() {
                std::thread::sleep(NAP * 4);
                continue;
            }
            let want = playback.get();
            if self.cur.as_ref().is_none_or(|c| c.path != want.demo) {
                self.cur = None;
                match Cursor::open(&want.demo, &self.relay.hub) {
                    Ok(c) => {
                        self.cur = Some(c);
                        playback.set_problem(None);
                    }
                    Err(e) => {
                        // Not written yet, or not playable: try again in a while.
                        trace!("cannot play {}: {e:#}", want.demo.display());
                        playback.set_problem(Some(format!("{e:#}")));
                        std::thread::sleep(NAP * 40);
                        continue;
                    }
                }
            }
            self.step(&want)?;
        }
        Ok(())
    }

    /// Whether a client is in; after a while with none, the demo is dropped.
    fn anyone_in(&mut self) -> bool {
        let mut hub = self.relay.hub();
        if !hub.clients.is_empty() {
            self.idle = None;
            return true;
        }
        let since = *self.idle.get_or_insert_with(Instant::now);
        if since.elapsed() > IDLE && self.cur.is_some() {
            self.cur = None;
            hub.forget();
        }
        false
    }

    /// Plays on toward `want`: one frame, a jump, or a short wait.
    fn step(&mut self, want: &Want) -> Result<()> {
        let Some(c) = &mut self.cur else { return Ok(()) };
        let interval = c.dec.tick_interval();
        let first = c.first.unwrap_or(i64::MIN);
        let wanted = i64::from(want.now(interval));
        // Not before the demo starts.
        let target = if want.from_start { first.saturating_add(wanted) } else { wanted.max(first) };
        let jump = (JUMP.as_secs_f32() / interval.max(1e-3)) as i64;
        if c.started && target + jump < c.tick() {
            // Back: the demo again from its start, and every client signs on again.
            self.cur = None;
            self.cur = Some(Cursor::open(&want.demo, &self.relay.hub)?);
            return Ok(());
        }
        if want.paused != self.paused {
            self.paused = want.paused;
            self.relay.hub().set_paused(want.paused);
        }
        let Some(c) = &mut self.cur else { return Ok(()) };
        if c.started && c.tick() + jump < target {
            skim(c, &self.relay.hub, target)?;
            self.relay.hub().all_want_keyframe(Instant::now());
        }
        if self.relay.hub().wants_keyframe()
            && let Some(key) = c.dec.keyframe()
        {
            self.relay.hub().send_keyframe(&key, self.paused);
        }
        match c.next_tick()? {
            // The first frame, in full, is only decoded: every client starts from a keyframe of
            // wherever playback is.
            Some(_) if !c.started => {
                if let Some((bits, life)) = c.decode() {
                    self.relay.hub().skim(&bits, &life);
                } else {
                    c.next = None;
                }
            }
            Some(next) if !want.paused && next <= target => {
                if let Some((bits, life)) = c.decode() {
                    let mut hub = self.relay.hub();
                    hub.upstream_stream(&bits, false, &life);
                    hub.upstream_flush();
                } else {
                    c.next = None;
                }
            }
            _ => std::thread::sleep(NAP),
        }
        Ok(())
    }
}

/// Decodes on to `target` without sending: only string table changes reach the clients.
fn skim(c: &mut Cursor, hub: &Mutex<Hub>, target: i64) -> Result<()> {
    while let Some(next) = c.next_tick()?
        && next <= target
    {
        match c.decode() {
            Some((bits, life)) => lock(hub).skim(&bits, &life),
            None => c.next = None,
        }
    }
    Ok(())
}
