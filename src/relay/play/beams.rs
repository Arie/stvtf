//! Kept beams shown again as they run out, and the clients' clock put past them.

use super::playback::Versioned;
use super::player::Player;
use super::{Beam, FLUSH, KEPT_BURST, KEPT_EVERY, KEPT_RATE, NAP, Playback, SET_BACK};
use crate::net::bits::BitBuf;
use crate::net::messages;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// The kept beams shown: which of them were last handed over, when each was last sent, and
/// how many may be sent now (refilled at [`KEPT_RATE`]).
pub(super) struct Kept {
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

impl Kept {
    pub(super) fn new(now: Instant) -> Self {
        Kept { version: 0, sent: HashMap::new(), tokens: KEPT_BURST, refilled: now }
    }

    /// Every kept beam is due again.
    pub(super) fn clear(&mut self) {
        self.sent.clear();
    }

    /// Those no longer kept are forgotten, once `version` changed.
    fn follow(&mut self, version: u64, beams: &[Beam]) {
        if version != self.version {
            let keys: HashSet<BeamKey> = beams.iter().map(beam_key).collect();
            self.sent.retain(|key, _| keys.contains(key));
            self.version = version;
        }
    }

    fn refill(&mut self, now: Instant) {
        self.tokens = (self.tokens + now.duration_since(self.refilled).as_secs_f64() * KEPT_RATE).min(KEPT_BURST);
        self.refilled = now;
    }

    /// Those of `beams` due now, as many as may be sent: those about to run out first, then those
    /// never shown; each to show for `life`.
    fn due(&self, beams: &[Beam], now: Instant, life: f32) -> Vec<Beam> {
        let mut due: Vec<(bool, &Beam)> = beams
            .iter()
            .filter_map(|b| match self.sent.get(&beam_key(b)) {
                Some(at) if now.duration_since(*at) < KEPT_EVERY => None,
                at => Some((at.is_none(), b)),
            })
            .collect();
        due.sort_by_key(|(new, _)| *new);
        due.truncate(self.tokens as usize);
        due.into_iter().map(|(_, b)| Beam { life, ..*b }).collect()
    }

    fn mark_sent(&mut self, shown: &[Beam], now: Instant) {
        self.tokens -= shown.len() as f64;
        for b in shown {
            self.sent.insert(beam_key(b), now);
        }
    }
}

/// The tick past the beams, if the clock at `now` goes back to `key` with beams until `until`.
pub(super) fn past_beams(now: f64, key: f64, until: f64, interval: f32) -> Option<i32> {
    (now < until && now - key > SET_BACK && interval > 0.0).then(|| ((until + 1.0) / f64::from(interval)).ceil() as i32)
}

impl Player<'_> {
    /// Puts the clients' clock past their beams before a full update at `key` sets it back.
    /// Whether the full update is to wait.
    pub(super) fn flush(&mut self, key: i64, interval: f32) -> bool {
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
        self.relay.hub().send_to_waiting(&messages::tick(ahead));
        self.flushing = Some(Instant::now() + FLUSH);
        self.beams_until = 0.0;
        self.kept.clear();
        self.shown_light.again();
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
        self.models = c.dec.models_sent(self.models);
        let (mut model, mut load) = c.dec.beam_model(self.laser, self.models)?;
        if load.is_some() && self.match_laser.as_ref() == Some(&c.path) {
            // Its own comes later: until the clients have it, a sprite of the match's or none.
            (model, load) = (c.dec.beam_sprite()?, None);
        }
        if let Some(msg) = load {
            self.relay.hub().add_table_change_once(&msg);
            self.laser = Some(model);
        }
        c.dec.beams(beams, model)
    }

    /// Whether the match loaded the laser after the clients were given the relay's: every model of
    /// the match's after it is one off for them, so they sign on again (the demo opened afresh),
    /// after which its own is used.
    pub(super) fn laser_twice(&mut self) -> bool {
        if !self.cur.as_ref().is_some_and(|c| c.dec.laser_twice(self.laser)) {
            return false;
        }
        log!(":{} the match loaded the laser too: the clients sign on again", self.relay.port());
        self.match_laser = self.cur.take().map(|c| c.path);
        true
    }

    /// Shows each kept beam again as it runs out, for [`KEPT_EVERY`], new ones at once: at most
    /// [`KEPT_RATE`] a second, in bursts of [`KEPT_BURST`], so they never crowd out the frames
    /// (a client that misses one cannot follow the next).
    pub(super) fn show_kept(&mut self, playback: &Playback) {
        let Versioned { version, items: beams } = playback.beams();
        // Only once a client is in the game where playback is: at a demo's start, before it skims
        // to the wanted moment, a laser loaded would be the match's slot later.
        if self.cur.as_ref().is_none_or(|c| !c.started) || self.rekeyed || !self.relay.hub().any_in_game() {
            return;
        }
        let now = Instant::now();
        self.kept.follow(version, &beams);
        self.kept.refill(now);
        let life = KEPT_EVERY.as_secs_f32();
        let show = self.kept.due(&beams, now, life);
        if show.is_empty() {
            return;
        }
        // Shown only when there is a sprite to draw them with: else they are due again at once.
        if let Some(msgs) = self.beam_messages(&show) {
            self.kept.mark_sent(&show, now);
            let mut hub = self.relay.hub();
            for m in &msgs {
                hub.send_to_active(m);
            }
            drop(hub);
            self.shown_for(life);
        }
    }
}
