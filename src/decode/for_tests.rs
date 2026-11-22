//! What the crate's tests reach into the decoder for.

use tf_demo_parser::ParserState;

use super::{Decoder, Tables};
use crate::net::bits::BitBuf;

/// The index of prop `id` in class `class`'s table, if it has it.
pub(crate) fn props_index_for_tests(
    state: &ParserState,
    class: u16,
    id: tf_demo_parser::demo::sendprop::SendPropIdentifier,
) -> Option<u32> {
    super::props::prop_index(state.send_tables.get(usize::from(class))?, id)
}

/// A Steam voice packet's `SteamID64`, sample rate and Opus frames.
pub(crate) fn steam_voice_for_tests(data: &[u8]) -> Option<(u64, u16, Vec<Vec<u8>>)> {
    super::extract::steam_voice_parts(data)
}

pub(super) fn tests_tables() -> Tables {
    crate::builds::tables(crate::builds::CURRENT, None).unwrap()
}

impl Decoder {
    pub(crate) fn player_for_tests(&self) -> Option<u32> {
        self.scene.world.as_ref()?.player()
    }

    /// Whether frame `tick` is kept.
    pub(crate) fn has_frame_for_tests(&self, tick: u32) -> bool {
        self.scene.world.as_ref().is_some_and(|w| w.has(tick))
    }

    /// A full update of frame `at`, if kept.
    pub(crate) fn keyframe_at_for_tests(&self, at: u32) -> Option<BitBuf> {
        Some(self.scene.world.as_ref()?.full_update(self.handler.get_parser_state(), Some(at), None)?.1)
    }

    /// Frame `to` as a delta from frame `from`, if both are kept.
    pub(crate) fn delta_for_tests(&self, from: u32, to: u32) -> Option<BitBuf> {
        self.scene.world.as_ref()?.delta(self.handler.get_parser_state(), from, to)
    }

    /// Every entity as the world has it.
    pub(crate) fn world_dump(&self) -> std::collections::BTreeMap<u32, super::world::Dumped> {
        self.scene.world.as_ref().map_or_default(super::world::World::dump)
    }

    /// [`Decoder::world_dump`] for comparing two decoders, one of which started from a full
    /// update. Leaves out the weapons' `m_bResetParity`, which a full update sets to the value a
    /// client starts with. In a player's own demo it also leaves out that player's origin as sent
    /// to other players, which a full update copies from the origin sent to their own game.
    pub(crate) fn world_compared(&self) -> std::collections::BTreeMap<u32, super::world::Dumped> {
        let state = self.handler.get_parser_state();
        let parity = tf_demo_parser::demo::sendprop::SendPropIdentifier::new("DT_TFWeaponBase", "m_bResetParity");
        let mut dump = self.world_dump();
        for (class, _, props) in dump.values_mut() {
            if let Some(at) = props_index_for_tests(state, *class, parity) {
                props.retain(|(i, _)| *i != at);
            }
        }
        if let Some((class, _, props)) = self.player_for_tests().and_then(|p| dump.get_mut(&p)) {
            for name in ["m_vecOrigin", "m_vecOrigin[2]"] {
                let id = tf_demo_parser::demo::sendprop::SendPropIdentifier::new("DT_TFNonLocalPlayerExclusive", name);
                if let Some(at) = props_index_for_tests(state, *class, id) {
                    props.retain(|(i, _)| *i != at);
                }
            }
        }
        dump
    }

    /// How many entities a keyframe would hold.
    pub(crate) fn world_len(&self) -> usize {
        self.scene.world.as_ref().map_or(0, super::world::World::len)
    }

    pub(crate) fn table_names_for_tests(&self, name: &str) -> Option<std::collections::BTreeMap<u16, Option<String>>> {
        self.scene.tables.names_for_tests(name)
    }

    pub(crate) fn parser_state_for_tests(&self) -> &ParserState {
        self.handler.get_parser_state()
    }

    pub(crate) fn rig_for_tests(&self) -> Option<(u32, u32)> {
        self.scene.world.as_ref()?.rig()
    }
}
