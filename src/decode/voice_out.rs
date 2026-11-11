//! A player's voice written as the game sends it: `svc_VoiceData` in their slot, Steam's framing
//! round Opus frames (the reverse of `extract::voice`).

use super::Decoder;
use crate::net::bits::{BitBuf, BitWriter};
use crate::net::messages::TYPE_BITS;

const SVC_VOICEDATA: u32 = 15;
/// Steam's `SteamID64` of account 0.
const STEAMID64: u64 = 76_561_197_960_265_728;
/// Steam's chunk types: the sample rate, and Opus with loss concealment.
const SAMPLE_RATE: u8 = 0x0B;
const OPUS_PLC: u8 = 0x06;

/// The account of `[U:1:<account>]`.
fn account(steamid: &str) -> Option<u64> {
    steamid.strip_prefix("[U:1:")?.strip_suffix(']')?.parse().ok()
}

impl Decoder {
    /// `frames` (Opus, 20 ms each) as the voice of the player with user id `user`, or with none
    /// (a caster, a coach: `steamid` theirs, if they have one) as `SourceTV`'s own, in its bot's
    /// slot. Numbered from `seq` on, which goes on by as many. None for no frames, more than a
    /// packet holds, a player not in the match or without a `SteamID`, or no bot to speak as.
    pub(crate) fn voice_message(
        &self,
        user: Option<u16>,
        steamid: &str,
        rate: u16,
        frames: &[Vec<u8>],
        seq: &mut u16,
    ) -> Option<BitBuf> {
        let (ent, account) = match user {
            Some(user) => {
                let snapshot = self.extractor().snapshot();
                let p = snapshot.players.iter().find(|p| p.id.0 == user)?;
                (p.ent, account(&p.steamid)?)
            }
            None => (self.extractor().sourcetv_entity()?, account(steamid).unwrap_or(0)),
        };
        let slot = u8::try_from(ent.checked_sub(1)?).ok()?;
        if frames.is_empty() {
            return None;
        }
        let mut chunk = Vec::new();
        for f in frames {
            chunk.extend(u16::try_from(f.len()).ok()?.to_le_bytes());
            chunk.extend(seq.to_le_bytes());
            chunk.extend(f);
            *seq = seq.wrapping_add(1);
        }
        let mut data = (STEAMID64 + account).to_le_bytes().to_vec();
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
}
