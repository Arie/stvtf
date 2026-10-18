pub mod capture;
pub mod decode;
pub mod net;
pub mod session;
pub mod watch;

/// The TF2 server build whose tables and CRC are compiled in.
pub const CURRENT_BUILD: &str = "11138802";

pub struct Build {
    /// `g_SendTableCRC` of the server's engine.
    pub crc: u32,
    /// A demo of that build cut off after its data tables.
    pub tables: &'static [u8],
}

/// Compiled-in data for known TF2 server builds (A2S_INFO `version`).
pub fn builtin(build: &str) -> Option<Build> {
    match build {
        "11138802" => Some(Build { crc: 0x5FCD4733, tables: include_bytes!("../assets/tables_11138802.dem") }),
        _ => None,
    }
}
