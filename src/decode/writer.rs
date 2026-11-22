//! Messages written for a relay's clients in the stream's own encoding: game events, director
//! camera cues, and (in `beams`, `lights`, `notes`, `voice_out`) what a room shows and says.
//! A [`Writer`] reads the decoder's state and changes none of it.

use tf_demo_parser::ParserState;

use super::decoder::{Decoder, EVENT_ID_BITS, GAMEEVENT_LENGTH_BITS, Span, encode_message};
use super::extract::Extractor;
use crate::net::bits::{BitBuf, BitReader};
use crate::net::messages::{SVC_GAMEEVENT, TYPE_BITS};

/// Writes messages as the stream the decoder is on would have them.
pub(crate) struct Writer<'a> {
    pub(super) state: &'a ParserState,
    pub(super) extractor: &'a Extractor,
    pub(super) tables: &'a super::tables::Tables,
}

impl Decoder {
    /// A writer of messages for clients of this stream, as it is now.
    pub(crate) fn writer(&self) -> Writer<'_> {
        Writer { state: self.handler.get_parser_state(), extractor: self.extractor(), tables: &self.tables }
    }
}

impl Writer<'_> {
    /// The entity of user id `user`, while they are in the match.
    fn player_entity(&self, user: u16) -> Option<u32> {
        Some(self.extractor.snapshot().players.iter().find(|p| p.id.0 == user)?.ent)
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
        let state = self.state;
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

    /// Whether the message at `span` is a director camera cue (`hltv_chase`, `hltv_fixed`, …).
    pub(crate) fn is_director_cue(&self, data: &[u8], span: &Span) -> bool {
        use tf_demo_parser::demo::gameevent_gen::GameEventType as E;
        if u32::from(span.id) != SVC_GAMEEVENT {
            return false;
        }
        let at = span.bits.start + TYPE_BITS + GAMEEVENT_LENGTH_BITS;
        let Ok(id) = BitReader::at(data, at).read_bits(EVENT_ID_BITS) else { return false };
        let defs = &self.state.event_definitions;
        defs.get(id as usize).is_some_and(|d| {
            matches!(
                d.event_type,
                E::HLTVChase | E::HLTVFixed | E::HLTVCameraman | E::HLTVRankCamera | E::HLTVRankEntity
            )
        })
    }
}
