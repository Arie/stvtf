//! The TF2 server builds whose data is compiled in, and the send tables of any build.

use std::path::Path;

use crate::Error;
use crate::decode::Tables;

/// The newest TF2 server build compiled in. A build with no data compiled in gets its tables.
pub const CURRENT: &str = BUILDS[0].0;

/// Known TF2 server builds (`A2S_INFO` `version`) and the demo of their data tables, newest first.
/// Most updates keep the tables, so builds share them. `cargo run --example add_build` adds to it.
const BUILDS: &[(&str, &[u8])] = &[
    // add_build: builds
    ("11267535", TABLES_11138802),
    ("11185272", TABLES_11138802),
];

// add_build: tables
const TABLES_11138802: &[u8] = include_bytes!("../assets/tables_11138802.dem");

/// Compiled-in data for one TF2 server build.
#[derive(Copy, Clone, Debug)]
pub struct Build {
    /// A demo of that build cut off after its data tables, which give its `SendTable` CRC too.
    pub tables: &'static [u8],
}

/// Compiled-in data for known TF2 server builds (`A2S_INFO` `version`).
pub fn builtin(build: &str) -> Option<Build> {
    BUILDS.iter().find(|b| b.0 == build).map(|b| Build { tables: b.1 })
}

/// Send tables for a build: the given demo, or the compiled-in ones.
pub fn tables(build: &str, demo: Option<&Path>) -> crate::Result<Tables> {
    match (demo, builtin(build)) {
        (Some(p), _) => Tables::from_demo(p),
        (None, Some(b)) => Tables::from_demo_bytes(b.tables),
        (None, None) => Err(Error::UnknownBuild { build: build.to_string() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_without_data_needs_a_demo() {
        assert!(tables(CURRENT, None).is_ok());
        assert!(matches!(tables("1", None), Err(Error::UnknownBuild { build }) if build == "1"));
        assert!(tables(CURRENT, Some(Path::new("/nonexistent.dem"))).is_err(), "a given demo counts");
    }
}
