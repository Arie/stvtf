//! Outbound client message bodies, as bit streams (bytes, bit count).

use super::bits::BitWriter;

pub const NET_DISCONNECT: u32 = 1;
pub const NET_TICK: u32 = 3;
pub const NET_SETCONVAR: u32 = 5;
pub const NET_SIGNONSTATE: u32 = 6;
pub const CLC_CLIENTINFO: u32 = 8;
const TYPE_BITS: usize = 6;

pub type Bits = (Vec<u8>, usize);

/// Signon states (`SIGNONSTATE_*` in the engine). Ordered, so `>=` compares progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Signon {
    None = 0,
    Challenge = 1,
    Connected = 2,
    New = 3,
    PreSpawn = 4,
    Spawn = 5,
    Full = 6,
    ChangeLevel = 7,
}

impl Signon {
    pub fn name(self) -> &'static str {
        match self {
            Signon::None => "none",
            Signon::Challenge => "challenge",
            Signon::Connected => "connected",
            Signon::New => "new",
            Signon::PreSpawn => "prespawn",
            Signon::Spawn => "spawn",
            Signon::Full => "full",
            Signon::ChangeLevel => "changelevel",
        }
    }
}

fn finish(w: BitWriter) -> Bits {
    let n = w.nbits();
    (w.into_bytes(), n)
}

const USERINFO: &[(&str, &str)] = &[
    ("cl_team", "default"),
    ("cl_class", "default"),
    ("cl_interp_ratio", "2"),
    ("cl_interp", "0.1"),
    ("cl_spec_mode", "1"),
    ("name", ""),
    ("cl_interpolate", "1"),
    ("tv_nochat", "0"),
    ("cl_language", "english"),
    ("rate", "80000"),
    ("cl_cmdrate", "66"),
    ("cl_updaterate", "66"),
    ("net_maxroutable", "1260"),
];

fn write_signonstate(w: &mut BitWriter, state: Signon, spawncount: i32) {
    w.write_bits(NET_SIGNONSTATE, TYPE_BITS);
    w.write_u8(state as u8);
    w.write_i32(spawncount);
}

/// net_SetConVar(userinfo) followed by net_SignonState(CONNECTED, -1).
pub fn connected_reply(name: &str) -> Bits {
    let mut w = BitWriter::new();
    w.write_bits(NET_SETCONVAR, TYPE_BITS);
    w.write_u8(USERINFO.len() as u8);
    for (k, v) in USERINFO {
        w.write_string(k);
        w.write_string(if *k == "name" { name } else { v });
    }
    write_signonstate(&mut w, Signon::Connected, -1);
    finish(w)
}

/// clc_ClientInfo (with the build's SendTable CRC) followed by net_SignonState(NEW).
pub fn client_info(spawncount: i32, sendtable_crc: u32, replay_bit: bool) -> Bits {
    let mut w = BitWriter::new();
    w.write_bits(CLC_CLIENTINFO, TYPE_BITS);
    w.write_i32(spawncount);
    w.write_bits(sendtable_crc, 32);
    w.write_bit(true); // is HLTV
    w.write_i32(0); // friends id
    w.write_string("");
    for _ in 0..4 {
        w.write_bit(false); // no custom files
    }
    if replay_bit {
        w.write_bit(false);
    }
    write_signonstate(&mut w, Signon::New, spawncount);
    finish(w)
}

pub fn signonstate(state: Signon, spawncount: i32) -> Bits {
    let mut w = BitWriter::new();
    write_signonstate(&mut w, state, spawncount);
    finish(w)
}

pub fn disconnect(reason: &str) -> Bits {
    let mut w = BitWriter::new();
    w.write_bits(NET_DISCONNECT, TYPE_BITS);
    w.write_string(reason);
    finish(w)
}

/// Acking the server tick is what moves the server's delta reference forward.
pub fn tick(tick: i32) -> Bits {
    let mut w = BitWriter::new();
    w.write_bits(NET_TICK, TYPE_BITS);
    w.write_i32(tick);
    w.write_bits(0, 16);
    w.write_bits(0, 16);
    finish(w)
}

/// One message stream wrapped as a single-block reliable region.
pub fn reliable_region(body: &Bits, subchannel: u8) -> Bits {
    let nbytes = body.1.div_ceil(8);
    let mut w = BitWriter::new();
    w.write_bits(u32::from(subchannel), 3);
    w.write_bit(true);
    w.write_bit(false);
    w.write_bit(false);
    w.write_varint32(nbytes as u32);
    w.write_bytes(&body.0[..nbytes]);
    w.write_bit(false);
    finish(w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::bits::BitReader;
    use crate::net::reliable::Reassembler;

    #[test]
    fn reliable_region_roundtrips_through_reassembler() {
        let body = signonstate(Signon::Full, 1234);
        let region = reliable_region(&body, 3);
        let parsed = Reassembler::default().feed(&region.0, 0).unwrap();
        assert_eq!(parsed.subchannel, 3);
        let mut r = BitReader::new(&parsed.completed[0]);
        assert_eq!(r.read_bits(6).unwrap(), NET_SIGNONSTATE);
        assert_eq!(r.read_u8().unwrap(), 6);
        assert_eq!(r.read_i32().unwrap(), 1234);
    }

    #[test]
    fn client_info_layout() {
        let (bytes, nbits) = client_info(7, 0x5FCD4733, true);
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(6).unwrap(), CLC_CLIENTINFO);
        assert_eq!(r.read_i32().unwrap(), 7);
        assert_eq!(r.read_bits(32).unwrap(), 0x5FCD4733);
        assert!(r.read_bit().unwrap());
        assert_eq!(r.read_i32().unwrap(), 0);
        assert_eq!(r.read_string(8).unwrap(), "");
        assert_eq!(r.read_bits(5).unwrap(), 0);
        assert_eq!(r.read_bits(6).unwrap(), NET_SIGNONSTATE);
        assert_eq!(r.read_u8().unwrap(), 3);
        assert_eq!(r.read_i32().unwrap(), 7);
        assert_eq!(r.pos, nbits);
    }

    #[test]
    fn connected_reply_carries_name() {
        let (bytes, _) = connected_reply("stvmap");
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(6).unwrap(), NET_SETCONVAR);
        let n = r.read_u8().unwrap();
        let mut found = false;
        for _ in 0..n {
            let k = r.read_string(64).unwrap();
            let v = r.read_string(64).unwrap();
            if k == "name" {
                found = v == "stvmap";
            }
        }
        assert!(found);
        assert_eq!(r.read_bits(6).unwrap(), NET_SIGNONSTATE);
        assert_eq!(r.read_u8().unwrap(), 2);
    }
}
