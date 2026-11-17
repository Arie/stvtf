//! Turns parser state into the lines the overview consumes: periodic snapshots
//! plus discrete events (kills, damage, shots, explosions, ubers, rounds, chat...).

mod convert;
mod debug;
mod entities;
mod events;
mod extractor;
mod limits;
mod snapshot;
mod state;
mod stopwatch;
mod voice;

pub(super) use self::convert::{building_kind, opt_user, strip_colors, team_num, xyz};
pub(crate) use self::debug::debug;
pub(crate) use self::extractor::Extractor;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(in crate::decode) use self::extractor::steam_voice_parts;
