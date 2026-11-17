//! What the crate's tests reach into the decoder for.

use tf_demo_parser::ParserState;

use super::Tables;

/// The index of prop `id` in class `class`'s table, if it has it.
#[cfg(test)]
pub(crate) fn props_index_for_tests(
    state: &ParserState,
    class: u16,
    id: tf_demo_parser::demo::sendprop::SendPropIdentifier,
) -> Option<u32> {
    super::props::prop_index(state.send_tables.get(usize::from(class))?, id)
}

/// A Steam voice packet's `SteamID64`, sample rate and Opus frames.
#[cfg(test)]
pub(crate) fn steam_voice_for_tests(data: &[u8]) -> Option<(u64, u16, Vec<Vec<u8>>)> {
    super::extract::steam_voice_parts(data)
}

#[cfg(test)]
pub(super) fn tests_tables() -> Tables {
    crate::builds::tables(crate::builds::CURRENT, None).unwrap()
}
