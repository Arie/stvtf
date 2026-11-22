//! The player: playing demos to the clients where playback wants them, jumping when off.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use super::beams::Kept;
use super::cursor::Cursor;
use super::index::Index;
use super::tuning::{FRAME_WAIT, HAND_OVER_WAIT, IDLE, IDLE_NAP, NAP, NOT_A_FRAME, RETRY};
use super::{Playback, Want, light, names, notes, shot, voice};
use crate::Result;
use crate::decode::ModelsSent;
use crate::relay::handover::{Played, TakenOver};
use crate::relay::{Handover, Relay};
use crate::sync::lock;

mod demos;
mod jumps;

use self::jumps::jump_ticks;

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
    /// Clients were taken over from the process before, and where their playback was.
    taken_over: Option<TakenOver>,
    /// A demo that will never open (another TF2 build's, say): not tried again.
    pub(super) given_up: Option<std::path::PathBuf>,
    /// Checkpoints through the demo playing, made on the side.
    pub(super) index: Option<Arc<Index>>,
    /// Those of the demos played before it, the last few: going back into one starts from them.
    pub(super) past: Vec<Arc<Index>>,
    /// The clients' camera, as the room has it.
    pub(super) shot: shot::Shot,
    /// The kept beams shown: when each last was, and what may be sent now.
    pub(super) kept: Kept,
    /// The notes the clients were given.
    pub(super) shown_notes: notes::Shown,
    pub(super) shown_names: names::Shown,
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
    /// Since when a full update waits for the frame after it.
    frame_waited: Option<Instant>,
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
            taken_over: lock(&relay.taken_over).take(),
            given_up: None,
            index: None,
            past: Vec::new(),
            shot: shot::Shot::default(),
            kept: Kept::new(Instant::now()),
            shown_notes: notes::Shown::default(),
            shown_names: names::Shown::default(),
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
            frame_waited: None,
        }
    }
}

impl Player<'_> {
    pub(super) fn run(mut self, playback: &Playback, stop: &AtomicBool) {
        while !stop.load(Ordering::Relaxed) {
            if playback.hand_over_asked() {
                playback.give(self.hand_over());
                return;
            }
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
                    tracing::info!("relay: playing {}: {e:#}", want.demo.display());
                    playback.set_problem(Some(format!("{e:#}")));
                    self.cur = None;
                    std::thread::sleep(RETRY);
                }
            }
        }
    }

    /// The clients, once those in the game are at rest or [`HAND_OVER_WAIT`] is over, with where
    /// playback is: the relay is silent from then on.
    pub(super) fn hand_over(&mut self) -> Handover {
        let until = Instant::now() + HAND_OVER_WAIT;
        while !self.relay.hub().all_at_rest() && Instant::now() < until {
            std::thread::sleep(NAP);
        }
        let played =
            self.cur.as_ref().filter(|c| c.started && self.signed_on.as_ref() == Some(&c.path)).map(|c| Played {
                demo: c.path.clone(),
                tick: c.tick(),
                laser: self.laser,
                models: self.models.with(&c.dec),
                match_laser: self.match_laser.clone(),
            });
        let h = self.relay.hub().hand_over(played);
        tracing::info!("relay: :{} {} clients handed over", self.relay.port(), h.clients());
        h
    }

    /// What the room shows besides the demo: kept beams, notes, voice and light.
    fn show_extras(&mut self, playback: &Playback) {
        self.show_kept(playback);
        self.show_notes(&playback.notes());
        self.send_voice(&playback.take_voice());
        self.show_light(playback.light());
        let (version, names) = playback.names();
        self.show_names(version, &names);
        // A score counted otherwise: the clients have the one from before until a full update.
        if self.cur.as_mut().is_some_and(|c| c.dec.set_score_add(playback.score())) {
            self.rekey();
        }
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
            self.past.clear();
            self.signed_on = None;
            self.given_up = None;
            hub.start_over();
        }
        false
    }

    /// Plays on toward `want`: one frame, a jump, or a short wait.
    pub(super) fn step(&mut self, want: &Want) -> Result<()> {
        if self.restart_if_laser_loaded_twice() {
            return Ok(());
        }
        self.shoot(want);
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
        self.relay.hub().rekey_unacked(Instant::now());
        if !self.relay.hub().wants_keyframe() {
            self.frame_waited = None;
        } else if self.send_keyframe(interval).is_break() {
            return Ok(());
        }
        self.cue(want.camera.as_ref());
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
        if tracing::enabled!(tracing::Level::DEBUG)
            && self.traced.is_none_or(|t| t.elapsed() >= crate::relay::tuning::TRACE_EVERY)
        {
            self.traced = Some(Instant::now());
            let tick = self.cur.as_ref().map_or(0, Cursor::tick);
            tracing::debug!(
                "relay: playing tick {tick}, wanted {target}, paused {}, speed {}",
                want.paused,
                want.speed
            );
        }
    }

    /// The room paused or went on: the clients are told the match's own pause, and get a keyframe.
    fn follow_pause(&mut self, want: &Want) {
        if want.paused == self.paused {
            return;
        }
        let what = if want.paused { "paused" } else { "plays on" };
        tracing::info!(
            "relay: :{} {what} at {}, {} clients",
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

    /// A keyframe for the clients waiting for one: of the frame the next one builds on, with the
    /// frames since. Break while their clock is first put past their beams.
    pub(super) fn send_keyframe(&mut self, interval: f32) -> ControlFlow<()> {
        let Some(at) = self.cur.as_ref().map(Cursor::tick) else { return ControlFlow::Continue(()) };
        if self.flush(at, interval) {
            return ControlFlow::Break(());
        }
        let Some(c) = &mut self.cur else { return ControlFlow::Continue(()) };
        // Of the frame the next one builds on, which from a live server is often a few ticks
        // back: at the end of what is written so far, it waits for that frame, for a while. A
        // recording that stopped with no end has none, nor a paused match: the last frame then.
        c.peek();
        let next = c.next_frame();
        if next.is_none() && !c.ended && self.frame_waited.get_or_insert_with(Instant::now).elapsed() < FRAME_WAIT {
            return ControlFlow::Continue(());
        }
        self.frame_waited = None;
        let key = if self.paused { c.dec.keyframe_held(next) } else { c.dec.keyframe(next) };
        let Some((tick, key)) = key else {
            if c.started {
                self.problem = Some("this demo cannot be followed from a full update here".into());
            }
            return ControlFlow::Continue(());
        };
        // No frames since: a client acks the full update first, and gets the next frame from it.
        let mut since = Vec::new();
        // The round's end shown, or hidden, as at this moment.
        since.extend(c.dec.round_event());
        // Its clock may have gone back: the timers, waiting for a time it passed, redraw.
        since.extend(c.dec.writer().update_timer());
        let mut hub = self.relay.hub();
        if c.dec.players_own() {
            hub.with_next_keyframe(super::cursor::as_the_player(c.view));
        }
        hub.send_keyframe(tick, &key, &since, c.paused);
        hub.reshow_notes(Instant::now());
        drop(hub);
        self.floor = Some(super::follow::floor_after(self.floor, &c.dec, tick, &since));
        if std::mem::take(&mut self.rekeyed) {
            self.clock = Some((f64::from(tick) * f64::from(interval), Instant::now()));
        }
        self.problem = None;
        // Those just in are told where the camera is.
        self.shot.cued = None;
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
            Some(next) if !want.paused && next <= target => self.play_frame(want),
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

impl Player<'_> {
    /// Every client gets a full update.
    pub(super) fn rekey(&mut self) {
        self.relay.hub().rekey_all(Instant::now());
        self.rekeyed = true;
        self.floor = None;
    }
}
