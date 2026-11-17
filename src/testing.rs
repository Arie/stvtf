//! What the crate's tests share.

use std::path::{Path, PathBuf};

/// A file in `tests/fixtures`.
pub(crate) fn fixture(name: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).join(name)
}
