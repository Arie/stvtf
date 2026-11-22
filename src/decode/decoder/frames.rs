//! Frames written for a relay's clients from the world kept: full updates, deltas from a frame a
//! client has, and what follows a full update.

use super::{Decoder, Entities};
use crate::net::bits::BitBuf;

impl Decoder {
    /// `net_Tick` and every entity in full, of the frame that `next` (the next frame's message
    /// stream, if known) builds on, or the latest when it is a full update or not known, with that
    /// frame's server tick: a client that starts from it, and gets the frames after it, can
    /// follow. None without [`Decoder::keep_world`], before a full update came, after a frame may
    /// have been missed, or when the frame `next` builds on is not kept.
    pub(crate) fn keyframe(&self, next: Option<&[u8]>) -> Option<(u32, BitBuf)> {
        let base = next.and_then(|d| self.delta_base(d));
        self.world.as_ref()?.full_update(self.handler.get_parser_state(), base, None)
    }

    /// [`Decoder::keyframe`] with every running round timer held where it is, for clients whose
    /// clock runs on while no frames come (a room paused).
    pub(crate) fn keyframe_held(&self, next: Option<&[u8]>) -> Option<(u32, BitBuf)> {
        let base = next.and_then(|d| self.delta_base(d));
        self.world.as_ref()?.full_update(self.handler.get_parser_state(), base, Some(self.tick_interval()))
    }

    /// The entities of the last frame decoded as a delta from frame `from`, the one message: for a
    /// client that acked `from`. None when either is not kept, or it does not encode.
    pub(crate) fn entities_from(&self, from: u32) -> Option<BitBuf> {
        self.world.as_ref()?.delta_message(self.handler.get_parser_state(), from, self.server_tick())
    }

    /// The frames to send after a keyframe of frame `key`, from `recent` (frames played, by server
    /// tick): those after it, each as played if the client still has the frame it builds on, else
    /// (from a live server, a frame a few ticks back) rewritten as a delta from `key`, or failing
    /// that the last it was sent. A game client keeps only frames with entities, and drops those
    /// older than the base of each delta it takes.
    pub(crate) fn catch_up(&self, key: u32, recent: &[(u32, BitBuf)]) -> Vec<BitBuf> {
        let Some(world) = &self.world else { return Vec::new() };
        let state = self.handler.get_parser_state();
        let mut have = vec![key];
        let mut out = Vec::new();
        let own = world.own_baselines();
        for (tick, bits) in recent.iter().filter(|(t, _)| *t > key) {
            match self.entities(bits.bytes()) {
                // No entities in it: nothing builds on it.
                None => out.push(bits.clone()),
                Some(Entities::Delta(base)) if have.contains(&base) && !own => {
                    out.push(bits.clone());
                    have.retain(|t| *t >= base);
                }
                _ => {
                    // From the keyframe's frame while the client has it: the stream's next frames
                    // may still build on that one.
                    let from = if have.contains(&key) { key } else { have.last().copied().unwrap_or(key) };
                    match world.delta(state, from, *tick) {
                        Some(d) => {
                            out.push(d);
                            have.retain(|t| *t >= from);
                        }
                        None if own => {}
                        None => out.push(bits.clone()),
                    }
                }
            }
            if world.has(*tick) && !have.contains(tick) {
                have.push(*tick);
            }
        }
        out
    }
}
