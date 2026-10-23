use std::hash::{BuildHasher, RandomState};
use std::time::Instant;

/// Room for any UDP datagram.
pub const MAX_DATAGRAM: usize = 65536;

/// Unpredictable bits: `RandomState` is keyed from the OS random source, so no extra dependency
/// is needed.
pub(crate) fn random_u64() -> u64 {
    RandomState::new().hash_one(Instant::now())
}

pub mod bits;
pub mod compress;
pub mod handshake;
pub mod messages;
pub mod netchan;
pub mod receiver;
pub mod reliable;
pub mod split;
