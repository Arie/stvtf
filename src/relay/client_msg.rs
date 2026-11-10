//! The messages a client sends: the few the relay acts on, the rest read past.

use crate::net::bits::{BitBuf, BitReader};
use crate::net::messages::{self, NET_DISCONNECT, NET_SIGNONSTATE, NET_TICK, Signon, TYPE_BITS};
use anyhow::{Result, bail};

/// Longest string read from a client message.
const MAX_STRING: usize = 4096;

// Client message types the relay reads past (`net_*` and `clc_*` in the engine).
const NET_NOP: u32 = 0;
const NET_FILE: u32 = 2;
const NET_STRINGCMD: u32 = 4;
const CLC_MOVE: u32 = 9;
const CLC_VOICEDATA: u32 = 10;
const CLC_BASELINEACK: u32 = 11;
const CLC_LISTENEVENTS: u32 = 12;
const CLC_RESPONDCVARVALUE: u32 = 13;
const CLC_CMDKEYVALUES: u32 = 16;
/// The engine's `MAX_CUSTOM_FILES`: a CRC for each in `clc_ClientInfo`.
const CUSTOM_FILES: usize = 4;
/// `clc_ListenEvents` has a bit per game event, of the engine's `MAX_EVENT_NUMBER`.
const EVENT_BITS: usize = 512;

/// A client's messages the relay acts on.
#[derive(Debug, PartialEq)]
pub(crate) enum ClientMsg {
    Signon(Signon),
    Tick(i32),
    Disconnect(String),
}

/// The messages of one client stream, up to the first it cannot skip.
pub(crate) fn client_messages(bits: &BitBuf) -> Vec<ClientMsg> {
    let mut out = Vec::new();
    let mut r = BitReader::new(bits.bytes());
    let _ = read_client_messages(&mut r, bits.nbits(), &mut out);
    out
}

fn read_client_messages(r: &mut BitReader<'_>, nbits: usize, out: &mut Vec<ClientMsg>) -> Result<()> {
    while nbits.saturating_sub(r.pos()) >= TYPE_BITS {
        match r.read_bits(TYPE_BITS)? {
            NET_NOP => {}
            NET_DISCONNECT => out.push(ClientMsg::Disconnect(r.read_string(MAX_STRING)?)),
            NET_FILE => {
                // Transfer id, file name, requested.
                r.skip(32)?;
                r.read_string(MAX_STRING)?;
                r.skip(1)?;
            }
            NET_TICK => {
                out.push(ClientMsg::Tick(r.read_i32()?));
                r.skip(32)?; // frame time and its deviation
            }
            NET_STRINGCMD => {
                r.read_string(MAX_STRING)?;
            }
            messages::NET_SETCONVAR => {
                for _ in 0..2 * usize::from(r.read_u8()?) {
                    r.read_string(MAX_STRING)?;
                }
            }
            NET_SIGNONSTATE => {
                let state = r.read_u8()?;
                r.skip(32)?; // spawncount
                if let Some(s) = Signon::from_u8(state) {
                    out.push(ClientMsg::Signon(s));
                }
            }
            messages::CLC_CLIENTINFO => {
                r.skip(32 + 32 + 1 + 32)?; // spawncount, CRC, is HLTV, friends id
                r.read_string(MAX_STRING)?;
                for _ in 0..CUSTOM_FILES {
                    if r.read_bit()? {
                        r.skip(32)?; // custom file CRC
                    }
                }
                r.skip(1)?; // is replay
            }
            CLC_MOVE => {
                // New and backup command counts, then the commands.
                r.skip(4 + 3)?;
                let len = r.read_bits(16)? as usize;
                r.skip(len)?;
            }
            CLC_VOICEDATA => {
                let len = r.read_bits(16)? as usize;
                r.skip(len)?;
            }
            CLC_BASELINEACK => r.skip(32 + 1)?,
            CLC_LISTENEVENTS => r.skip(EVENT_BITS)?,
            CLC_RESPONDCVARVALUE => {
                // Cookie, status, name, value.
                r.skip(32 + 4)?;
                r.read_string(MAX_STRING)?;
                r.read_string(MAX_STRING)?;
            }
            CLC_CMDKEYVALUES => {
                let len = r.read_bits(32)? as usize;
                r.skip(len.saturating_mul(8))?;
            }
            id => bail!("client message {id} not understood"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::bits::BitWriter;

    #[test]
    fn client_messages_from_stvtf_parse() {
        let mut w = BitWriter::new();
        w.write_buf(&messages::client_info(7, 0x5FCD_4733, true));
        w.write_buf(&messages::tick(-1));
        w.write_buf(&messages::goodbye());
        assert_eq!(
            client_messages(&w.finish()),
            [ClientMsg::Signon(Signon::New), ClientMsg::Tick(-1), ClientMsg::Disconnect("Disconnect by user.".into())]
        );
        let got = client_messages(&messages::connected_reply("x"));
        assert_eq!(got, [ClientMsg::Signon(Signon::Connected)]);
    }

    /// Every message a game client sends is read past, to the ones the relay acts on after it.
    #[test]
    fn client_messages_a_game_client_sends_are_skipped() {
        let mut w = BitWriter::new();
        let msg = |w: &mut BitWriter, id: u32| w.write_bits(id, TYPE_BITS);
        let string = |w: &mut BitWriter, s: &str| {
            w.write_bytes(s.as_bytes());
            w.write_bits(0, 8);
        };
        msg(&mut w, NET_NOP);
        msg(&mut w, NET_FILE);
        w.write_bits(1, 32);
        string(&mut w, "maps/x.bsp");
        w.write_bit(true);
        msg(&mut w, NET_STRINGCMD);
        string(&mut w, "status");
        msg(&mut w, messages::NET_SETCONVAR);
        w.write_bits(1, 8);
        string(&mut w, "name");
        string(&mut w, "value");
        msg(&mut w, messages::CLC_CLIENTINFO);
        w.write_bits(0, 32);
        w.write_bits(0, 32);
        w.write_bit(true);
        w.write_bits(0, 32);
        string(&mut w, "me");
        for custom in [true, false, true, false] {
            w.write_bit(custom);
            if custom {
                w.write_bits(9, 32);
            }
        }
        w.write_bit(false);
        msg(&mut w, CLC_MOVE);
        w.write_bits(0, 7);
        w.write_bits(12, 16);
        w.write_bits(0, 12);
        msg(&mut w, CLC_VOICEDATA);
        w.write_bits(8, 16);
        w.write_bits(0, 8);
        msg(&mut w, CLC_BASELINEACK);
        w.write_bits(0, 32);
        w.write_bit(false);
        msg(&mut w, CLC_LISTENEVENTS);
        for _ in 0..EVENT_BITS / 32 {
            w.write_bits(0, 32);
        }
        msg(&mut w, CLC_RESPONDCVARVALUE);
        w.write_bits(0, 32);
        w.write_bits(0, 4);
        string(&mut w, "cvar");
        string(&mut w, "1");
        msg(&mut w, CLC_CMDKEYVALUES);
        w.write_bits(1, 32);
        w.write_bits(0, 8);
        w.write_buf(&messages::tick(55));
        assert_eq!(client_messages(&w.finish()), [ClientMsg::Tick(55)]);
    }

    #[test]
    fn client_messages_stop_at_the_first_not_understood() {
        let mut w = BitWriter::new();
        w.write_buf(&messages::tick(1));
        w.write_bits(31, TYPE_BITS);
        w.write_buf(&messages::tick(2));
        assert_eq!(client_messages(&w.finish()), [ClientMsg::Tick(1)]);
    }

    /// A client can claim any length: nothing it says makes the relay read past the packet.
    #[test]
    fn client_messages_with_lengths_past_the_end_stop_there() {
        for id in [CLC_MOVE, CLC_VOICEDATA, CLC_CMDKEYVALUES] {
            let mut w = BitWriter::new();
            w.write_buf(&messages::tick(1));
            w.write_bits(id, TYPE_BITS);
            if id == CLC_MOVE {
                w.write_bits(0, 7);
            }
            w.write_bits(u32::MAX, 16);
            w.write_bits(u32::MAX, 16);
            assert_eq!(client_messages(&w.finish()), [ClientMsg::Tick(1)], "message {id}");
        }
    }
}
