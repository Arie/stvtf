//! Voice from outside the match (a team's Discord, through stv.tf), sent to the clients in the game
//! as a player's own: `svc_VoiceData` in their slot, in Steam's framing, as it comes.

use super::Player;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The most voice waiting to be sent; past it the oldest goes.
pub(super) const MAX_QUEUED: usize = 512;

/// Opus frames (20 ms each, in order) to send as the voice of the player with user id `user`, or
/// with none (a caster, a coach) as `SourceTV`'s own, one such voice at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceIn {
    /// The player's user id; none for a speaker who is no player.
    pub user: Option<u16>,
    /// The speaker's `SteamID` (`[U:1:…]`), for one who is no player; may be empty.
    pub steamid: String,
    /// Samples a second the frames are labelled with.
    pub rate: u16,
    /// The Opus packets.
    pub frames: Vec<Vec<u8>>,
}

/// The next frame number of each speaker, by user id: a client orders a speaker's frames by it.
#[derive(Default)]
pub(super) struct Sequences(HashMap<Option<u16>, u16>, Option<(String, Instant)>);

/// How long `SourceTV`'s slot stays its last speaker's after their last frame.
const SLOT_HELD: Duration = Duration::from_millis(400);

impl Sequences {
    /// Whether `steamid`, who is no player, has `SourceTV`'s slot now: it is one voice's at a
    /// time (two in one slot are noise), the first to talk until they fall silent.
    pub(super) fn takes_slot(&mut self, steamid: &str, now: Instant) -> bool {
        if let Some((holder, last)) = &self.1
            && holder != steamid
            && now.saturating_duration_since(*last) < SLOT_HELD
        {
            return false;
        }
        self.1 = Some((steamid.to_string(), now));
        true
    }
}

impl Player<'_> {
    /// Sends `voice` to the clients in the game, unreliably, as the game sends voice; a speaker
    /// not in the match (yet, or any more) is passed over.
    pub(super) fn send_voice(&mut self, voice: &[VoiceIn]) {
        let Some(c) = self.cur.as_ref().filter(|c| c.started) else { return };
        let mut hub = self.relay.hub();
        for v in voice {
            if v.user.is_none() && !self.voice_seq.takes_slot(&v.steamid, Instant::now()) {
                continue;
            }
            let seq = self.voice_seq.0.entry(v.user).or_insert(0);
            if let Some(m) = c.dec.voice_message(v.user, &v.steamid, v.rate, &v.frames, seq) {
                hub.tell_active_now(&m);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::upward_until;
    use tf_demo_parser::demo::message::Message;

    fn voice_data(dec: &crate::decode::Decoder, msg: &crate::net::bits::BitBuf) -> (u8, Vec<u8>) {
        let state = dec.parser_state_for_tests();
        let mut s = bitbuffer::BitReadStream::new(bitbuffer::BitReadBuffer::new(msg.bytes(), bitbuffer::LittleEndian));
        let t: tf_demo_parser::demo::message::MessageType = bitbuffer::BitRead::read(&mut s).unwrap();
        let Message::VoiceData(v) = Message::from_type(t, &mut s, state).unwrap() else { panic!("not voice") };
        let mut d = v.data.clone();
        (v.client, (0..usize::from(v.length) / 8).map(|_| d.read_int::<u8>(8).unwrap()).collect())
    }

    #[test]
    fn those_who_are_no_player_have_sourcetvs_slot_one_at_a_time() {
        use std::time::{Duration, Instant};
        let (mut seqs, now) = (super::Sequences::default(), Instant::now());
        assert!(seqs.takes_slot("caster", now));
        assert!(seqs.takes_slot("caster", now + Duration::from_millis(100)));
        assert!(!seqs.takes_slot("other", now + Duration::from_millis(200)), "two voices in one slot are noise");
        assert!(seqs.takes_slot("caster", now + Duration::from_millis(300)));
        assert!(seqs.takes_slot("other", now + Duration::from_millis(700)), "once the first fell silent");
    }

    #[test]
    fn a_voice_packet_reads_back_as_the_games_own() {
        let c = upward_until(|c| c.dec.extractor().snapshot().players.iter().any(|p| p.alive));
        let p = c.dec.extractor().snapshot().players.iter().find(|p| p.alive).cloned().unwrap();
        let frames = vec![vec![0x68, 1, 2], vec![0x68, 3]];
        let mut seq = 0;
        let msg = c.dec.voice_message(Some(p.id.0), "", 24000, &frames, &mut seq).unwrap();
        let (client, data) = voice_data(&c.dec, &msg);
        assert_eq!(u32::from(client) + 1, p.ent, "in the player's slot");
        let (steamid, rate, read) = crate::decode::steam_voice_for_tests(&data).unwrap();
        assert_eq!(format!("[U:1:{}]", steamid & 0xFFFF_FFFF), p.steamid);
        assert_eq!((rate, read), (24000, frames));
        let (body, crc) = data.split_at(data.len() - 4);
        assert_eq!(crc, crc32fast::hash(body).to_le_bytes(), "Steam's checksum");
        assert_eq!(seq, 2);
    }

    #[test]
    fn frames_are_numbered_on_from_the_last_packet() {
        let c = upward_until(|c| c.dec.extractor().snapshot().players.iter().any(|p| p.alive));
        let user = c.dec.extractor().snapshot().players.iter().find(|p| p.alive).unwrap().id.0;
        let mut seq = 0;
        let numbers = |data: &[u8]| {
            // SteamID (8), rate chunk (3), Opus chunk's type and length (3), then its frames.
            let (mut at, mut out) = (14, Vec::new());
            while at + 4 <= data.len() - 4 {
                let len = usize::from(u16::from_le_bytes([data[at], data[at + 1]]));
                out.push(u16::from_le_bytes([data[at + 2], data[at + 3]]));
                at += 4 + len;
            }
            out
        };
        let first = c.dec.voice_message(Some(user), "", 24000, &[vec![1], vec![2]], &mut seq).unwrap();
        let second = c.dec.voice_message(Some(user), "", 24000, &[vec![3]], &mut seq).unwrap();
        assert_eq!(numbers(&voice_data(&c.dec, &first).1), [0, 1]);
        assert_eq!(numbers(&voice_data(&c.dec, &second).1), [2]);
    }

    #[test]
    fn a_speaker_who_is_no_player_talks_as_sourcetv_itself() {
        let c = upward_until(|c| c.dec.extractor().snapshot().players.iter().any(|p| p.alive));
        let players: Vec<u32> = c.dec.extractor().snapshot().players.iter().map(|p| p.ent).collect();
        let msg = c.dec.voice_message(None, "[U:1:77]", 24000, &[vec![0x68]], &mut 0).expect("the bot's slot");
        let (client, data) = voice_data(&c.dec, &msg);
        assert!(!players.contains(&(u32::from(client) + 1)), "no player's slot: {client}");
        assert_eq!(crate::decode::steam_voice_for_tests(&data).unwrap().0 & 0xFFFF_FFFF, 77, "their own SteamID");
    }

    #[test]
    fn a_player_not_in_the_match_gets_no_packet() {
        let c = upward_until(|c| c.started);
        assert!(c.dec.voice_message(Some(u16::MAX), "", 24000, &[vec![1]], &mut 0).is_none());
        let user = c.dec.extractor().snapshot().players.first().map_or(0, |p| p.id.0);
        assert!(c.dec.voice_message(Some(user), "", 24000, &[], &mut 0).is_none(), "nor one without frames");
    }
}
