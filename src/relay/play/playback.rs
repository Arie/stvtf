//! Where playback should be, and what the clients are to see besides the demo: the control surface.

use super::{Beam, Light, Note, VoiceIn, voice};
use crate::lock;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
    pub(super) fn now(&self, interval: f32) -> u32 {
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
    /// Beams the clients in the game keep seeing, until replaced, and how many times they were.
    kept: Mutex<(u64, Arc<Vec<Beam>>)>,
    /// Notes they keep seeing, until replaced, and how many times they were.
    pub(super) notes: Mutex<(u64, Arc<Vec<Note>>)>,
    /// Voice to send the clients in the game as players' own, in order.
    voice: Mutex<VecDeque<VoiceIn>>,
    /// The light they keep seeing, if any.
    pub(super) light: Mutex<Option<Light>>,
}

impl Playback {
    /// Playback that starts out wanting `want`.
    pub fn new(want: Want) -> Self {
        Playback {
            want: Mutex::new(want),
            problem: Mutex::new(None),
            kept: Mutex::new((0, Arc::default())),
            notes: Mutex::new((0, Arc::default())),
            voice: Mutex::new(VecDeque::new()),
            light: Mutex::new(None),
        }
    }

    /// The light the clients in the game keep seeing (a room's spotlight), in place of the one
    /// before; None puts it out.
    pub fn keep_light(&self, light: Option<Light>) {
        *lock(&self.light) = light;
    }

    /// Sends `v` to the clients in the game as that player's voice, those in it now; when more
    /// waits than they could take, the oldest goes.
    pub fn voice(&self, v: VoiceIn) {
        let mut q = lock(&self.voice);
        if q.len() >= voice::MAX_QUEUED {
            q.pop_front();
        }
        q.push_back(v);
    }

    pub(super) fn take_voice(&self) -> Vec<VoiceIn> {
        lock(&self.voice).drain(..).collect()
    }

    /// Notes the clients in the game keep seeing (a room's), in place of those before: shown
    /// again after every full update; those gone are hidden. The same ones again change nothing.
    pub fn keep_notes(&self, notes: Vec<Note>) {
        let mut kept = lock(&self.notes);
        if *kept.1 != notes {
            kept.0 += 1;
            kept.1 = Arc::new(notes);
        }
    }

    /// Beams the clients in the game keep seeing (a room's marker), in place of those kept before:
    /// shown again every `KEPT_EVERY`, each for as long, so the old go within that and nothing
    /// shows twice over.
    pub fn keep(&self, beams: Vec<Beam>) {
        let mut kept = lock(&self.kept);
        kept.0 += 1;
        kept.1 = Arc::new(beams);
    }

    pub(super) fn kept(&self) -> (u64, Arc<Vec<Beam>>) {
        lock(&self.kept).clone()
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

    pub(super) fn get(&self) -> Want {
        lock(&self.want).clone()
    }

    pub(super) fn set_problem(&self, problem: Option<String>) {
        *lock(&self.problem) = problem;
    }
}
