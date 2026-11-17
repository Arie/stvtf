//! A build's send tables, from a demo of it.

use bitbuffer::{BitRead, BitReadBuffer, BitReadStream, LittleEndian};
use tf_demo_parser::demo::header::Header;
use tf_demo_parser::demo::packet::Packet;
use tf_demo_parser::demo::packet::datatable::DataTablePacket;
use tf_demo_parser::demo::parser::{DemoHandler, RawPacketStream};

use crate::Result;
use crate::error::{Context, bail, invalid};

/// The entity class tables of one server build, taken from a demo of that build.
pub struct Tables {
    pub(super) header: Header,
    pub(super) datatables: DataTablePacket,
    pub(super) crc: u32,
    /// The data tables packet's bytes, for a demo written with them.
    pub(super) raw: Vec<u8>,
}

impl Tables {
    /// Reads the tables from a demo file (any STV or POV demo of the build).
    pub fn from_demo(path: &std::path::Path) -> Result<Tables> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        Self::from_demo_bytes(&bytes)
    }

    /// Reads the tables from demo bytes; everything after the data tables is ignored.
    pub fn from_demo_bytes(bytes: &[u8]) -> Result<Tables> {
        let mut stream = BitReadStream::new(BitReadBuffer::new(bytes, LittleEndian));
        let header = Header::read(&mut stream)?;
        let handler = DemoHandler::with_analyser(tf_demo_parser::demo::parser::NullHandler);
        let mut packets = RawPacketStream::new(stream);
        while let Some(packet) = packets.next(handler.get_parser_state())? {
            if let Packet::DataTables(dt) = packet {
                let crc = super::crc::sendtable_crc(&dt)?;
                return Ok(Tables { header, datatables: dt, crc, raw: raw_datatables(bytes)? });
            }
        }
        Err(invalid!("cannot find data tables in the demo"))
    }

    /// The server class names (`CTFPlayer`, …), in class id order.
    pub fn class_names(&self) -> impl Iterator<Item = &str> {
        self.datatables.server_classes.iter().map(|c| c.name.as_str())
    }

    /// The number of send tables.
    pub fn send_table_count(&self) -> usize {
        self.datatables.tables.len()
    }

    /// The data tables packet as a demo of the build has it.
    pub fn datatables_bytes(&self) -> &[u8] {
        &self.raw
    }

    /// The `SendTable` CRC of the build, as the server checks it at signon.
    pub fn sendtable_crc(&self) -> u32 {
        self.crc
    }
}

/// The data of the first data tables packet of a demo.
pub(super) fn raw_datatables(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut r = crate::demo::Reader::new(std::io::Cursor::new(bytes))?;
    loop {
        match r.next_packet()? {
            crate::demo::Next::Packet(crate::demo::Packet::DataTables { data, .. }) => return Ok(data),
            crate::demo::Next::Packet(_) => {}
            _ => bail!("cannot find data tables in the demo"),
        }
    }
}
