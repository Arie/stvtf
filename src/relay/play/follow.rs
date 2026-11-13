//! Frames played to the clients in the game, as they are where a game client can follow them.

use super::Camera;
use super::cursor::without_cues;
use super::player::Player;
use crate::decode::{Decoder, Entities};
use crate::net::bits::BitBuf;
use crate::relay::hub::Part;
use std::time::Instant;

impl Player<'_> {
    /// Plays the next frame to the clients (without the demo's camera cues when the room has its
    /// own `camera`). One they cannot follow as it is (see [`follows`]) is passed over, and they
    /// get a full update of their own instead.
    pub(super) fn play_frame(&mut self, camera: Option<Camera>) {
        let Some(c) = &mut self.cur else { return };
        let base = c.next_frame().and_then(|d| c.dec.entities(d));
        if !follows(self.floor, base) {
            trace!(
                "a frame the clients cannot follow, from {base:?}, they have from {:?}: a full update instead",
                self.floor
            );
            // Only its string table changes reach them; its sounds, events and temp entities do
            // not.
            if let Some((bits, life)) = c.decode() {
                self.relay.hub().skim(&bits, &life);
            }
            self.rekey();
            return;
        }
        if let Some(Entities::Delta(b)) = base {
            self.floor = Some(self.floor.map_or(b, |f| f.max(b)));
        }
        let was_paused = c.paused;
        let Some((bits, life)) = c.decode() else { return };
        // Ours, not the demo's director's.
        let (bits, life) = if camera.is_some() { without_cues(&c.dec, &bits, &life) } else { (bits, life) };
        let mut hub = self.relay.hub();
        hub.upstream_stream(&bits, Part::Unreliable, &life);
        hub.upstream_flush();
        self.clock = Some((c.tick() as f64 * f64::from(c.dec.tick_interval()), Instant::now()));
        // The match's own HUD resets (a spawn, a new round) drop the notes too.
        if life.spans.iter().any(|s| crate::relay::server_msg::is_reset_hud(bits.bytes(), s)) {
            hub.reshow_notes(Instant::now());
        }
        // The match paused or went on: the clients are told reliably too.
        if c.paused != was_paused {
            hub.queue_pause(c.paused);
            // Going on after it, the timers redraw at once (as casters' `hud_reloadscheme` has
            // them do).
            if !c.paused
                && let Some(m) = c.dec.update_timer()
            {
                hub.queue_to_active(&m);
            }
        }
    }
}

/// Whether clients that may have no frame older than `floor` follow one whose entities build on
/// `base`. Not a full update: a game client keeps no frame from
/// before it, and the demo's next delta may build on one (a recording that asked for a full update
/// can have deltas from before it).
pub(super) fn follows(floor: Option<u32>, base: Option<Entities>) -> bool {
    match base {
        None => true,
        Some(Entities::Full) => false,
        Some(Entities::Delta(b)) => floor.is_none_or(|f| b >= f),
    }
}

/// The oldest frame the clients in the game may have, from `floor`, once those just in got a full
/// update of frame `key` and the frames `since`: what the latest of them builds on.
pub(super) fn floor_after(floor: Option<u32>, dec: &Decoder, key: u32, since: &[BitBuf]) -> u32 {
    let base = since
        .iter()
        .filter_map(|f| match dec.entities(f.bytes()) {
            Some(Entities::Delta(b)) => Some(b),
            _ => None,
        })
        .fold(key, u32::max);
    floor.map_or(base, |f| f.max(base))
}
