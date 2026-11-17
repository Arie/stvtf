//! What stvtf produces: one [`Line`] per event, snapshot or status change.
//!
//! Each type serializes to the JSON the README's Output section describes (one object per line,
//! tagged by `type`), and reads back from it, so saved output can be loaded into the same types.

mod events;
mod kinds;
mod line;
mod round;
mod snapshot;
mod status;

pub use self::events::*;
pub use self::kinds::*;
pub use self::line::{Line, Stamped};
pub use self::round::{Position, round1, round2, round3};
pub use self::snapshot::*;
pub use self::status::{Server, Status, World};

#[cfg(test)]
mod tests;
