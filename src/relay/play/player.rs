//! The player: playing demos to the clients where playback wants them, jumping when off.

use super::beams::Kept;
use super::cursor::{Cursor, owned};
use super::index::Index;
use super::{CUE_EVERY, Camera, IDLE, IDLE_NAP, JUMP, NAP, NOT_A_FRAME, Playback, RETRY, Want, light, notes, voice};
use crate::decode::ModelsSent;
use crate::lock;
use crate::relay::{Hub, Relay};
use anyhow::Result;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub(super) struct Player<'r> {
    pub(super) relay: &'r Relay,
    pub(super) cur: Option<Cursor>,
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
    /// The demo that could not be opened, and why, as last logged: said once, not every try.
    refused: Option<(std::path::PathBuf, String)>,
    /// Checkpoints through the demo playing, made on the side.
    pub(super) index: Option<Arc<Index>>,
    /// The camera the clients were last told to take, and when.
    cued: Option<(Camera, Instant)>,
    /// The kept beams shown: when each last was, and what may be sent now.
    pub(super) kept: Kept,
    /// The notes the clients were given.
    pub(super) shown_notes: notes::Shown,
    /// Where each speaker's voice sent is.
    pub(super) voice_seq: voice::Sequences,
    /// The light the clients were given.
    pub(super) shown_light: light::Shown,
    /// Where the clients were given the laser beams are drawn with, if they were: theirs there
    /// until the next signon.
    pub(super) laser: Option<u16>,
    /// What the clients' `modelprecache` has had since their signon.
    pub(super) models: ModelsSent,
    /// A demo whose match loads its own laser after the start: the relay's is not loaded for it.
    pub(super) match_laser: Option<std::path::PathBuf>,
    /// The clients' clock (seconds) at an instant.
    pub(super) clock: Option<(f64, Instant)>,
    /// Their clock when their last beam runs out.
    pub(super) beams_until: f64,
    /// Every client waits for a full update.
    pub(super) rekeyed: bool,
    /// The oldest frame the clients in the game may still have: a game client drops those older
    /// than the base of each delta, and all on a full update.
    pub(super) floor: Option<u32>,
    /// The full update waits until then, their clock put ahead.
    pub(super) flushing: Option<Instant>,
    /// The demo whose signon the clients have: the next makes them reconnect first.
    signed_on: Option<std::path::PathBuf>,
}

impl<'r> Player<'r> {
    pub(super) fn new(relay: &'r Relay) -> Self {
        Player {
            relay,
            cur: None,
            idle: None,
            paused: false,
            traced: None,
            last_target: None,
            dry: false,
            problem: None,
            refused: None,
            index: None,
            cued: None,
            kept: Kept::new(Instant::now()),
            shown_notes: notes::Shown::default(),
            voice_seq: voice::Sequences::default(),
            shown_light: light::Shown::default(),
            laser: None,
            models: ModelsSent::default(),
            match_laser: None,
            clock: None,
            beams_until: 0.0,
            rekeyed: false,
            floor: None,
            flushing: None,
            signed_on: None,
        }
    }
}

impl Player<'_> {
    pub(super) fn run(mut self, playback: &Playback, stop: &AtomicBool) {
        while !stop.load(Ordering::Relaxed) {
            if !self.anyone_in() {
                std::thread::sleep(IDLE_NAP);
                continue;
            }
            let want = playback.want();
            if self.open_wanted(&want, playback).is_break() {
                continue;
            }
            if self.flushing.is_none() {
                self.show_extras(playback);
            }
            match self.step(&want) {
                Ok(()) => playback.set_problem(self.problem.clone()),
                Err(e) => {
                    // The file went or is corrupt: tell why, and open it again in a while.
                    log!("playing {}: {e:#}", want.demo.display());
                    playback.set_problem(Some(format!("{e:#}")));
                    self.cur = None;
                    std::thread::sleep(RETRY);
                }
            }
        }
    }

    /// Opens the demo `want` names unless it is playing, once no signon is held: break while it is,
    /// or the demo cannot be opened.
    fn open_wanted(&mut self, want: &Want, playback: &Playback) -> ControlFlow<()> {
        if self.cur.as_ref().is_some_and(|c| c.path == want.demo) {
            self.relay.hub().release_signon_hold();
            return ControlFlow::Continue(());
        }
        // Not while a client is in a signon: a second crashes it. Nothing plays meanwhile.
        if self.relay.hub().hold_for_signon(Instant::now()) {
            std::thread::sleep(NAP);
            return ControlFlow::Break(());
        }
        // Another demo (a live match's next one), or this again: clients in a game sign on again.
        self.cur = None;
        if let Some(was) = self.signed_on.take() {
            trace!("the clients leave {} for {}", was.display(), want.demo.display());
            self.relay.hub().reconnect_all();
        }
        match Cursor::open(&want.demo, &self.relay.hub) {
            Ok(c) => {
                self.start_demo(c);
                self.refused = None;
                playback.set_problem(None);
                if self.index.as_ref().is_none_or(|i| i.path != want.demo) {
                    self.index = Some(Index::shared(&want.demo));
                }
                ControlFlow::Continue(())
            }
            Err(e) => {
                // Not written yet, or not playable: try again in a while.
                let why = format!("{e:#}");
                if self.refuses_anew(&want.demo, &why) {
                    log!("cannot play {}: {why}", want.demo.display());
                }
                playback.set_problem(Some(why));
                std::thread::sleep(RETRY);
                ControlFlow::Break(())
            }
        }
    }

    /// Whether `demo` not opening for `why` is news: it is tried again every [`RETRY`].
    pub(super) fn refuses_anew(&mut self, demo: &std::path::Path, why: &str) -> bool {
        if self.refused.as_ref().is_some_and(|(d, w)| d == demo && w == why) {
            return false;
        }
        self.refused = Some((demo.to_owned(), why.to_owned()));
        true
    }

    /// What the room shows besides the demo: kept beams, notes, voice and light.
    fn show_extras(&mut self, playback: &Playback) {
        self.show_kept(playback);
        self.show_notes(&playback.notes());
        self.send_voice(&playback.take_voice());
        self.show_light(playback.light());
    }

    /// Plays `c`, just opened: the clients sign on to its tables, the laser not among them.
    pub(super) fn start_demo(&mut self, c: Cursor) {
        if self.match_laser.as_ref().is_some_and(|p| *p != c.path) {
            self.match_laser = None;
        }
        self.signed_on = Some(c.path.clone());
        self.cur = Some(c);
        self.laser = None;
        self.models = ModelsSent::default();
        self.floor = None;
        self.shown_notes = notes::Shown::default();
        self.shown_light = light::Shown::default();
        self.clock = None;
        self.beams_until = 0.0;
    }

    /// Whether a client is in; after a while with none, the demo is dropped.
    fn anyone_in(&mut self) -> bool {
        let mut hub = self.relay.hub();
        if !hub.clients.is_empty() {
            self.idle = None;
            return true;
        }
        hub.release_signon_hold();
        let since = *self.idle.get_or_insert_with(Instant::now);
        if since.elapsed() > IDLE && self.cur.is_some() {
            self.cur = None;
            self.index = None;
            self.signed_on = None;
            hub.start_over();
        }
        false
    }

    /// Back to `target`: the demo again from the last checkpoint before it (or its start), decoded
    /// up to there, and a full update of it: the clients stay in, and go back. If an entry of the
    /// string tables they have changed value since (a player left or joined), those entries are set
    /// back first.
    fn jump_back(&mut self, want: &Want, target: i64) -> Result<()> {
        let Some(c) = &self.cur else { return Ok(()) };
        log!(":{} jump back from {} to {target}, {} clients", self.relay.port(), c.tick(), self.relay.clients());
        self.relay.hub().reshow_panel_with_next_keyframe();
        // The clients keep the models loaded since.
        self.models = self.models.with(&c.dec);
        let rewritten = c.dec.tables_rewritten();
        let took = Instant::now();
        // From the last checkpoint before it, or else the start.
        let mark = self.index.as_ref().and_then(|i| i.before(target));
        let mut again = match &mark {
            Some(m) => Cursor::restore(&want.demo, m)?,
            None => Cursor::open_detached(&want.demo)?,
        };
        // The clients had the string table changes up to there.
        self.dry = skim(&mut again, None, target).1;
        trace!("decoded back to {target} in {} ms", took.elapsed().as_millis());
        if again.dec.tables_rewritten() < rewritten {
            // The clients' tables are set back to these, or failing that, they sign on again.
            // The stream's changes after here are sent again when playback comes to them.
            let later = owned(c.tables.get(again.tables.len()..).unwrap_or_default());
            if let Some(msgs) = again.dec.set_tables_back(&c.dec) {
                trace!("string tables set back with {} messages", msgs.len());
                self.relay.hub().set_tables_back(&later, &msgs);
            } else {
                log!(":{} the string tables changed since: the clients sign on again", self.relay.port());
                self.cur = None;
                self.dry = false;
                return Ok(());
            }
        }
        self.cur = Some(again);
        self.rekey();
        Ok(())
    }

    /// Plays on toward `want`: one frame, a jump, or a short wait.
    pub(super) fn step(&mut self, want: &Want) -> Result<()> {
        if self.restart_if_laser_loaded_twice() {
            return Ok(());
        }
        let Some(c) = &self.cur else { return Ok(()) };
        let interval = c.dec.tick_interval();
        let target = target_tick(c, want, interval);
        let jump = jump_ticks(interval);
        let behind = c.started && target + jump < c.tick();
        self.follow_target(want, target, jump);
        if behind {
            return self.jump_back(want, target);
        }
        self.follow_pause(want);
        self.jump_ahead(want, target, jump)?;
        if self.relay.hub().wants_keyframe() && self.send_keyframe(interval).is_break() {
            return Ok(());
        }
        self.cue(want.camera);
        self.advance(want, target);
        Ok(())
    }

    /// Takes `target` as the moment wanted: one far from the last is a seek, which may find more
    /// written than the last jump did.
    fn follow_target(&mut self, want: &Want, target: i64, jump: i64) {
        if self.last_target.is_none_or(|t| target.abs_diff(t) > jump.unsigned_abs()) {
            self.dry = false;
        }
        self.last_target = Some(target);
        if *crate::relay::TRACE && self.traced.is_none_or(|t| t.elapsed() >= crate::relay::TRACE_EVERY) {
            self.traced = Some(Instant::now());
            let tick = self.cur.as_ref().map_or(0, Cursor::tick);
            trace!("playing tick {tick}, wanted {target}, paused {}, speed {}", want.paused, want.speed);
        }
    }

    /// The room paused or went on: the clients are told the match's own pause, and get a keyframe.
    fn follow_pause(&mut self, want: &Want) {
        if want.paused == self.paused {
            return;
        }
        let what = if want.paused { "paused" } else { "plays on" };
        log!(
            ":{} {what} at {}, {} clients",
            self.relay.port(),
            self.cur.as_ref().map_or(0, Cursor::tick),
            self.relay.clients()
        );
        self.paused = want.paused;
        let demo_paused = self.cur.as_ref().is_some_and(|c| c.paused);
        self.relay.hub().queue_pause(demo_paused);
        // A full update with the round timers held, or as they are going on: one that needs no
        // frame a client may lack (one just in has only its own full update).
        self.rekey();
    }

    /// Decodes on to `target` without sending when playback is more than `jump` behind it and the
    /// demo has it written, from a checkpoint if one is well past: every client gets a keyframe.
    fn jump_ahead(&mut self, want: &Want, target: i64, jump: i64) -> Result<()> {
        let Some(c) = &mut self.cur else { return Ok(()) };
        if !(c.started && c.tick() + jump < target && !self.dry && c.next_tick().is_some_and(|n| n <= target)) {
            return Ok(());
        }
        let (from, took) = (c.tick(), Instant::now());
        // From a checkpoint well past where it is, if there is one: the clients get the string
        // table changes passed over with it, those after the ones they had.
        let mark = self.index.as_ref().and_then(|i| i.before(target)).filter(|m| i64::from(m.tick) > from + jump);
        if let Some(m) = &mark {
            let passed = m.tables();
            self.relay.hub().add_table_changes(passed.get(c.tables.len()..).unwrap_or_default());
            *c = Cursor::restore(&want.demo, m)?;
        }
        let (decoded, dry) = skim(c, Some(&self.relay.hub), target);
        let decoded = decoded + usize::from(mark.is_some());
        log!(
            ":{} jump ahead from {from} to {} in {} ms, {} clients",
            self.relay.port(),
            c.tick(),
            took.elapsed().as_millis(),
            self.relay.clients()
        );
        self.dry = dry;
        if decoded > 0 {
            self.rekey();
        }
        Ok(())
    }

    /// A keyframe for the clients waiting for one: of the frame the next one builds on, with the
    /// frames since. Break while their clock is first put past their beams.
    fn send_keyframe(&mut self, interval: f32) -> ControlFlow<()> {
        let Some(at) = self.cur.as_ref().map(Cursor::tick) else { return ControlFlow::Continue(()) };
        if self.flush(at, interval) {
            return ControlFlow::Break(());
        }
        let Some(c) = &mut self.cur else { return ControlFlow::Continue(()) };
        // Of the frame the next one builds on, which from a live server is often a few ticks
        // back: at the end of what is written so far, it waits for that frame.
        c.peek();
        let next = c.next_frame();
        if next.is_none() && !c.ended {
            return ControlFlow::Continue(());
        }
        let key = if self.paused { c.dec.keyframe_held(next) } else { c.dec.keyframe(next) };
        let Some((tick, key)) = key else {
            if c.started {
                self.problem = Some("this demo cannot be followed from a full update here".into());
            }
            return ControlFlow::Continue(());
        };
        let mut since = c.dec.catch_up(tick, c.recent.make_contiguous());
        // The round's end shown, or hidden, as at this moment.
        since.extend(c.dec.round_event());
        // Its clock may have gone back: the timers, waiting for a time it passed, redraw.
        since.extend(c.dec.update_timer());
        let mut hub = self.relay.hub();
        hub.send_keyframe(&key, &since, c.paused);
        hub.reshow_notes(Instant::now());
        drop(hub);
        self.floor = Some(super::follow::floor_after(self.floor, &c.dec, tick, &since));
        if std::mem::take(&mut self.rekeyed) {
            self.clock = Some((f64::from(tick) * f64::from(interval), Instant::now()));
        }
        self.problem = None;
        // Those just in are told where the camera is.
        self.cued = None;
        ControlFlow::Continue(())
    }

    /// Plays the next frame if it is due, passes over a packet that is no frame, or waits a moment.
    fn advance(&mut self, want: &Want, target: i64) {
        let Some(c) = &mut self.cur else { return };
        match c.next_tick() {
            // Not a frame (a user command of a POV demo, say): passed over, paused or not.
            Some(NOT_A_FRAME) => c.skip_next(),
            // The first frame, in full, is only decoded: every client starts from a keyframe of
            // wherever playback is.
            Some(_) if !c.started => {
                if let Some((bits, life)) = c.decode() {
                    self.relay.hub().skim(&bits, &life);
                }
            }
            Some(next) if !want.paused && next <= target => self.play_frame(want.camera),
            _ => std::thread::sleep(NAP),
        }
    }
}

/// The server tick `want` wants now, not before the demo starts.
fn target_tick(c: &Cursor, want: &Want, interval: f32) -> i64 {
    let first = c.first.unwrap_or(i64::MIN);
    let wanted = i64::from(want.tick_now(interval));
    if want.from_start { first.saturating_add(wanted) } else { wanted.max(first) }
}

/// The ticks in [`JUMP`]: off by more, playback jumps.
fn jump_ticks(interval: f32) -> i64 {
    (JUMP.as_secs_f32() / interval.max(1e-3)) as i64
}

impl Player<'_> {
    /// Every client gets a full update.
    pub(super) fn rekey(&mut self) {
        self.relay.hub().rekey_all(Instant::now());
        self.rekeyed = true;
        self.floor = None;
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
            self.relay.hub().queue_to_active(&msg);
        }
        self.cued = Some((camera, Instant::now()));
    }
}

/// Decodes on to `target` without sending: only string table changes reach the clients, through
/// `hub` if given. How many frames it decoded, and whether it stopped short for want of more
/// written.
fn skim(c: &mut Cursor, hub: Option<&Mutex<Hub>>, target: i64) -> (usize, bool) {
    let mut decoded = 0;
    loop {
        match c.next_tick() {
            None => return (decoded, !c.ended),
            Some(next) if next > target => return (decoded, false),
            Some(_) => match c.decode() {
                Some((bits, life)) => {
                    if let Some(hub) = hub {
                        lock(hub).skim(&bits, &life);
                    }
                    decoded += 1;
                }
                None => c.skip_next(),
            },
        }
    }
}
