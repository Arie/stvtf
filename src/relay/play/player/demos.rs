//! Which demo plays: opening it, going on into the next, taking over from the process before.

use std::ops::ControlFlow;
use std::time::Instant;

use super::Player;
use super::jumps::skim;
use crate::Result;
use crate::decode::{BeamModel, ModelsSent};
use crate::error::bail;
use crate::relay::handover::{Played, TakenOver};
use crate::relay::play::cursor::{Cursor, owned};
use crate::relay::play::index::Index;
use crate::relay::play::tuning::{NAP, NOT_A_FRAME, PAST_INDEXES, RETRY};
use crate::relay::play::{Playback, Want, light, names, notes};

impl Player<'_> {
    /// Opens the demo `want` names unless it is playing, once no signon is held. Breaks while one
    /// is held, or when the demo cannot be opened. A demo not written yet is tried again every
    /// [`RETRY`]; one that will never open is not tried again.
    pub(in crate::relay::play) fn open_wanted(&mut self, want: &Want, playback: &Playback) -> ControlFlow<()> {
        if self.cur.as_ref().is_some_and(|c| c.path == want.demo) {
            self.relay.hub().release_signon_hold();
            return ControlFlow::Continue(());
        }
        if self.given_up.as_ref() == Some(&want.demo) {
            std::thread::sleep(RETRY);
            return ControlFlow::Break(());
        }
        // Not while a client is in a signon: a second crashes it. Nothing plays meanwhile.
        if self.relay.hub().hold_for_signon(Instant::now()) {
            std::thread::sleep(NAP);
            return ControlFlow::Break(());
        }
        if self.goes_on_into(&want.demo) {
            return ControlFlow::Continue(());
        }
        if let Some(TakenOver(played)) = self.taken_over.take() {
            match played.filter(|p| p.demo == want.demo).map(|p| self.go_on_from(p)) {
                Some(Ok(())) => return ControlFlow::Continue(()),
                Some(Err(e)) => {
                    tracing::info!("relay: :{} the clients taken over sign on again: {e:#}", self.relay.port());
                }
                None => {
                    tracing::info!("relay: :{} the clients taken over sign on again: another demo", self.relay.port());
                }
            }
            self.signed_on = Some(want.demo.clone());
        }
        // Another demo (a live match's next one), or this again: clients in a game sign on again.
        self.cur = None;
        if let Some(was) = self.signed_on.take() {
            tracing::debug!("relay: the clients leave {} for {}", was.display(), want.demo.display());
            self.relay.hub().reconnect_all();
        }
        match Cursor::open(&want.demo, &self.relay.hub) {
            Ok(c) => {
                self.start_demo(c);
                self.refused = None;
                playback.set_problem(None);
                self.index_of(&want.demo);
                ControlFlow::Continue(())
            }
            Err(e) => {
                let why = format!("{e:#}");
                if !super::super::cursor::may_open_yet(&e) {
                    self.given_up = Some(want.demo.clone());
                }
                if self.refuses_anew(&want.demo, &why) {
                    tracing::info!("relay: cannot play {}: {why}", want.demo.display());
                }
                playback.set_problem(Some(why));
                std::thread::sleep(RETRY);
                ControlFlow::Break(())
            }
        }
    }

    /// Makes the checkpoints of `demo` the ones playback uses; those it used before are kept a
    /// while.
    pub(super) fn index_of(&mut self, demo: &std::path::Path) {
        if self.index.as_ref().is_some_and(|i| i.path == demo) {
            return;
        }
        if let Some(was) = self.index.replace(Index::shared(demo)) {
            self.past.retain(|i| i.path != was.path);
            self.past.push(was);
            if self.past.len() > PAST_INDEXES {
                self.past.remove(0);
            }
        }
    }

    /// Whether `demo` failing to open with `why` was not the last failure logged.
    pub(in crate::relay::play) fn refuses_anew(&mut self, demo: &std::path::Path, why: &str) -> bool {
        if self.refused.as_ref().is_some_and(|(d, w)| d == demo && w == why) {
            return false;
        }
        self.refused = Some((demo.to_owned(), why.to_owned()));
        true
    }

    /// Whether `demo` continues the one playing; if so, playback goes on in it. Both must be of
    /// one map load of one server (a match recorded again by the next process), and the one
    /// playing must have a frame at or past where `demo` begins: it is first played up to there.
    /// The clients keep what they have and get a full update; `demo`'s signon is for those to
    /// come. If the clients have the relay's laser, a client coming in later must get it at the
    /// same model index.
    pub(in crate::relay::play) fn goes_on_into(&mut self, demo: &std::path::Path) -> bool {
        let (laser, port, pose) = (self.laser, self.relay.port(), self.shot.pose);
        let Some(c) = self.cur.as_mut().filter(|c| c.started && c.server.is_some()) else { return false };
        let Ok(mut next) = Cursor::open_detached(demo) else { return false };
        while !next.started && next.next_tick().is_some() {
            if next.decode().is_none() {
                next.skip_next();
            }
        }
        let Some(begins) = next.first.filter(|_| next.server == c.server) else {
            tracing::info!(
                "relay: :{port} {} is of another map load ({:?}, playing {:?}): a signon",
                demo.display(),
                next.server,
                c.server
            );
            return false;
        };
        // Decodes the one playing up to there, or as far as it has, passing on its string table
        // changes. The frames are not sent: the clients need a full update either way.
        let mut passed = false;
        while c.tick() < begins {
            match c.next_tick() {
                None => break,
                Some(NOT_A_FRAME) => c.skip_next(),
                Some(_) => {
                    if let Some((bits, life)) = c.decode() {
                        self.relay.hub().skim(&bits, &life);
                        passed = true;
                    }
                }
            }
        }
        let at = c.tick();
        // The message that loads the laser in `next`, for those to come; none without a laser.
        // None if it would not load at the index the clients have it at.
        let laser_there = |next: &Cursor| match (laser, next.dec.writer().beam_model(None, ModelsSent::default())) {
            (None, _) => Some(None),
            (Some(has), Some(BeamModel { index, load: Some(load) })) if index == has => Some(Some(load)),
            _ => None,
        };
        // `next` starts from its last checkpoint before there, if any were made (a demo played
        // before).
        let made = self.past.iter().chain(&self.index).find(|i| i.path == demo);
        if let Some(m) = made.and_then(|i| i.before(at)).filter(|m| i64::from(m.tick) > next.tick())
            && let Ok(there) = Cursor::restore(demo, &m)
        {
            next = there;
        }
        next.dec.scene_mut().set_shot(pose);
        skim(&mut next, None, at);
        if at < begins || laser_there(&next).is_none() {
            let why =
                if at < begins { "the one playing ends before it begins" } else { "the laser is elsewhere in it" };
            tracing::info!("relay: :{port} {} at {begins}, playing at {at}: {why}: a signon", demo.display());
            if passed {
                self.rekey();
            }
            return false;
        }
        self.relay.hub().hold_in_game();
        // Its signon for those to come, then its string table changes up to there: `next` has
        // them, and is where playback goes on.
        let opened = Cursor::open(demo, &self.relay.hub).ok().and_then(|_| {
            self.relay.hub().add_table_changes(&owned(&next.tables));
            if let Some(load) = laser_there(&next)? {
                self.relay.hub().add_table_change_once(&load);
            }
            Some(next)
        });
        self.relay.hub().release_held(Instant::now());
        let Some(next) = opened else { return false };
        tracing::info!(
            "relay: :{} goes on into {} at {}, {} clients",
            self.relay.port(),
            demo.display(),
            next.tick(),
            self.relay.clients()
        );
        self.start_demo(next);
        self.laser = laser;
        self.index_of(demo);
        self.relay.hub().reshow_panel_with_next_keyframe();
        true
    }

    /// Goes on where the process before was (`played`): its demo decoded up to there, giving the
    /// clients nothing (they have it), and a full update of wherever playback is wanted next.
    pub(super) fn go_on_from(&mut self, played: Played) -> Result<()> {
        let took = Instant::now();
        let index = Index::shared(&played.demo);
        let mut c = match index.before(played.tick) {
            Some(m) => Cursor::restore(&played.demo, &m)?,
            None => Cursor::open_detached(&played.demo)?,
        };
        self.shoot_in(&mut c);
        skim(&mut c, None, played.tick);
        if c.tick() != played.tick || !c.started {
            bail!("cannot find a frame of {} at {}", played.demo.display(), played.tick);
        }
        tracing::info!(
            "relay: :{} goes on at {} after {} ms, {} clients",
            self.relay.port(),
            c.tick(),
            took.elapsed().as_millis(),
            self.relay.clients()
        );
        self.start_demo(c);
        self.index = Some(index);
        self.laser = played.laser;
        self.models = played.models;
        self.match_laser = played.match_laser;
        self.relay.hub().reshow_panel_with_next_keyframe();
        Ok(())
    }

    /// Plays `c`, just opened: the clients sign on to its tables, which lack the relay's laser.
    pub(in crate::relay::play) fn start_demo(&mut self, c: Cursor) {
        if self.match_laser.as_ref().is_some_and(|p| *p != c.path) {
            self.match_laser = None;
        }
        self.signed_on = Some(c.path.clone());
        self.cur = Some(c);
        self.laser = None;
        self.models = ModelsSent::default();
        self.floor = None;
        self.shown_notes = notes::Shown::default();
        self.shown_names = names::Shown::default();
        self.shown_light = light::Shown::default();
        self.clock = None;
        self.beams_until = 0.0;
    }
}
