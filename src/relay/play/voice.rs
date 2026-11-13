//! Voice from outside the match (a team's Discord, through stv.tf), sent to the clients in the game
//! as a player's own: `svc_VoiceData` in their slot, in Steam's framing, as it comes.

use super::player::Player;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The most voice waiting to be sent; past it the oldest goes.
pub(super) const MAX_QUEUED: usize = 512;
/// The most speakers whose frame numbers are kept: more than a server has slots.
const MAX_SPEAKERS: usize = 256;

/// Opus frames (20 ms each, in order) to send as the voice of the player with user id `user`, or
/// with none (a caster, a coach) in a slot nobody is in; on a full server as `SourceTV`'s own,
/// one such voice at a time.
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

/// Whose voice a packet is, to number its frames: a client orders a speaker's frames by them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Slot {
    /// A player, by user id.
    Player(u16),
    /// Someone who is no player, in a slot nobody is in.
    Spare(u8),
    /// The same, in `SourceTV`'s own: a full server has no other.
    SourceTv,
}

/// The next frame number of each speaker, and where those who are no player talk.
#[derive(Default)]
pub(super) struct Sequences {
    next: HashMap<Slot, u16>,
    /// The slot nobody is in that each of them has, and when they last talked in it.
    spare: HashMap<String, (u8, Instant)>,
    /// Who has `SourceTV`'s slot, and when they last talked.
    sourcetv: Option<(String, Instant)>,
    /// Those being sent, until they stop.
    talking: HashMap<Slot, Talking>,
}

/// A speaker being sent: as whom, and when last.
struct Talking {
    last: Instant,
    user: Option<u16>,
    spare: Option<u8>,
    steamid: String,
    rate: u16,
}

/// How long without frames ends a speaker's burst: posts come 100 ms apart.
const STOPPED: Duration = Duration::from_millis(300);

/// How long `SourceTV`'s slot stays its last speaker's after their last frame, and a slot nobody
/// is in.
const SLOT_HELD: Duration = Duration::from_millis(400);
const SPARE_HELD: Duration = Duration::from_secs(30);

impl Sequences {
    /// `who` was sent in `slot`, numbered up to `seq`: ended once they stop. Their numbering is
    /// kept for at most [`MAX_SPEAKERS`].
    fn record(&mut self, slot: Slot, seq: u16, who: Talking) {
        if self.next.len() < MAX_SPEAKERS || self.next.contains_key(&slot) {
            self.next.insert(slot, seq);
        }
        self.talking.insert(slot, who);
    }

    /// Takes those who stopped, once each; their numbering starts over.
    fn take_stopped(&mut self, now: Instant) -> Vec<Talking> {
        let gone: Vec<Slot> = self
            .talking
            .iter()
            .filter(|(_, t)| now.saturating_duration_since(t.last) >= STOPPED)
            .map(|(s, _)| *s)
            .collect();
        self.next.retain(|s, _| !gone.contains(s));
        gone.iter().filter_map(|s| self.talking.remove(s)).collect()
    }

    /// Claims a slot for `steamid`, who is no player, of those nobody is in now (`free`): theirs while
    /// it stays free, else the first no other voice has. A client plays a voice in such a slot
    /// as of someone unconnected. None when there is none left.
    pub(super) fn claim_spare(&mut self, steamid: &str, free: &[u8], now: Instant) -> Option<u8> {
        self.spare.retain(|_, (slot, last)| free.contains(slot) && now.saturating_duration_since(*last) < SPARE_HELD);
        let slot = match self.spare.get(steamid) {
            Some((slot, _)) => *slot,
            None => *free.iter().find(|f| !self.spare.values().any(|(s, _)| s == *f))?,
        };
        self.spare.insert(steamid.to_string(), (slot, now));
        Some(slot)
    }

    /// Claims `SourceTV`'s slot for `steamid`, who is no player; whether they have it. It is one
    /// voice's at a time (two in one slot are noise), the first to talk until they fall silent.
    pub(super) fn claim_sourcetv(&mut self, steamid: &str, now: Instant) -> bool {
        if let Some((holder, last)) = &self.sourcetv
            && holder != steamid
            && now.saturating_duration_since(*last) < SLOT_HELD
        {
            return false;
        }
        self.sourcetv = Some((steamid.to_string(), now));
        true
    }
}

impl Player<'_> {
    /// Sends `voice` to the clients in the game, unreliably, as the game sends voice; a speaker
    /// not in the match (yet, or any more) is passed over.
    pub(super) fn send_voice(&mut self, voice: &[VoiceIn]) {
        let Some(c) = self.cur.as_ref().filter(|c| c.started) else { return };
        let mut hub = self.relay.hub();
        let now = Instant::now();
        for v in voice {
            let (spare, slot) = match v.user {
                Some(user) => (None, Slot::Player(user)),
                None => match self.voice_seq.claim_spare(&v.steamid, &c.dec.extractor().spare_slots(), now) {
                    Some(slot) => (Some(slot), Slot::Spare(slot)),
                    None if self.voice_seq.claim_sourcetv(&v.steamid, now) => (None, Slot::SourceTv),
                    None => continue,
                },
            };
            // Numbered from where the speaker's last left off; kept only for one who was sent.
            let mut seq = self.voice_seq.next.get(&slot).copied().unwrap_or(0);
            let mut sent = false;
            for part in crate::decode::voice_runs(&v.frames) {
                if let Some(m) = c.dec.voice_message(v.user, spare, &v.steamid, v.rate, part, &mut seq) {
                    hub.send_to_active(&m);
                    sent = true;
                }
            }
            if sent {
                let who = Talking { last: now, user: v.user, spare, steamid: v.steamid.clone(), rate: v.rate };
                self.voice_seq.record(slot, seq, who);
            }
        }
        // As the game ends a voice: without it a client's decoder is left mid-speech.
        for t in self.voice_seq.take_stopped(now) {
            if let Some(m) = c.dec.voice_end(t.user, t.spare, &t.steamid, t.rate) {
                hub.send_to_active(&m);
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
        assert!(seqs.claim_sourcetv("caster", now));
        assert!(seqs.claim_sourcetv("caster", now + Duration::from_millis(100)));
        assert!(!seqs.claim_sourcetv("other", now + Duration::from_millis(200)), "two voices in one slot are noise");
        assert!(seqs.claim_sourcetv("caster", now + Duration::from_millis(300)));
        assert!(seqs.claim_sourcetv("other", now + Duration::from_millis(700)), "once the first fell silent");
    }

    #[test]
    fn those_who_are_no_player_each_have_a_slot_nobody_is_in() {
        use std::time::{Duration, Instant};
        let (mut seqs, now) = (super::Sequences::default(), Instant::now());
        assert_eq!(seqs.claim_spare("caster", &[23, 22, 21], now), Some(23));
        assert_eq!(seqs.claim_spare("other", &[23, 22, 21], now), Some(22), "talking at once, each their own");
        assert_eq!(seqs.claim_spare("caster", &[23, 22, 21], now), Some(23), "and they keep it");
        assert_eq!(seqs.claim_spare("caster", &[22, 21], now), Some(21), "a player came into theirs: another");
        assert_eq!(seqs.claim_spare("third", &[22, 21], now), None, "none left over");
        let later = now + Duration::from_secs(60);
        assert_eq!(seqs.claim_spare("caster", &[22, 21], later), Some(22), "one long silent is free again");
        assert_eq!(seqs.claim_spare("third", &[], later), None, "a full server has none");
    }

    #[test]
    fn a_speaker_who_is_no_player_talks_in_a_slot_nobody_is_in() {
        let c = upward_until(|c| c.dec.extractor().snapshot().players.iter().any(|p| p.alive));
        let spare = c.dec.extractor().spare_slots();
        let taken: Vec<u32> = c.dec.extractor().snapshot().players.iter().map(|p| p.ent).collect();
        assert!(
            !spare.is_empty() && spare.iter().all(|s| !taken.contains(&(u32::from(*s) + 1))),
            "{spare:?} {taken:?}"
        );
        assert!(spare.is_sorted_by(|a, b| a > b), "from the top, away from where players come in");
        let msg = c.dec.voice_message(None, Some(spare[0]), "[U:1:77]", 24000, &[vec![0x68]], &mut 0).unwrap();
        let (client, data) = voice_data(&c.dec, &msg);
        assert_eq!(client, spare[0]);
        assert_eq!(crate::decode::steam_voice_for_tests(&data).unwrap().0 & 0xFFFF_FFFF, 77);
    }

    #[test]
    fn a_voice_packet_reads_back_as_the_games_own() {
        let c = upward_until(|c| c.dec.extractor().snapshot().players.iter().any(|p| p.alive));
        let p = c.dec.extractor().snapshot().players.iter().find(|p| p.alive).cloned().unwrap();
        let frames = vec![vec![0x68, 1, 2], vec![0x68, 3]];
        let mut seq = 0;
        let msg = c.dec.voice_message(Some(p.id.0), None, "", 24000, &frames, &mut seq).unwrap();
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
        let first = c.dec.voice_message(Some(user), None, "", 24000, &[vec![1], vec![2]], &mut seq).unwrap();
        let second = c.dec.voice_message(Some(user), None, "", 24000, &[vec![3]], &mut seq).unwrap();
        assert_eq!(numbers(&voice_data(&c.dec, &first).1), [0, 1]);
        assert_eq!(numbers(&voice_data(&c.dec, &second).1), [2]);
    }

    #[test]
    fn a_speaker_who_stopped_is_ended_and_numbered_from_nought_again() {
        use std::time::{Duration, Instant};
        let (mut seqs, now) = (super::Sequences::default(), Instant::now());
        let slot = super::Slot::Player(7);
        seqs.record(
            slot,
            5,
            super::Talking { last: now, user: Some(7), spare: None, steamid: String::new(), rate: 24000 },
        );
        assert!(seqs.take_stopped(now + Duration::from_millis(150)).is_empty(), "between two posts");
        assert_eq!(seqs.next.get(&slot), Some(&5));
        let ended = seqs.take_stopped(now + Duration::from_millis(400));
        assert_eq!(ended.iter().map(|t| t.user).collect::<Vec<_>>(), [Some(7)]);
        assert_eq!(seqs.next.get(&slot), None);
        assert!(seqs.take_stopped(now + Duration::from_secs(1)).is_empty(), "ended once");
    }

    #[test]
    fn a_speaker_past_those_numbered_is_still_ended() {
        use std::time::{Duration, Instant};
        let (mut seqs, now) = (super::Sequences::default(), Instant::now());
        let talking =
            |user| super::Talking { last: now, user: Some(user), spare: None, steamid: String::new(), rate: 24000 };
        for user in 0..super::MAX_SPEAKERS as u16 {
            seqs.record(super::Slot::Player(user), 1, talking(user));
        }
        let past = super::MAX_SPEAKERS as u16;
        seqs.record(super::Slot::Player(past), 1, talking(past));
        assert_eq!(seqs.next.len(), super::MAX_SPEAKERS);
        let ended = seqs.take_stopped(now + Duration::from_secs(1));
        assert!(ended.iter().any(|t| t.user == Some(past)), "its end goes too");
        assert_eq!(ended.len(), super::MAX_SPEAKERS + 1);
    }

    #[test]
    fn the_end_of_a_voice_is_steams_marker_alone() {
        let c = upward_until(|c| c.dec.extractor().snapshot().players.iter().any(|p| p.alive));
        let p = c.dec.extractor().snapshot().players.iter().find(|p| p.alive).cloned().unwrap();
        let msg = c.dec.voice_end(Some(p.id.0), None, "", 24000).unwrap();
        let (client, data) = voice_data(&c.dec, &msg);
        assert_eq!(u32::from(client) + 1, p.ent);
        // SteamID (8), rate chunk (3), then the Opus chunk: type, length 2, the marker.
        assert_eq!(data[11..data.len() - 4], [0x06, 2, 0, 0xFF, 0xFF]);
        let (body, crc) = data.split_at(data.len() - 4);
        assert_eq!(crc, crc32fast::hash(body).to_le_bytes());
    }

    #[test]
    fn a_speaker_who_is_no_player_talks_as_sourcetv_itself() {
        let c = upward_until(|c| c.dec.extractor().snapshot().players.iter().any(|p| p.alive));
        let players: Vec<u32> = c.dec.extractor().snapshot().players.iter().map(|p| p.ent).collect();
        let msg = c.dec.voice_message(None, None, "[U:1:77]", 24000, &[vec![0x68]], &mut 0).expect("the bot's slot");
        let (client, data) = voice_data(&c.dec, &msg);
        assert!(!players.contains(&(u32::from(client) + 1)), "no player's slot: {client}");
        assert_eq!(crate::decode::steam_voice_for_tests(&data).unwrap().0 & 0xFFFF_FFFF, 77, "their own SteamID");
    }

    #[test]
    fn a_player_not_in_the_match_gets_no_packet() {
        let c = upward_until(|c| c.started);
        assert!(c.dec.voice_message(Some(u16::MAX), None, "", 24000, &[vec![1]], &mut 0).is_none());
        let user = c.dec.extractor().snapshot().players.first().map_or(0, |p| p.id.0);
        assert!(c.dec.voice_message(Some(user), None, "", 24000, &[], &mut 0).is_none(), "nor one without frames");
    }
}
