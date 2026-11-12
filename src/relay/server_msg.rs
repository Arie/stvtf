//! The server's messages the relay looks into, and those it writes of its own.

use crate::decode::{Lifecycle, Span};
use crate::net::bits::{BitBuf, BitReader, BitWriter};
use crate::net::messages::{
    NET_SIGNONSTATE, SVC_CREATESTRINGTABLE, SVC_SETPAUSE, SVC_UPDATESTRINGTABLE, SVC_USERMESSAGE, Signon, TYPE_BITS,
};

const USERMESSAGE_RESETHUD: u32 = 6;
const USERMESSAGE_VGUIMENU: u32 = 12;

/// The state `msg`, of type `id`, says if it is a `net_SignonState`.
pub(super) fn signon_state(id: u8, msg: &BitBuf) -> Option<Signon> {
    if u32::from(id) != NET_SIGNONSTATE {
        return None;
    }
    BitReader::at(msg.bytes(), TYPE_BITS).read_u8().ok().and_then(Signon::from_u8)
}

pub(super) fn is_table_change(id: u8) -> bool {
    matches!(u32::from(id), SVC_CREATESTRINGTABLE | SVC_UPDATESTRINGTABLE)
}

/// The string table changes of a stream.
pub(super) fn table_changes<'a>(bits: &'a BitBuf, life: &'a Lifecycle) -> impl Iterator<Item = BitBuf> + 'a {
    life.spans.iter().filter(|s| is_table_change(s.id)).map(|s| BitReader::slice(bits.bytes(), s.bits.clone()))
}

/// The user message `ResetHUD` (no. 6, a byte), as a match's own: a game client resets every panel
/// of its HUD.
#[cfg(test)]
pub(super) fn reset_hud() -> BitBuf {
    let mut w = BitWriter::new();
    w.write_bits(SVC_USERMESSAGE, TYPE_BITS);
    w.write_bits(USERMESSAGE_RESETHUD, 8);
    w.write_bits(8, 11); // the length of its data in bits
    w.write_bits(0, 8);
    w.finish()
}

/// `VGUIMenu` user message: name, show byte, key count (0).
pub(super) fn show_panel(name: &str, show: bool) -> BitBuf {
    let mut data: Vec<u8> = name.bytes().chain([0, u8::from(show), 0]).collect();
    let mut w = BitWriter::new();
    w.write_bits(SVC_USERMESSAGE, TYPE_BITS);
    w.write_bits(USERMESSAGE_VGUIMENU, 8);
    w.write_bits(data.len() as u32 * 8, 11);
    w.write_bytes(&std::mem::take(&mut data));
    w.finish()
}

/// Whether the message at `span` is a `ResetHUD`.
pub(crate) fn is_reset_hud(data: &[u8], span: &Span) -> bool {
    u32::from(span.id) == SVC_USERMESSAGE
        && BitReader::at(data, span.bits.start + TYPE_BITS).read_bits(8).is_ok_and(|t| t == USERMESSAGE_RESETHUD)
}

/// `svc_SetPause`.
pub(super) fn set_pause(paused: bool) -> BitBuf {
    let mut w = BitWriter::new();
    w.write_bits(SVC_SETPAUSE, TYPE_BITS);
    w.write_bit(paused);
    w.finish()
}
