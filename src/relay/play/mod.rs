//! A relay that plays a demo instead of passing a server on, at a moment set from outside: a
//! room's playback. The demo may still be being written (a live match recorded as it goes).
//!
//! - Frames go out at their own pace, as far as the wanted moment. Off from it by more than a
//!   second, playback jumps: it decodes on (back, from the demo's start) without sending, and
//!   every client gets a full update of where it went. Only another demo file signs them on again.
//! - A full update is written from the decoded entities (see `decode::world`), for a client that
//!   joins, loses a frame or follows a jump; the frames after it are the demo's own.
//! - Paused, no frames go out. The clients are not told, so their clock runs on and what is drawn
//!   for them (beams) shows and fades; the round timers are held instead, and going on they get a
//!   full update of where it is. The match's own pauses in the demo they are told (`svc_SetPause`).
//! - Before a full update sets the clients' clock back, a `net_Tick` ahead runs their beams out.
//! - With nobody in, nothing is decoded; after a while what was decoded is dropped.
//! - Why it cannot play (not written yet, another build's demo, a read error) is told through
//!   [`Playback::problem`], and it tries again.

mod index;
mod notes;

use super::{Hub, Relay};
pub use crate::decode::beams::Beam;
use crate::decode::{Decoder, Lifecycle, Span, Tables};
use crate::demo::{self, Next, Packet};
use crate::lock;
use crate::net::bits::{BitBuf, BitReader, BitWriter};
use crate::net::messages::{self, TYPE_BITS};
use crate::output::Line;
use anyhow::{Context, Result, bail};
use index::Index;
pub use notes::Note;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Off from the wanted moment by more than this, playback jumps instead of catching up.
const JUMP: Duration = Duration::from_secs(1);
/// What [`Cursor::next_tick`] gives for a packet that is not a frame: due at once.
const NOT_A_FRAME: i64 = i64::MIN;
/// How long the player waits for a frame that is not due, or not written yet.
const NAP: Duration = Duration::from_millis(5);
/// How long the player waits for a client while nobody is in.
const IDLE_NAP: Duration = Duration::from_millis(20);
/// With nobody in for this long, the decoded demo is dropped.
const IDLE: Duration = Duration::from_secs(60);
/// Frames kept that a keyframe's client may need: those since the frame the next one builds on.
const RECENT: usize = 128;
/// How long the player waits after an error, or a demo it cannot open, before trying it again.
const RETRY: Duration = Duration::from_millis(200);
/// How often the camera cue is repeated, for clients that looked away.
const CUE_EVERY: Duration = Duration::from_secs(3);
/// The most beams waiting to be shown: more are dropped.
const MAX_BEAMS_QUEUED: usize = 4096;
/// How often kept beams are shown again; each shows until then, its life rounded up to the step
/// it is sent in, so they overlap by under a tenth of a second.
const KEPT_EVERY: Duration = Duration::from_secs(2);
/// Kept beams sent a second at most, and at once: well within what a client takes besides the
/// frames (a burst of hundreds of datagrams loses some, frames among them).
const KEPT_RATE: f64 = 800.0;
const KEPT_BURST: f64 = 48.0;
/// A clock set back by more than this (seconds) is first put past the beams.
const SET_BACK: f64 = 0.5;
/// How long the clients get to draw a frame with their clock ahead.
const FLUSH: Duration = Duration::from_millis(150);

/// The kept beams shown: which of them were last handed over, when each was last sent, and
/// how many may be sent now (refilled at [`KEPT_RATE`]).
struct Kept {
    version: u64,
    sent: HashMap<BeamKey, Instant>,
    tokens: f64,
    refilled: Instant,
}

/// A beam as shown, but for how long.
type BeamKey = ([u32; 3], [u32; 3], [u8; 3], u32, u8);

fn beam_key(b: &Beam) -> BeamKey {
    (b.from.map(f32::to_bits), b.to.map(f32::to_bits), b.rgb, b.width.to_bits(), b.speed)
}

/// The tick past the beams, if the clock at `now` goes back to `key` with beams until `until`.
fn past_beams(now: f64, key: f64, until: f64, interval: f32) -> Option<i32> {
    (now < until && now - key > SET_BACK && interval > 0.0).then(|| ((until + 1.0) / f64::from(interval)).ceil() as i32)
}

/// The `SendTable` CRC of this build's class tables, which a game client builds its classes from.
static OUR_CRC: LazyLock<Option<u32>> = LazyLock::new(|| {
    let b = crate::builtin(crate::CURRENT_BUILD)?;
    Tables::from_demo_bytes(b.tables).ok().map(|t| t.sendtable_crc())
});

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
    /// Overrides the demo's director camera.
    pub camera: Option<Camera>,
}

/// A camera the clients' director is told to take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Camera {
    /// In the eyes of a player.
    Follow {
        /// Their user id.
        player: u16,
    },
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
    /// Beams to show the clients in the game, once.
    beams: Mutex<Vec<Beam>>,
    /// Beams the clients in the game keep seeing, until replaced, and how many times they were.
    kept: Mutex<(u64, Arc<Vec<Beam>>)>,
    /// Notes they keep seeing, until replaced, and how many times they were.
    notes: Mutex<(u64, Arc<Vec<Note>>)>,
}

impl Playback {
    /// Playback that starts out wanting `want`.
    pub fn new(want: Want) -> Self {
        Playback {
            want: Mutex::new(want),
            problem: Mutex::new(None),
            beams: Mutex::new(Vec::new()),
            kept: Mutex::new((0, Arc::default())),
            notes: Mutex::new((0, Arc::default())),
        }
    }

    /// Notes the clients in the game keep seeing (a room's), in place of those before: shown
    /// again after every full update; those gone are hidden.
    pub fn keep_notes(&self, notes: Vec<Note>) {
        let mut kept = lock(&self.notes);
        kept.0 += 1;
        kept.1 = Arc::new(notes);
    }

    /// Beams the clients in the game keep seeing (a room's marker), in place of those kept before:
    /// shown again every `KEPT_EVERY`, each for as long, so the old go within that and nothing
    /// shows twice over.
    pub fn keep(&self, beams: Vec<Beam>) {
        let mut kept = lock(&self.kept);
        kept.0 += 1;
        kept.1 = Arc::new(beams);
    }

    fn kept(&self) -> (u64, Arc<Vec<Beam>>) {
        lock(&self.kept).clone()
    }

    /// Shows `beams` to the clients in the game now; those that are not miss them.
    pub fn draw(&self, beams: &[Beam]) {
        let mut queued = lock(&self.beams);
        if queued.len() + beams.len() <= MAX_BEAMS_QUEUED {
            queued.extend_from_slice(beams);
        }
    }

    fn take_beams(&self) -> Vec<Beam> {
        std::mem::take(&mut *lock(&self.beams))
    }

    /// Where playback should be from now on.
    pub fn set(&self, want: Want) {
        *lock(&self.want) = want;
    }

    /// Why the demo wanted cannot be played (not written yet, or of another TF2 build), as of the
    /// last client that tried.
    pub fn problem(&self) -> Option<String> {
        lock(&self.problem).clone()
    }

    fn get(&self) -> Want {
        lock(&self.want).clone()
    }

    fn set_problem(&self, problem: Option<String>) {
        *lock(&self.problem) = problem;
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
    /// The demo's stop packet was read: no more frames come.
    ended: bool,
    /// The last frames decoded, by server tick, cut after their last message.
    recent: VecDeque<(u32, BitBuf)>,
    /// The match itself was paused (`svc_SetPause` in the demo) as of the last frame decoded: a
    /// client that jumps here, past the frame that said so, is told.
    paused: bool,
    /// Where `next` starts in the file.
    next_pos: u64,
    /// Every string table change since the signon, in order: what a checkpoint starts from.
    tables: Vec<Arc<BitBuf>>,
}

impl Cursor {
    /// Opens a demo and reads it up to its first frame, passing its signon to `hub`.
    fn open(path: &Path, hub: &Mutex<Hub>) -> Result<Cursor> {
        Self::open_with(path, Some(hub))
    }

    /// [`Cursor::open`], giving the signon to `hub` if any: none when the clients have the demo's
    /// already (a jump in it), or to decode it on the side.
    fn open_with(path: &Path, hub: Option<&Mutex<Hub>>) -> Result<Cursor> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut reader = demo::Reader::new(file)?;
        let mut signon = Vec::new();
        let mut tables = None;
        let mut synced = false;
        let next = loop {
            match reader.next_packet()? {
                Next::Packet(Packet::Signon { data, .. }) => signon.push(data),
                Next::Packet(Packet::DataTables { .. }) => tables = Some(tables_of(path, &reader)?),
                Next::Packet(p @ Packet::Message { .. }) => break Some(p),
                Next::Packet(Packet::SyncTick { .. }) => synced = true,
                Next::Packet(_) => {}
                // A live demo can be read before its first frame is written; its signon is all
                // there once the sync tick that ends it is.
                Next::More if tables.is_some() && synced => break None,
                Next::More => bail!("{} has no signon yet", path.display()),
                Next::Stop => bail!("{} has no frames", path.display()),
            }
        };
        let tables = tables.context("the demo has no data tables")?;
        if Some(tables.sendtable_crc()) != *OUR_CRC {
            bail!("the demo is of a TF2 build with other class tables, which a game client cannot play");
        }
        let mut dec = Decoder::new(&tables, 0.0)?;
        dec.keep_world();
        let mut spawncount = None;
        for data in signon {
            let bits = BitBuf::from_bytes(data);
            let life = dec.feed(bits.bytes(), bits.nbits());
            let Some(hub) = hub else { continue };
            if let Some((count, _)) = &life.server_info {
                spawncount = Some(*count);
            }
            let (bits, life) = match spawncount {
                Some(count) => with_spawncount(&bits, &life, count),
                None => (bits, life),
            };
            let mut h = lock(hub);
            h.upstream_stream(&bits, true, &life);
            h.upstream_flush();
        }
        Ok(Cursor {
            path: path.to_owned(),
            reader,
            dec,
            next,
            offset: 0,
            started: false,
            first: None,
            ended: false,
            recent: VecDeque::default(),
            paused: false,
            next_pos: 0,
            tables: Vec::new(),
        })
    }

    /// The next packet, read ahead; None when the demo has no more yet.
    fn peek(&mut self) -> Option<&Packet> {
        if self.next.is_none() && !self.ended {
            self.next_pos = self.reader.position();
            self.next = match self.reader.next_packet() {
                Ok(Next::Packet(p)) => Some(p),
                Ok(Next::More) => None,
                Ok(Next::Stop) => {
                    self.ended = true;
                    None
                }
                // A demo cut off in a broken packet (a recording stopped short, say) ends there.
                Err(e) => {
                    trace!("{} ends in an unreadable packet: {e:#}", self.path.display());
                    self.ended = true;
                    None
                }
            };
        }
        self.next.as_ref()
    }

    /// The data of the packet read ahead, if it is a frame.
    fn next_frame(&self) -> Option<&[u8]> {
        match &self.next {
            Some(Packet::Message { data, .. }) => Some(data),
            _ => None,
        }
    }

    /// The server tick the next frame has: its own `net_Tick`, or as far as the frames so far tell
    /// (a demo's tick less the server's, which a pause changes: an STV demo has no frames while the
    /// server is paused); None when there is none yet, [`NOT_A_FRAME`] for another packet.
    fn next_tick(&mut self) -> Option<i64> {
        let offset = self.offset;
        self.peek()?;
        Some(match &self.next {
            Some(Packet::Message { tick, data }) => {
                let guess = i64::from(*tick) + offset;
                self.dec.frame_tick(data).map_or(guess, i64::from)
            }
            _ => NOT_A_FRAME,
        })
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
        for span in life.spans.iter().filter(|s| u32::from(s.id) == messages::SVC_SETPAUSE) {
            if let Ok(p) = BitReader::at(bits.bytes(), span.bits.start + TYPE_BITS).read_bit() {
                self.paused = p;
            }
        }
        self.tables.extend(super::table_changes(&bits, &life).map(Arc::new));
        self.recent.push_back((self.dec.server_tick(), life.cut(&bits)));
        if self.recent.len() > RECENT {
            self.recent.pop_front();
        }
        Some((bits, life))
    }

    /// Where playback is: the server tick of the last frame.
    fn tick(&self) -> i64 {
        i64::from(self.dec.server_tick())
    }
}

/// A signon stream with its `net_SignonState`s at `count`, the `ServerInfo`'s: a game client refuses
/// any other ("Received wrong spawn count"), and an STV demo's differ from its `ServerInfo`'s.
fn with_spawncount(bits: &BitBuf, life: &Lifecycle, count: i32) -> (BitBuf, Lifecycle) {
    let mut w = BitWriter::new();
    let mut spans = Vec::with_capacity(life.spans.len());
    for span in &life.spans {
        let msg = BitReader::slice(bits.bytes(), span.bits.clone());
        let msg = match super::signon_state(span.id, &msg) {
            Some(s) => messages::signonstate(s, count),
            None => msg,
        };
        let start = w.nbits();
        w.write_buf(&msg);
        spans.push(Span { id: span.id, bits: start..w.nbits() });
    }
    let fixed = Lifecycle {
        signon: life.signon.clone(),
        server_info: life.server_info.clone(),
        tick: life.tick,
        spans,
        ..Lifecycle::default()
    };
    (w.finish(), fixed)
}

/// String table changes as messages of their own.
fn owned(tables: &[Arc<BitBuf>]) -> Vec<BitBuf> {
    tables.iter().map(|t| (**t).clone()).collect()
}

/// The tables of the demo at `path`, read up to where `reader` is: just past its data tables.
fn tables_of(path: &Path, reader: &demo::Reader<File>) -> Result<Tables> {
    let mut prefix = vec![0u8; reader.position() as usize];
    File::open(path)?.read_exact(&mut prefix)?;
    Tables::from_demo_bytes(&prefix)
}

impl Relay {
    /// Plays demos to the clients, where `playback` wants them, until `stop`.
    pub fn play(&self, playback: &Playback, stop: &AtomicBool, emit: &mut dyn FnMut(Line)) -> Result<()> {
        self.announce(emit)?;
        self.serving(stop, || {
            self.hub().mode = super::Mode::Play { paused: false };
            Player {
                relay: self,
                cur: None,
                idle: None,
                paused: false,
                traced: None,
                last_target: None,
                dry: false,
                problem: None,
                index: None,
                cued: None,
                kept: Kept { version: 0, sent: HashMap::new(), tokens: KEPT_BURST, refilled: Instant::now() },
                shown_notes: notes::Shown::default(),
                laser: None,
                clock: None,
                beams_until: 0.0,
                rekeyed: false,
                flushing: None,
            }
            .run(playback, stop);
        });
        Ok(())
    }
}

struct Player<'r> {
    relay: &'r Relay,
    cur: Option<Cursor>,
    /// Since when nobody is in.
    idle: Option<Instant>,
    /// What the clients were last told.
    paused: bool,
    /// When the trace last said where playback is.
    traced: Option<Instant>,
    /// The moment wanted last time round, to tell a seek from playback going on.
    last_target: Option<i64>,
    /// A jump ahead ran out of what is written: no other until the next seek (playback ahead of a
    /// growing demo plays its frames as they come, instead of a full update for each).
    dry: bool,
    /// Why the clients get nothing, if they do not.
    problem: Option<String>,
    /// Checkpoints through the demo playing, made on the side.
    index: Option<Arc<Index>>,
    /// The camera the clients were last told to take, and when.
    cued: Option<(Camera, Instant)>,
    /// The kept beams shown: when each last was, and what may be sent now.
    kept: Kept,
    /// The notes the clients were given.
    shown_notes: notes::Shown,
    /// Where the clients were given the laser beams are drawn with, if they were.
    laser: Option<u16>,
    /// The clients' clock (seconds) at an instant.
    clock: Option<(f64, Instant)>,
    /// Their clock when their last beam runs out.
    beams_until: f64,
    /// Every client waits for a full update.
    rekeyed: bool,
    /// The full update waits until then, their clock put ahead.
    flushing: Option<Instant>,
}

impl Player<'_> {
    fn run(mut self, playback: &Playback, stop: &AtomicBool) {
        while !stop.load(Ordering::Relaxed) {
            if !self.anyone_in() {
                std::thread::sleep(IDLE_NAP);
                continue;
            }
            let want = playback.get();
            if self.cur.as_ref().is_none_or(|c| c.path != want.demo) {
                // Another demo (a live match's next one): clients in a game sign on again.
                if self.cur.take().is_some() {
                    self.relay.hub().reconnect_all();
                }
                match Cursor::open(&want.demo, &self.relay.hub) {
                    Ok(c) => {
                        self.cur = Some(c);
                        // Its own tables: the clients sign on to them, the laser not among them.
                        self.laser = None;
                        self.shown_notes = notes::Shown::default();
                        self.clock = None;
                        self.beams_until = 0.0;
                        playback.set_problem(None);
                        if self.index.as_ref().is_none_or(|i| i.path != want.demo) {
                            self.index = Some(Index::shared(&want.demo));
                        }
                    }
                    Err(e) => {
                        // Not written yet, or not playable: try again in a while.
                        trace!("cannot play {}: {e:#}", want.demo.display());
                        playback.set_problem(Some(format!("{e:#}")));
                        std::thread::sleep(RETRY);
                        continue;
                    }
                }
            }
            if self.flushing.is_none() {
                self.draw(&playback.take_beams());
                self.show_kept(playback);
                let (version, notes) = lock(&playback.notes).clone();
                self.show_notes(version, &notes);
            }
            match self.step(&want) {
                Ok(()) => playback.set_problem(self.problem.clone()),
                Err(e) => {
                    // The file went or is corrupt: tell why, and open it again in a while.
                    trace!("playing {}: {e:#}", want.demo.display());
                    playback.set_problem(Some(format!("{e:#}")));
                    self.cur = None;
                    self.relay.hub().reconnect_all();
                    std::thread::sleep(RETRY);
                }
            }
        }
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
            self.index = None;
            hub.forget();
        }
        false
    }

    /// Back to `target`: the demo again from the last checkpoint before it (or its start), decoded up
    /// to there, and a full update of it: the clients stay in, and go back. If an entry of the
    /// string tables they have changed value since (a player left or joined), those entries are
    /// set back first.
    fn jump_back(&mut self, want: &Want, target: i64) -> Result<()> {
        let Some(c) = &self.cur else { return Ok(()) };
        trace!("jump back from {} to {target}", c.tick());
        let rewritten = c.dec.tables_rewritten();
        let took = Instant::now();
        // From the last checkpoint before it, or else the start.
        let mark = self.index.as_ref().and_then(|i| i.before(target));
        let mut again = match &mark {
            Some(m) => Cursor::restore(&want.demo, m)?,
            None => Cursor::open_with(&want.demo, None)?,
        };
        self.dry = skim(&mut again, &self.relay.hub, target).1;
        trace!("decoded back to {target} in {} ms", took.elapsed().as_millis());
        if again.dec.tables_rewritten() < rewritten {
            // The clients' tables are set back to these, or failing that, they sign on again.
            // The stream's changes after here are sent again when playback comes to them.
            let later = owned(c.tables.get(again.tables.len()..).unwrap_or_default());
            if let Some(msgs) = again.dec.set_tables_back(&c.dec) {
                trace!("string tables set back with {} messages", msgs.len());
                self.relay.hub().set_tables_back(&later, &msgs);
            } else {
                trace!("the string tables changed since: the clients sign on again");
                self.relay.hub().reconnect_all();
                drop(Cursor::open(&want.demo, &self.relay.hub)?);
                self.relay.hub().log_tables(&owned(&again.tables));
            }
        }
        self.cur = Some(again);
        self.rekey();
        Ok(())
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
        // A seek, not playback going on: a jump may find more written than the last.
        if self.last_target.is_none_or(|t| target.abs_diff(t) > jump.unsigned_abs()) {
            self.dry = false;
        }
        self.last_target = Some(target);
        if *super::TRACE && self.traced.is_none_or(|t| t.elapsed() >= super::TRACE_EVERY) {
            self.traced = Some(Instant::now());
            trace!("playing tick {}, wanted {target}, paused {}, speed {}", c.tick(), want.paused, want.speed);
        }
        if c.started && target + jump < c.tick() {
            return self.jump_back(want, target);
        }
        if want.paused != self.paused {
            trace!("paused: {}", want.paused);
            self.paused = want.paused;
            let demo_paused = self.cur.as_ref().is_some_and(|c| c.paused);
            let mut hub = self.relay.hub();
            hub.set_paused(demo_paused);
            // A full update with the round timers held, or as they are going on: one that needs no
            // frame a client may lack (one just in has only its own full update).
            hub.all_want_keyframe(Instant::now());
            drop(hub);
            self.rekeyed = true;
        }
        let Some(c) = &mut self.cur else { return Ok(()) };
        if c.started && c.tick() + jump < target && !self.dry && c.next_tick().is_some_and(|n| n <= target) {
            let (from, took) = (c.tick(), Instant::now());
            // From a checkpoint well past where it is, if there is one: the clients get the string
            // table changes passed over with it.
            let mark = self.index.as_ref().and_then(|i| i.before(target)).filter(|m| i64::from(m.tick) > from + jump);
            if let Some(m) = &mark {
                self.relay.hub().log_tables(&m.tables());
                *c = Cursor::restore(&want.demo, m)?;
            }
            let (decoded, dry) = skim(c, &self.relay.hub, target);
            let decoded = decoded + usize::from(mark.is_some());
            trace!("jump ahead from {from} to {} in {} ms", c.tick(), took.elapsed().as_millis());
            self.dry = dry;
            if decoded > 0 {
                self.relay.hub().all_want_keyframe(Instant::now());
                self.rekeyed = true;
            }
        }
        if self.relay.hub().wants_keyframe() {
            let at = c.tick();
            if self.flush(at, interval) {
                return Ok(());
            }
            let Some(c) = &mut self.cur else { return Ok(()) };
            // Of the frame the next one builds on, which from a live server is often a few ticks
            // back: at the end of what is written so far, it waits for that frame.
            c.peek();
            let next = c.next_frame();
            if next.is_some() || c.ended {
                let key = if self.paused { c.dec.keyframe_held(next) } else { c.dec.keyframe(next) };
                if let Some((tick, key)) = key {
                    let mut since = c.dec.catch_up(tick, c.recent.make_contiguous());
                    // The round's end shown, or hidden, as at this moment.
                    since.extend(c.dec.round_event());
                    // Its clock may have gone back: panels waiting for a time it passed redraw.
                    since.push(super::reset_hud());
                    self.relay.hub().send_keyframe(&key, &since, c.paused);
                    if std::mem::take(&mut self.rekeyed) {
                        self.clock = Some((f64::from(tick) * f64::from(interval), Instant::now()));
                    }
                    self.problem = None;
                    // Those just in are told where the camera is.
                    self.cued = None;
                } else if c.started {
                    self.problem = Some("this demo cannot be followed from a full update here".into());
                }
            }
        }
        self.cue(want.camera);
        let Some(c) = &mut self.cur else { return Ok(()) };
        match c.next_tick() {
            // Not a frame (a user command of a POV demo, say): passed over, paused or not.
            Some(NOT_A_FRAME) => c.next = None,
            // The first frame, in full, is only decoded: every client starts from a keyframe of
            // wherever playback is.
            Some(_) if !c.started => {
                if let Some((bits, life)) = c.decode() {
                    self.relay.hub().skim(&bits, &life);
                }
            }
            Some(next) if !want.paused && next <= target => {
                let was_paused = c.paused;
                if let Some((bits, life)) = c.decode() {
                    // Ours, not the demo's director's.
                    let (bits, life) =
                        if want.camera.is_some() { without_cues(&c.dec, &bits, &life) } else { (bits, life) };
                    let mut hub = self.relay.hub();
                    hub.upstream_stream(&bits, false, &life);
                    hub.upstream_flush();
                    self.clock = Some((c.tick() as f64 * f64::from(interval), Instant::now()));
                    // The match paused or went on: the clients are told reliably too.
                    if c.paused != was_paused {
                        hub.set_paused(c.paused);
                        // Going on after it, the HUD redraws its timer at once (as casters'
                        // `hud_reloadscheme` has it do).
                        if !c.paused {
                            hub.tell_active(&super::reset_hud());
                        }
                    }
                }
            }
            _ => std::thread::sleep(NAP),
        }
        Ok(())
    }
}

impl Player<'_> {
    /// Every client gets a full update.
    fn rekey(&mut self) {
        self.relay.hub().all_want_keyframe(Instant::now());
        self.rekeyed = true;
    }

    /// Puts the clients' clock past their beams before a full update at `key` sets it back.
    /// Whether the full update is to wait.
    fn flush(&mut self, key: i64, interval: f32) -> bool {
        if let Some(until) = self.flushing {
            if Instant::now() < until {
                std::thread::sleep(NAP);
                return true;
            }
            self.flushing = None;
            return false;
        }
        let Some((at, since)) = self.clock.filter(|_| self.rekeyed) else { return false };
        let now = at + since.elapsed().as_secs_f64();
        let Some(ahead) = past_beams(now, key as f64 * f64::from(interval), self.beams_until, interval) else {
            return false;
        };
        trace!("the clients' clock put ahead to {ahead}, past the beams they see");
        self.relay.hub().tell_waiting_now(&messages::tick(ahead));
        self.flushing = Some(Instant::now() + FLUSH);
        self.beams_until = 0.0;
        self.kept.sent.clear();
        true
    }

    /// Beams shown now last `life` seconds.
    fn shown_for(&mut self, life: f32) {
        if let Some((at, since)) = self.clock {
            self.beams_until = self.beams_until.max(at + since.elapsed().as_secs_f64() + f64::from(life));
        }
    }

    /// `beams` as messages, drawn with the laser: the clients are given it first if they have not
    /// been (and those to come too), where they will have it.
    fn beam_messages(&mut self, beams: &[Beam]) -> Option<Vec<BitBuf>> {
        let c = self.cur.as_ref().filter(|c| c.started)?;
        let (model, load) = c.dec.beam_model(self.laser)?;
        if let Some(msg) = load {
            self.relay.hub().log_once(&msg);
            self.laser = Some(model);
        }
        c.dec.beams(beams, model)
    }

    /// Shows each kept beam again as it runs out, for [`KEPT_EVERY`], new ones at once: at most
    /// [`KEPT_RATE`] a second, in bursts of [`KEPT_BURST`], so they never crowd out the frames
    /// (a client that misses one cannot follow the next).
    fn show_kept(&mut self, playback: &Playback) {
        let (version, beams) = playback.kept();
        if self.cur.as_ref().is_none_or(|c| !c.started) {
            return;
        }
        let now = Instant::now();
        let k = &mut self.kept;
        if version != k.version {
            let keys: HashSet<BeamKey> = beams.iter().map(beam_key).collect();
            k.sent.retain(|key, _| keys.contains(key));
            k.version = version;
        }
        k.tokens = (k.tokens + now.duration_since(k.refilled).as_secs_f64() * KEPT_RATE).min(KEPT_BURST);
        k.refilled = now;
        // Those about to run out first, then those never shown.
        let mut due: Vec<(bool, &Beam)> = beams
            .iter()
            .filter_map(|b| match k.sent.get(&beam_key(b)) {
                Some(at) if now.duration_since(*at) < KEPT_EVERY => None,
                at => Some((at.is_none(), b)),
            })
            .collect();
        due.sort_by_key(|(new, _)| *new);
        let n = due.len().min(k.tokens as usize);
        if n == 0 {
            return;
        }
        k.tokens -= n as f64;
        let life = KEPT_EVERY.as_secs_f32();
        let show: Vec<Beam> = due[..n].iter().map(|(_, b)| Beam { life, ..**b }).collect();
        for b in &show {
            k.sent.insert(beam_key(b), now);
        }
        if let Some(msgs) = self.beam_messages(&show) {
            let mut hub = self.relay.hub();
            for m in &msgs {
                hub.tell_active_now(m);
            }
            drop(hub);
            self.shown_for(life);
        }
    }

    /// Shows `beams` to the clients in the game.
    fn draw(&mut self, beams: &[Beam]) {
        if beams.is_empty() {
            return;
        }
        match self.beam_messages(beams) {
            Some(msgs) => {
                trace!("{} beams to the clients in", beams.len());
                let mut hub = self.relay.hub();
                for m in &msgs {
                    hub.tell_active(m);
                }
                drop(hub);
                self.shown_for(beams.iter().map(|b| b.life).fold(0.0, f32::max));
            }
            None => trace!("{} beams not drawn: no beam class or sprite in the demo", beams.len()),
        }
    }

    /// Tells the clients in the game to take `camera`, when it changed and every [`CUE_EVERY`].
    fn cue(&mut self, camera: Option<Camera>) {
        let Some(camera) = camera else {
            self.cued = None;
            return;
        };
        if self.cued.is_some_and(|(was, at)| was == camera && at.elapsed() < CUE_EVERY) {
            return;
        }
        let Some(c) = self.cur.as_ref().filter(|c| c.started) else { return };
        let Camera::Follow { player } = camera;
        if let Some(msg) = c.dec.hltv_chase(player) {
            trace!("camera on player {player}");
            self.relay.hub().tell_active(&msg);
        }
        self.cued = Some((camera, Instant::now()));
    }
}

/// A frame's stream without the director's camera cues.
fn without_cues(dec: &Decoder, bits: &BitBuf, life: &Lifecycle) -> (BitBuf, Lifecycle) {
    if !life.spans.iter().any(|s| dec.is_director_cue(bits.bytes(), s)) {
        return (bits.clone(), life.clone());
    }
    let mut w = BitWriter::new();
    let mut spans = Vec::with_capacity(life.spans.len());
    for span in life.spans.iter().filter(|s| !dec.is_director_cue(bits.bytes(), s)) {
        let start = w.nbits();
        w.write_range(bits.bytes(), span.bits.clone());
        spans.push(Span { id: span.id, bits: start..w.nbits() });
    }
    (w.finish(), Lifecycle { spans, ..life.clone() })
}

/// Decodes on to `target` without sending: only string table changes reach the clients. How many
/// frames it decoded, and whether it stopped short for want of more written.
fn skim(c: &mut Cursor, hub: &Mutex<Hub>, target: i64) -> (usize, bool) {
    let mut decoded = 0;
    loop {
        match c.next_tick() {
            None => return (decoded, !c.ended),
            Some(next) if next > target => return (decoded, false),
            Some(_) => match c.decode() {
                Some((bits, life)) => {
                    lock(hub).skim(&bits, &life);
                    decoded += 1;
                }
                None => c.next = None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::messages::{SVC_GAMEEVENT, SVC_TEMPENTITIES};
    use tf_demo_parser::demo::gameevent_gen::GameEvent;
    use tf_demo_parser::demo::message::Message;

    /// `pl_upward.dem` decoded until `done`.
    pub(super) fn upward_until(done: impl Fn(&Cursor) -> bool) -> Cursor {
        let path = crate::fixture("pl_upward.dem");
        let mut c = Cursor::open_with(&path, None).unwrap();
        while !done(&c) {
            match c.next_tick() {
                None => panic!("the demo ended"),
                Some(NOT_A_FRAME) => c.next = None,
                Some(_) => drop(c.decode()),
            }
        }
        c
    }

    #[test]
    fn a_full_update_holds_the_round_timer_and_the_next_runs_it_again() {
        // Twenty seconds in, with the round's timer on the HUD: the relay's, and a client's.
        let twenty = || {
            upward_until(|c| {
                c.started && c.dec.server_tick() >= c.first.unwrap_or(i64::MAX).saturating_add(20 * 66) as u32
            })
        };
        let (mut relay, mut client) = (twenty(), twenty());
        let running = client.dec.extractor().snapshot().timers;
        assert!(!running.is_empty() && running.iter().all(|t| !t.paused), "{running:?}");
        relay.peek();
        let next = relay.next_frame();
        let (_, held) = relay.dec.keyframe_held(next).expect("a full update");
        client.dec.feed(held.bytes(), held.nbits());
        let timers = client.dec.extractor().snapshot().timers;
        assert_eq!(timers.len(), running.len());
        for (h, r) in timers.iter().zip(&running) {
            assert!(h.paused, "{h:?}");
            assert!((h.remaining - r.remaining).abs() < 0.5, "held at {} where it was {}", h.remaining, r.remaining);
        }
        // Going on: a full update as it is runs them again, though running is their baseline.
        let (_, plain) = relay.dec.keyframe(next).expect("a full update");
        client.dec.feed(plain.bytes(), plain.nbits());
        let timers = client.dec.extractor().snapshot().timers;
        assert!(timers.iter().all(|t| !t.paused), "{timers:?}");
    }

    #[test]
    fn the_clock_is_put_past_beams_it_would_go_back_under() {
        let interval = 0.5;
        // Paused a minute, a line drawn 59 s in, playback going on at 100 s: gone by 162 s.
        assert_eq!(past_beams(160.0, 100.0, 161.0, interval), Some(324));
        assert_eq!(past_beams(160.0, 100.0, 159.0, interval), None, "nothing showing");
        assert_eq!(past_beams(100.2, 100.0, 102.0, interval), None, "a client just in, playback going");
        assert_eq!(past_beams(100.0, 400.0, 102.0, interval), None, "ahead: they run out at once");
    }

    #[test]
    fn beams_read_back_as_the_games_own_drawn_with_a_sprite_of_the_match() {
        use crate::decode::beams::{BASE, POINTS};
        use tf_demo_parser::demo::sendprop::{SendPropIdentifier, SendPropValue};
        use tf_demo_parser::demo::vector::Vector;
        let c = upward_until(|c| c.started);
        let beam = Beam {
            from: [1.0, 2.0, 3.0],
            to: [100.0, -200.0, 300.0],
            rgb: [255, 128, 0],
            life: 4.0,
            width: 2.0,
            speed: 20,
        };
        let (model, load) = c.dec.beam_model(None).expect("a model to draw beams with");
        let msgs = c.dec.beams(&[beam; 40], model).expect("a beam class");
        assert_eq!(msgs.len(), 3, "16 a message");
        assert!(msgs.iter().all(|m| m.nbits() / 8 < 1000), "each fits a datagram");
        let state = c.dec.parser_state_for_tests();
        let mut events = Vec::new();
        for m in &msgs {
            let mut s = tf_demo_parser::Stream::from(bitbuffer::BitReadBuffer::new_owned(
                m.bytes().to_vec(),
                bitbuffer::LittleEndian,
            ));
            assert_eq!(s.read_int::<u32>(TYPE_BITS).unwrap(), SVC_TEMPENTITIES);
            let Message::TempEntities(te) =
                Message::from_type(tf_demo_parser::demo::message::MessageType::TempEntities, &mut s, state).unwrap()
            else {
                panic!()
            };
            events.extend(te.events);
        }
        assert_eq!(events.len(), 40);
        let get = |t: &str, n: &str| {
            events[39]
                .props
                .iter()
                .find(|p| p.identifier == SendPropIdentifier::new(t, n))
                .map(|p| p.value.clone())
                .unwrap()
        };
        assert_eq!(get(POINTS, "m_vecEndPoint"), SendPropValue::Vector(Vector { x: 100.0, y: -200.0, z: 300.0 }));
        assert_eq!(get(BASE, "g"), SendPropValue::Integer(128));
        assert_eq!(get(BASE, "m_nSpeed"), SendPropValue::Integer(20));
        assert_eq!(get(BASE, "m_nModelIndex"), SendPropValue::Integer(i64::from(model)));
        // The match did not load the laser: it goes after the last of the models, as a game client
        // adds it whatever index it is sent with.
        let load = load.expect("the laser loaded by the relay");
        let mut s = tf_demo_parser::Stream::from(bitbuffer::BitReadBuffer::new_owned(
            load.bytes().to_vec(),
            bitbuffer::LittleEndian,
        ));
        assert_eq!(s.read_int::<u32>(TYPE_BITS).unwrap(), messages::SVC_UPDATESTRINGTABLE);
        let msg =
            Message::from_type(tf_demo_parser::demo::message::MessageType::UpdateStringTable, &mut s, state).unwrap();
        let Message::UpdateStringTable(u) = msg else { panic!() };
        let table = &state.string_tables[usize::from(u.table_id)];
        assert_eq!(u.entries.len(), 1);
        assert_eq!(u.entries[0].1.text.as_deref(), Some("sprites/laserbeam.vmt"));
        assert_eq!(u.entries[0].0, model);
        assert!(model < table.max_entries);
        assert_eq!(c.dec.beam_model(Some(model)).unwrap().0, model, "where it went, while the match leaves it be");
        let SendPropValue::Float(life) = get(BASE, "m_fLife") else { panic!() };
        assert!((life - 4.0).abs() < 0.2, "{life}");
    }

    #[test]
    fn a_camera_cue_names_the_players_entity_and_the_demos_own_go() {
        let path = crate::fixture("pl_upward.dem");
        let mut c = Cursor::open_with(&path, None).unwrap();
        let mut cues = 0;
        // Ten seconds in, with the players there.
        while c.dec.server_tick() < c.first.unwrap_or(i64::MAX).saturating_add(660) as u32 || c.first.is_none() {
            match c.next_tick() {
                None => panic!("the demo ended"),
                Some(NOT_A_FRAME) => c.next = None,
                Some(_) => {
                    let Some((bits, life)) = c.decode() else { continue };
                    cues += life.spans.iter().filter(|s| c.dec.is_director_cue(bits.bytes(), s)).count();
                    let (bits, life) = without_cues(&c.dec, &bits, &life);
                    assert!(!life.spans.iter().any(|s| c.dec.is_director_cue(bits.bytes(), s)));
                }
            }
        }
        eprintln!("{cues} director cues in the first 10 s");
        let player = c.dec.extractor().snapshot().players.iter().find(|p| p.alive).cloned().unwrap();
        let msg = c.dec.hltv_chase(player.id.0).unwrap();
        let span = Span { id: u8::try_from(SVC_GAMEEVENT).unwrap(), bits: 0..msg.nbits() };
        assert!(c.dec.is_director_cue(msg.bytes(), &span));
        let state = c.dec.parser_state_for_tests();
        let mut s = bitbuffer::BitReadStream::new(bitbuffer::BitReadBuffer::new(msg.bytes(), bitbuffer::LittleEndian));
        let t: tf_demo_parser::demo::message::MessageType = bitbuffer::BitRead::read(&mut s).unwrap();
        let Message::GameEvent(e) = Message::from_type(t, &mut s, state).unwrap() else { panic!() };
        let GameEvent::HLTVChase(chase) = e.event else { panic!("{:?}", e.event) };
        assert_eq!(u32::from(chase.target_1), player.ent);
        assert_eq!(chase.in_eye, 1);
    }
}
