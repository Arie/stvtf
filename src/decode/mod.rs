//! Feeds network message streams into tf-demo-parser.
//!
//! A `SourceTV` client gets no send tables over the wire (the server expects the
//! client to build them from its own game DLL, checked by the `SendTable` CRC), so
//! the tables come from any demo recorded on the same server build. Each network
//! message stream is then wrapped as a demo `Message` packet and handed to the
//! same `DemoHandler` a demo file would use.

pub(crate) mod beams;
pub(crate) mod lights;

mod crc;
mod decoder;
mod demo_file;
mod detail;
mod extract;
mod modes;
mod notes;
mod props;
mod send_tables;
mod tables;
mod voice_out;
mod world;

pub(crate) use self::decoder::{Baselines, Decoder, Entities, Lifecycle, Span};
pub use self::demo_file::demo_file;
pub use self::send_tables::Tables;
pub(crate) use self::tables::{BeamModel, ModelsSent, Placed};
pub(crate) use self::voice_out::{steam_voice, voice_runs};
pub(crate) use self::world::Pose;

#[cfg(test)]
mod baseline_tests;
#[cfg(test)]
mod for_tests;
#[cfg(test)]
mod keyframe_tests;
#[cfg(test)]
mod speed;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use self::for_tests::{props_index_for_tests, steam_voice_for_tests};
