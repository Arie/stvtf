//! What a relay tells its clients through the decoder: the round's events, director camera cues,
//! and the relay's own camera.

use super::Decoder;
use crate::decode::world::Pose;
use crate::net::bits::BitBuf;

impl Decoder {
    /// The last round start, win panel or game over event: what a client that skipped there
    /// (a jump, or one just in) is sent after a full update, so its HUD shows the round's end, or
    /// hides it, as the game does. Before the demo's first, a round start written here: one jumped
    /// back there from a win panel has it hidden.
    pub(crate) fn round_event(&self) -> Option<BitBuf> {
        use tf_demo_parser::demo::gameevent_gen::{GameEvent, TeamPlayRoundStartEvent};
        self.round.clone().or_else(|| {
            self.writer().game_event(GameEvent::TeamPlayRoundStart(TeamPlayRoundStartEvent { full_reset: false }))
        })
    }

    /// Sets the last round event, for a decoder that starts from a checkpoint.
    pub(crate) fn set_round_event(&mut self, event: Option<BitBuf>) {
        self.round = event;
    }

    /// The user id and entity of every player in the match and alive.
    pub(crate) fn living_players(&self) -> Vec<(u16, u32)> {
        self.extractor().snapshot().players.iter().filter(|p| p.alive).map(|p| (p.id.0, p.ent)).collect()
    }

    /// The clients' camera from the next frame on: at `pose`, or (none) the director's again.
    pub(crate) fn set_shot(&mut self, pose: Option<Pose>) {
        let (model, slots) = (self.tables.studio(), self.extractor().max_players());
        if let Some(w) = &mut self.world {
            w.set_shot(pose, model, slots);
        }
    }

    /// The player slots (from 0) the cameraman is not to take: a voice's.
    pub(crate) fn keep_from_shot(&mut self, slots: &[u8]) {
        if let Some(w) = &mut self.world {
            w.keep_from_shot(slots);
        }
    }

    /// The same in the frames kept, for a room paused. Whether any changed: a full update then.
    pub(crate) fn reshoot(&mut self) -> bool {
        let state = self.handler.get_parser_state();
        self.world.as_mut().is_some_and(|w| w.reshoot(state))
    }

    /// Whether the last frame changed the relay's camera: its entities are for no client then.
    pub(crate) fn shot_touched(&self) -> bool {
        self.world.as_ref().is_some_and(crate::decode::world::World::shot_touched)
    }

    /// The player who watches for the clients where the relay has the camera, once there.
    pub(crate) fn cameraman(&self) -> Option<u32> {
        self.world.as_ref()?.cameraman()
    }

    /// The player slots nobody is in, highest first: not the cameraman's.
    pub(crate) fn spare_slots(&self) -> Vec<u8> {
        let man = self.cameraman();
        self.extractor().spare_slots().into_iter().filter(|s| man != Some(u32::from(*s) + 1)).collect()
    }
}
