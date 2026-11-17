//! `.dem` files as raw packets: the signon and message packets keep their bits as they are, so
//! they can be passed on to a client unchanged, and a live session can be recorded as one.
//!
//! A demo is a 1072-byte header, then packets: a command byte, a tick, and per command:
//! signon and message packets 84 bytes of view and sequence data then a length and the message
//! stream; data tables, string tables and console commands a length and the data; user commands
//! a sequence, a length and the data; sync ticks nothing. A stop packet ends it.

mod command;
mod header;
mod reader;
mod record;
mod tables;
mod voice;
mod writer;

pub use self::header::{HEADER_LEN, Header};
pub use self::reader::{Next, Packet, Reader};
pub use self::record::Files;
pub(crate) use self::record::Recorder;
pub(crate) use self::tables::HeldTables;
pub use self::voice::{Voiced, with_voice};
pub use self::writer::Writer;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use self::record::tests::record_capture as record_capture_for_tests;
