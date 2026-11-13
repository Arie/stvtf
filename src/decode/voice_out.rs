//! A player's voice written as the game sends it: `svc_VoiceData` in their slot, Steam's framing
//! round Opus frames (the reverse of `extract::voice`).

use super::Decoder;
use crate::net::bits::{BitBuf, BitWriter};
use crate::net::messages::{SVC_VOICEDATA, TYPE_BITS};

/// Steam's `SteamID64` of account 0.
const STEAMID64: u64 = 76_561_197_960_265_728;
/// Steam's chunk types: the sample rate, and Opus with loss concealment.
const SAMPLE_RATE: u8 = 0x0B;
const OPUS_PLC: u8 = 0x06;
/// In place of a frame's length: the speaker stopped, the next frame is number 0.
const END: u16 = 0xFFFF;
/// What goes before each frame: its length and its number.
const FRAME_HEADER: usize = 4;

/// The most bytes of frames (each with its length and number) in one message: a client takes
/// some four thousand of voice whole, and drops what is longer.
pub(crate) const MAX_CHUNK: usize = 2000;

/// The account of `[U:1:<account>]`.
fn account(steamid: &str) -> Option<u64> {
    steamid.strip_prefix("[U:1:")?.strip_suffix(']')?.parse::<u32>().ok().map(u64::from)
}

/// `frames` in runs that each fit one message, in order; a frame too long for any is left out.
pub(crate) fn packs(frames: &[Vec<u8>]) -> impl Iterator<Item = &[Vec<u8>]> {
    let (mut parts, mut from, mut size) = (Vec::new(), 0, 0);
    for (i, f) in frames.iter().enumerate() {
        let takes = f.len() + FRAME_HEADER;
        if takes > MAX_CHUNK {
            parts.push(&frames[from..i]);
            (from, size) = (i + 1, 0);
        } else if size + takes > MAX_CHUNK {
            parts.push(&frames[from..i]);
            (from, size) = (i, takes);
        } else {
            size += takes;
        }
    }
    parts.push(&frames[from..]);
    parts.into_iter().filter(|p| !p.is_empty())
}

impl Decoder {
    /// `frames` (Opus, 20 ms each) as the voice of the player with user id `user`, or with none
    /// (a caster, a coach: `steamid` theirs, if they have one) in slot `spare`, which nobody is
    /// in, or without one as `SourceTV`'s own, in its bot's slot. Numbered from `seq` on, which
    /// goes on by as many. None for no frames, more than a packet holds, a player not in the match
    /// or without a `SteamID`, or no bot to speak as.
    pub(crate) fn voice_message(
        &self,
        user: Option<u16>,
        spare: Option<u8>,
        steamid: &str,
        rate: u16,
        frames: &[Vec<u8>],
        seq: &mut u16,
    ) -> Option<BitBuf> {
        let (slot, account) = self.voice_slot(user, spare, steamid)?;
        steam_voice(slot, account, rate, frames, seq)
    }

    /// The end of what [`Self::voice_message`] sent for the same speaker.
    pub(crate) fn voice_end(&self, user: Option<u16>, spare: Option<u8>, steamid: &str, rate: u16) -> Option<BitBuf> {
        let (slot, account) = self.voice_slot(user, spare, steamid)?;
        message(slot, account, rate, END.to_le_bytes().to_vec())
    }

    /// The slot and account a voice is sent in.
    fn voice_slot(&self, user: Option<u16>, spare: Option<u8>, steamid: &str) -> Option<(u8, u64)> {
        Some(match (user, spare) {
            (Some(user), _) => {
                let snapshot = self.extractor().snapshot();
                let p = snapshot.players.iter().find(|p| p.id.0 == user)?;
                (u8::try_from(p.ent.checked_sub(1)?).ok()?, account(&p.steamid)?)
            }
            (None, Some(slot)) => (slot, account(steamid).unwrap_or(0)),
            (None, None) => {
                (u8::try_from(self.extractor().sourcetv_entity()?.checked_sub(1)?).ok()?, account(steamid).unwrap_or(0))
            }
        })
    }
}

/// `frames` (Opus, 20 ms each) as the game sends a player's voice: `svc_VoiceData` in `slot`
/// (the player's entity less one), Steam's framing with their account round the frames, numbered
/// from `seq` on, which goes on by as many. None for no frames, or more than a message holds
/// ([`packs`] makes runs that fit): then no numbers are used.
pub(crate) fn steam_voice(slot: u8, account: u64, rate: u16, frames: &[Vec<u8>], seq: &mut u16) -> Option<BitBuf> {
    if frames.is_empty() {
        return None;
    }
    let (mut chunk, mut next) = (Vec::new(), *seq);
    for f in frames {
        chunk.extend(u16::try_from(f.len()).ok()?.to_le_bytes());
        chunk.extend(next.to_le_bytes());
        chunk.extend(f);
        next = next.wrapping_add(1);
    }
    if chunk.len() > MAX_CHUNK {
        return None;
    }
    *seq = next;
    message(slot, account, rate, chunk)
}

/// `svc_VoiceData` in `slot`: Steam's framing round an Opus `chunk`.
fn message(slot: u8, account: u64, rate: u16, chunk: Vec<u8>) -> Option<BitBuf> {
    let mut data = STEAMID64.checked_add(account)?.to_le_bytes().to_vec();
    data.push(SAMPLE_RATE);
    data.extend(rate.to_le_bytes());
    data.push(OPUS_PLC);
    data.extend(u16::try_from(chunk.len()).ok()?.to_le_bytes());
    data.extend(chunk);
    data.extend(crc32fast::hash(&data).to_le_bytes());
    let bits = u16::try_from(data.len() * 8).ok()?;
    let mut w = BitWriter::new();
    w.write_bits(SVC_VOICEDATA, TYPE_BITS);
    w.write_u8(slot);
    w.write_u8(0); // proximity: heard everywhere
    w.write_bits(u32::from(bits), 16);
    w.write_bytes(&data);
    Some(w.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_is_no_more_than_steam_has() {
        assert_eq!(account("[U:1:9]"), Some(9));
        assert_eq!(account("[U:1:4294967296]"), None);
        assert_eq!(account("[U:1:18446744073709551615]"), None);
    }

    #[test]
    fn frames_that_make_no_packet_use_no_numbers() {
        let mut seq = 5;
        assert!(steam_voice(1, 9, 24000, &[vec![1], vec![0; 70_000]], &mut seq).is_none());
        assert_eq!(seq, 5);
        assert!(steam_voice(1, 9, 24000, &[vec![1], vec![2]], &mut seq).is_some());
        assert_eq!(seq, 7);
    }

    #[test]
    fn many_frames_go_in_packets_a_client_takes_whole() {
        let frames: Vec<Vec<u8>> = (0..60u8).map(|i| vec![i; 100]).collect();
        let parts: Vec<&[Vec<u8>]> = packs(&frames).collect();
        assert!(parts.len() > 1);
        assert!(parts.iter().all(|p| p.iter().map(|f| f.len() + FRAME_HEADER).sum::<usize>() <= MAX_CHUNK));
        assert_eq!(parts.concat(), frames, "all of them, in order");
        // One too long for any packet is left out; those around it go.
        let frames = [vec![1], vec![0; MAX_CHUNK], vec![2]];
        assert_eq!(packs(&frames).collect::<Vec<_>>().concat(), [vec![1], vec![2]]);
    }
}
