use tf_demo_parser::demo::data::game_state::Player;
use tf_demo_parser::demo::message::voice::VoiceDataMessage;
use tf_demo_parser::demo::parser::analyser::UserInfo;

use super::*;

/// A packet from `client`'s slot saying it is `SteamID` `[U:1:145702602]`: 24 kHz unless
/// `rateless`, one Opus frame of three bytes, a CRC.
fn heard(x: &Extractor, client: u8, rateless: bool) -> Vec<Line> {
    let mut p = vec![0xca, 0x3e, 0xaf, 0x08, 0x01, 0x00, 0x10, 0x01];
    if !rateless {
        p.extend([0x0b, 0xc0, 0x5d]);
    }
    p.extend([0x06, 7, 0, 3, 0, 0, 0, 0x68, 1, 2]);
    p.extend([0; 4]);
    let length = u16::try_from(p.len() * 8).unwrap();
    let data = bitbuffer::BitReadStream::new(bitbuffer::BitReadBuffer::new_owned(p, bitbuffer::LittleEndian));
    x.voice(&VoiceDataMessage { client, proximity: 0, length, data });
    x.drain()
}

/// Players by entity, user id and `SteamID`.
fn with(players: &[(u32, u16, &str)]) -> Extractor {
    let mut x = Extractor::default();
    for &(ent, user, steamid) in players {
        let mut p = Player::default();
        p.entity = EntityId::from(ent);
        p.info = Some(UserInfo {
            classes: tf_demo_parser::demo::parser::analyser::ClassList::default(),
            name: steamid.into(),
            user_id: user.into(),
            steam_id: steamid.into(),
            entity_id: EntityId::from(ent),
            team: Team::Red,
        });
        x.gs.state.players.push(p);
    }
    x
}

fn line(lines: &[Line]) -> &out::Voice {
    let [Line::Voice(v)] = lines else { panic!("{lines:?}") };
    v
}

#[test]
fn a_voice_packet_is_a_voice_line_of_its_speakers_opus_frames() {
    let lines = heard(&with(&[]), 3, false);
    let v = line(&lines);
    assert_eq!((v.steamid.as_str(), v.rate, v.player), ("[U:1:145702602]", 24000, None), "no player has it");
    assert_eq!(v.frames, ["aAEC"], "the frame in base64");
}

#[test]
fn the_speaker_is_whoever_is_in_its_slot_as_the_game_has_it_not_the_steamid_it_claims() {
    let x = with(&[(4, 11, "[U:1:1]"), (6, 12, "[U:1:145702602]")]);
    let lines = heard(&x, 3, false);
    let v = line(&lines);
    assert_eq!((v.player, v.steamid.as_str()), (Some(PlayerId(11)), "[U:1:1]"), "slot 3 is entity 4");
}

#[test]
fn with_nobody_in_its_slot_the_speaker_is_found_by_steamid_but_not_among_those_who_left() {
    let mut x = with(&[(6, 12, "[U:1:145702602]")]);
    let lines = heard(&x, 3, false);
    assert_eq!(line(&lines).player, Some(PlayerId(12)));
    x.left.insert(12);
    let lines = heard(&x, 3, false);
    assert_eq!(line(&lines).player, None, "gone");
}

#[test]
fn a_packet_without_its_sample_rate_is_no_line() {
    assert_eq!(heard(&with(&[]), 3, true), vec![]);
}
