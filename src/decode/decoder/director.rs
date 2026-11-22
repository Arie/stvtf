//! What a relay tells its clients through the decoder: the round's events, director camera cues,
//! and the relay's own camera.

use super::{Decoder, EVENT_ID_BITS, GAMEEVENT_LENGTH_BITS, Span, encode_message};
use crate::decode::world::Pose;
use crate::net::bits::{BitBuf, BitReader};
use crate::net::messages::{SVC_GAMEEVENT, TYPE_BITS};

impl Decoder {
    /// The last round start, win panel or game over event: what a client that skipped there
    /// (a jump, or one just in) is sent after a full update, so its HUD shows the round's end, or
    /// hides it, as the game does. Before the demo's first, a round start written here: one jumped
    /// back there from a win panel has it hidden.
    pub(crate) fn round_event(&self) -> Option<BitBuf> {
        use tf_demo_parser::demo::gameevent_gen::{GameEvent, TeamPlayRoundStartEvent};
        self.round
            .clone()
            .or_else(|| self.game_event(GameEvent::TeamPlayRoundStart(TeamPlayRoundStartEvent { full_reset: false })))
    }

    /// Sets the last round event, for a decoder that starts from a checkpoint.
    pub(crate) fn set_round_event(&mut self, event: Option<BitBuf>) {
        self.round = event;
    }

    /// The entity of user id `user`, while they are in the match.
    pub(crate) fn player_entity(&self, user: u16) -> Option<u32> {
        Some(self.extractor().snapshot().players.iter().find(|p| p.id.0 == user)?.ent)
    }

    /// The user id and entity of every player in the match and alive.
    pub(crate) fn living_players(&self) -> Vec<(u16, u32)> {
        self.extractor().snapshot().players.iter().filter(|p| p.alive).map(|p| (p.id.0, p.ent)).collect()
    }

    /// `teamplay_update_timer`: the HUD's timers redraw, as from a time the clock went back past
    /// (a jump back, playback going on after a pause), and nothing else does (`ResetHUD` would
    /// blank the medic's charge meter until the charge changes).
    pub(crate) fn update_timer(&self) -> Option<BitBuf> {
        use tf_demo_parser::demo::gameevent_gen::{GameEvent, TeamPlayUpdateTimerEvent};
        self.game_event(GameEvent::TeamPlayUpdateTimer(TeamPlayUpdateTimerEvent {}))
    }

    /// `hltv_chase` into the eyes of user id `player`, for clients on `spec_autodirector 1`.
    pub(crate) fn hltv_chase(&self, player: u16) -> Option<BitBuf> {
        use tf_demo_parser::demo::gameevent_gen::{GameEvent, HLTVChaseEvent};
        let ent = self.player_entity(player)?;
        self.game_event(GameEvent::HLTVChase(HLTVChaseEvent {
            target_1: u16::try_from(ent).ok()?,
            target_2: 0,
            distance: 96,
            theta: 0,
            phi: 0,
            inertia: 0,
            in_eye: 1,
        }))
    }

    /// `event` as an `svc_GameEvent`, if the demo's event list has its kind.
    pub(in crate::decode) fn game_event(
        &self,
        event: tf_demo_parser::demo::gameevent_gen::GameEvent,
    ) -> Option<BitBuf> {
        use tf_demo_parser::demo::message::gameevent::GameEventMessage;
        let state = self.handler.get_parser_state();
        let event_type = event.event_type();
        let def = state.event_definitions.iter().find(|d| d.event_type == event_type)?;
        encode_message(SVC_GAMEEVENT, &GameEventMessage { event_type_id: def.id, event_type, event }, state)
    }

    /// `hltv_fixed` at `pos` looking along `pitch` and `yaw` (degrees), for the same clients.
    /// The game takes the coordinates and angles as signed, and no field of view but its own.
    #[allow(clippy::cast_sign_loss)]
    pub(crate) fn hltv_fixed(&self, pos: [i32; 3], pitch: i16, yaw: i16) -> Option<BitBuf> {
        use tf_demo_parser::demo::gameevent_gen::{GameEvent, HLTVFixedEvent};
        self.game_event(GameEvent::HLTVFixed(HLTVFixedEvent {
            pos_x: pos[0] as u32,
            pos_y: pos[1] as u32,
            pos_z: pos[2] as u32,
            theta: pitch as u16,
            phi: yaw as u16,
            offset: 0,
            fov: 90.0,
            target: 0,
        }))
    }

    /// `hltv_cameraman`: the same clients see what player `index` does.
    pub(crate) fn hltv_cameraman(&self, index: u32) -> Option<BitBuf> {
        use tf_demo_parser::demo::gameevent_gen::{GameEvent, HLTVCameramanEvent};
        self.game_event(GameEvent::HLTVCameraman(HLTVCameramanEvent { index: u16::try_from(index).ok()? }))
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

    #[cfg(test)]
    pub(crate) fn rig_for_tests(&self) -> Option<(u32, u32)> {
        self.world.as_ref()?.rig()
    }

    /// The player slots nobody is in, highest first: not the cameraman's.
    pub(crate) fn spare_slots(&self) -> Vec<u8> {
        let man = self.cameraman();
        self.extractor().spare_slots().into_iter().filter(|s| man != Some(u32::from(*s) + 1)).collect()
    }

    /// Whether the message at `span` is a director camera cue (`hltv_chase`, `hltv_fixed`, …).
    pub(crate) fn is_director_cue(&self, data: &[u8], span: &Span) -> bool {
        use tf_demo_parser::demo::gameevent_gen::GameEventType as E;
        if u32::from(span.id) != SVC_GAMEEVENT {
            return false;
        }
        let at = span.bits.start + TYPE_BITS + GAMEEVENT_LENGTH_BITS;
        let Ok(id) = BitReader::at(data, at).read_bits(EVENT_ID_BITS) else { return false };
        let defs = &self.handler.get_parser_state().event_definitions;
        defs.get(id as usize).is_some_and(|d| {
            matches!(
                d.event_type,
                E::HLTVChase | E::HLTVFixed | E::HLTVCameraman | E::HLTVRankCamera | E::HLTVRankEntity
            )
        })
    }
}
