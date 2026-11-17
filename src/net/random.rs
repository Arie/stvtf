//! Random numbers for the protocol's nonces and challenges.

use std::hash::{BuildHasher, RandomState};
use std::time::Instant;

/// Unpredictable bits: `RandomState` is keyed from the OS random source, so no extra dependency
/// is needed.
pub(crate) fn random_u64() -> u64 {
    RandomState::new().hash_one(Instant::now())
}
