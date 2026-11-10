//! TF2's text bubble (the training annotation), for a room's notes.

use super::Decoder;
use crate::net::bits::BitBuf;
use tf_demo_parser::demo::gameevent_gen::{GameEvent, HideAnnotationEvent, ShowAnnotationEvent};

/// A lifetime under 0: shown until hidden.
const FOR_GOOD: f32 = -1.0;

impl Decoder {
    /// `show_annotation`: bubble `id` saying `text` at `at`, or following entity `follow`, for
    /// everyone, until hidden; without the distance to it.
    pub(crate) fn annotation(&self, id: u32, text: &str, at: [f32; 3], follow: Option<u32>) -> Option<BitBuf> {
        self.game_event(GameEvent::ShowAnnotation(ShowAnnotationEvent {
            world_pos_x: at[0],
            world_pos_y: at[1],
            world_pos_z: at[2],
            world_normal_x: 0.0,
            world_normal_y: 0.0,
            world_normal_z: 1.0,
            id,
            text: text.into(),
            lifetime: FOR_GOOD,
            visibility_bit_field: u32::MAX,
            follow_ent_index: follow.unwrap_or(0),
            show_distance: false,
            play_sound: "".into(),
            show_effect: true,
        }))
    }

    /// `hide_annotation` of bubble `id`.
    pub(crate) fn hide_annotation(&self, id: u32) -> Option<BitBuf> {
        self.game_event(GameEvent::HideAnnotation(HideAnnotationEvent { id }))
    }
}
