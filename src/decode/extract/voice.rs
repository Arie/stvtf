//! Players' voice (`svc_VoiceData`), Steam's framing taken off: the speaker, the sample rate, and
//! the Opus frames, each as a browser's decoder takes it.
//!
//! A Steam voice packet is the speaker's `SteamID64` (8 bytes, little endian), then chunks, each a
//! type byte: `0x0B` the sample rate (u16), `0x06` Opus with packet loss concealment (a u16
//! length, then frames of a u16 length, a u16 sequence number and the Opus packet; a length of
//! `0xFFFF` resets the decoder), `0x00` silence (u16 samples); and a CRC32 (4 bytes) last.

/// What a Steam voice packet holds.
#[derive(Debug, PartialEq)]
pub(crate) struct Voice {
    pub(crate) steamid: u64,
    pub(crate) rate: u16,
    pub(crate) frames: Vec<Vec<u8>>,
}

const CRC: usize = 4;
const SAMPLE_RATE: u8 = 0x0B;
const OPUS_PLC: u8 = 0x06;
const SILENCE: u8 = 0x00;
const RESET: u16 = 0xFFFF;

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

/// The packet's speaker and Opus frames; none for a packet of another codec, or cut short.
pub(crate) fn steam_voice(data: &[u8]) -> Option<Voice> {
    let steamid = u64::from_le_bytes(data.get(..8)?.try_into().ok()?);
    let end = data.len().checked_sub(CRC)?;
    let (mut at, mut rate, mut frames) = (8, 0, Vec::new());
    while at < end {
        let kind = data[at];
        at += 1;
        match kind {
            SAMPLE_RATE | SILENCE => {
                let v = u16_at(data, at)?;
                if kind == SAMPLE_RATE {
                    rate = v;
                }
                at += 2;
            }
            OPUS_PLC => {
                let len = usize::from(u16_at(data, at)?);
                at += 2;
                let chunk = data.get(at..at + len)?;
                at += len;
                let mut f = 0;
                while f + 2 <= chunk.len() {
                    let n = u16_at(chunk, f)?;
                    f += 2;
                    if n == RESET {
                        continue;
                    }
                    f += 2; // the sequence number
                    let n = usize::from(n);
                    frames.push(chunk.get(f..f + n)?.to_vec());
                    f += n;
                }
            }
            _ => return None,
        }
    }
    (!frames.is_empty()).then_some(Voice { steamid, rate, frames })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_steam_voice_packet_is_its_speaker_rate_and_opus_frames() {
        // As in a serveme demo: a speaker, 24 kHz, two Opus frames, a CRC.
        let mut p = vec![0xca, 0x3e, 0xaf, 0x08, 0x01, 0x00, 0x10, 0x01, 0x0b, 0xc0, 0x5d, 0x06];
        let frames = [vec![0x68, 1, 2], vec![0x68, 3]];
        let mut chunk = Vec::new();
        for (seq, f) in frames.iter().enumerate() {
            chunk.extend((f.len() as u16).to_le_bytes());
            chunk.extend((seq as u16).to_le_bytes());
            chunk.extend(f);
        }
        chunk.extend(RESET.to_le_bytes());
        p.extend((chunk.len() as u16).to_le_bytes());
        p.extend(chunk);
        p.extend([0; 4]);
        let v = steam_voice(&p).unwrap();
        assert_eq!(v.steamid, 0x0110_0001_08af_3eca);
        assert_eq!(v.rate, 24000);
        assert_eq!(v.frames, frames);
    }
}
