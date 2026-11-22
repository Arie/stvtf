use tf_demo_parser::demo::message::usermessage::VoiceSubtitleMessage;

use super::*;

/// Runs `f` with lookups that know players by `user` and nobody's class, at tick 3.
fn with_lookup<R>(user: impl Fn(u32) -> Option<PlayerId>, raw_names: &RawNames, f: impl FnOnce(&Lookup<'_>) -> R) -> R {
    let class = |_| None;
    f(&Lookup { user_of_entity: &user, class_of_user: &class, tick: 3, raw_names })
}

#[test]
fn voice_commands_name_the_player() {
    let msg = UserMessage::VoiceSubtitle(VoiceSubtitleMessage { client: 5, menu: 0, item: 1 });
    let other = UserMessage::VoiceSubtitle(VoiceSubtitleMessage { client: 6, menu: 0, item: 1 });
    with_lookup(
        |ent| (ent == 5).then_some(PlayerId(12)),
        &RawNames::new(),
        |look| {
            assert_eq!(
                user_message(&msg, look),
                Some(Line::VoiceCommand(VoiceCommand { tick: 3, player: PlayerId(12), menu: 0, item: 1 }))
            );
            assert_eq!(user_message(&other, look), None);
        },
    );
}

fn read_raw(kind: u8, bytes: &[u8]) -> Option<Line> {
    let buffer = bitbuffer::BitReadBuffer::new(bytes, LittleEndian);
    with_lookup(
        |ent| (ent == 10).then_some(PlayerId(12)),
        &RawNames::new(),
        |look| raw_user_message(kind, &mut bitbuffer::BitReadStream::new(buffer), look),
    )
}

/// A `dom_canalzone` score counter, as recorded.
const HUD_MSG: &str =
    "02 66 66 26 3f 85 eb 51 3f ff 55 42 00 ff ff ff 00 00 00 00 00 3f 00 00 00 3f 00 00 80 3f 00 00 80 3e 30 00";

fn hex(text: &str) -> Vec<u8> {
    text.split(' ').map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
}

#[test]
fn hud_messages_and_votes_are_read_by_hand() {
    let hud = hex(HUD_MSG);
    assert_eq!(
        read_raw(USER_MESSAGE_HUD_MSG, &hud),
        Some(Line::HudText(HudText {
            tick: 3,
            channel: 2,
            pos: [0.65, 0.82],
            color: ChatColor::Rgb([0xFF, 0x55, 0x42]),
            hold: 1.0,
            text: "0".into(),
        }))
    );
    let mut vote = hex("00 00 00 00 00 0a");
    // A one-bit yes/no flag, then the target's entity (10) in the next 8 bits.
    vote.extend(b"#TF_vote_kick_player_other\0Totally Not A Bot\0");
    vote.extend([0x01 | 10 << 1, 0]);
    assert_eq!(
        read_raw(USER_MESSAGE_VOTE_START, &vote),
        Some(Line::VoteStarted(VoteStarted {
            tick: 3,
            team: crate::output::Team::Unassigned,
            caller: Some(PlayerId(12)),
            issue: "#TF_vote_kick_player_other".into(),
            details: "Totally Not A Bot".into(),
            yes_no: true,
            target: Some(PlayerId(12)),
        }))
    );
    assert_eq!(
        read_raw(USER_MESSAGE_VOTE_FAILED, &hex("02 01 00 00 00 03")),
        Some(Line::VoteFailed(VoteFailed { tick: 3, team: crate::output::Team::Red, reason: 3 }))
    );
    assert_eq!(
        read_raw(USER_MESSAGE_HINT_TEXT, b"Votes: 0/8\0"),
        Some(Line::Hint(Hint { tick: 3, text: "Votes: 0/8".into() }))
    );
}

fn raw(names: &[&str], values: Vec<GameEventValue>) -> Option<Line> {
    let mut raw_names = RawNames::new();
    raw_names.insert("killed_ball_carrier".into(), names.iter().map(|n| field_hash(n)).collect());
    let event = RawGameEvent { event_type: GameEventType::Unknown("killed_ball_carrier".into()), values };
    with_lookup(|ent| (ent < 10).then_some(PlayerId(100 + ent as u16)), &raw_names, |look| raw_event(&event, look))
}

#[test]
fn raw_events_are_read_by_their_field_names() {
    let short = GameEventValue::Short;
    let killed = |attacker, assister| {
        Some(Line::BallCarrierKilled(BallCarrierKilled { tick: 3, attacker, victim: PlayerId(106), assister }))
    };
    let names = ["attacker", "victim", "assister"];
    assert_eq!(raw(&names, vec![short(3), short(6), short(65535)]), killed(Some(PlayerId(103)), None));
    // In whatever order the demo's event list gives them.
    let names = ["victim", "assister", "attacker"];
    assert_eq!(raw(&names, vec![short(6), short(4), short(3)]), killed(Some(PlayerId(103)), Some(PlayerId(104))));
    // A missing or unreadable victim gives no line; a missing attacker is nobody.
    assert_eq!(raw(&["attacker", "victim"], vec![short(3)]), None);
    assert_eq!(raw(&["victim"], vec![GameEventValue::Float(6.0)]), None);
    assert_eq!(raw(&["victim"], vec![short(6)]), killed(None, None));
    assert_eq!(raw(&[], vec![short(3), short(6)]), None);
}

#[test]
fn short_user_messages_give_nothing() {
    let hud = hex(HUD_MSG);
    // Each cut before its last field; a vote failure is 6 bytes in all.
    for (kind, lens) in [
        (USER_MESSAGE_HUD_MSG, &[0, 1, 5, 12, 35][..]),
        (USER_MESSAGE_VOTE_START, &[0, 1, 5, 12][..]),
        (USER_MESSAGE_VOTE_PASS, &[0, 1, 5, 12][..]),
        (USER_MESSAGE_VOTE_FAILED, &[0, 1, 5][..]),
    ] {
        for &len in lens {
            assert_eq!(read_raw(kind, &hud[..len]), None, "message {kind}, {len} bytes");
        }
    }
    assert_eq!(read_raw(USER_MESSAGE_HINT_TEXT, b""), None);
}

#[test]
fn score_methods_of_robot_destruction() {
    assert_eq!(score_method(0), ScoreMethod::ReactorCaptured);
    assert_eq!(score_method(1), ScoreMethod::CoresCollected);
    assert_eq!(score_method(2), ScoreMethod::ReactorReturned);
    assert_eq!(score_method(3), ScoreMethod::ReactorStolen);
    assert_eq!(score_method(4), ScoreMethod::Unknown);
}

/// The line of an event, with entity 10 being player 12 and users as they are.
fn line_of(event: &GameEvent) -> Option<Line> {
    with_lookup(|ent| (ent == 10).then_some(PlayerId(12)), &RawNames::new(), |look| game_event(event, look))
}

#[test]
fn rocket_pack_flights_are_jumps() {
    use tf_demo_parser::demo::gameevent_gen::{RocketPackLandedEvent, RocketPackLaunchEvent};
    let line = |phase| Some(jump(3, 4, JumpKind::RocketPack, phase));
    let launch = GameEvent::RocketPackLaunch(RocketPackLaunchEvent { user_id: 4, play_sound: true });
    assert_eq!(line_of(&launch), line(JumpPhase::Start));
    let landed = GameEvent::RocketPackLanded(RocketPackLandedEvent { user_id: 4 });
    assert_eq!(line_of(&landed), line(JumpPhase::Land));
}

#[test]
fn points_lock_and_unlock() {
    use tf_demo_parser::demo::gameevent_gen::{TeamPlayPointLockedEvent, TeamPlayPointUnlockedEvent};
    let name = || "Second".into();
    let locked = GameEvent::TeamPlayPointLocked(TeamPlayPointLockedEvent { cp: 1, cp_name: name(), team: 2 });
    let unlocked = GameEvent::TeamPlayPointUnlocked(TeamPlayPointUnlockedEvent { cp: 1, cp_name: name(), team: 2 });
    for (event, want) in [(locked, true), (unlocked, false)] {
        let Some(Line::PointLocked(p)) = line_of(&event) else { panic!("{event:?}") };
        assert_eq!((p.tick, p.point, p.name.as_str(), p.locked), (3, 1, "Second", want));
    }
}

#[test]
fn time_remaining_and_detonated_buildings() {
    use tf_demo_parser::demo::gameevent_gen::{ObjectDetonatedEvent, TfMapTimeRemainingEvent};
    let time = GameEvent::TfMapTimeRemaining(TfMapTimeRemainingEvent { seconds: 60 });
    let Some(Line::TimeRemaining(t)) = line_of(&time) else { panic!() };
    assert_eq!((t.tick, t.seconds), (3, 60));
    let gone = GameEvent::ObjectDetonated(ObjectDetonatedEvent { user_id: 4, object_type: 2, index: 9 });
    assert_eq!(
        line_of(&gone),
        Some(Line::BuildingDetonated(ObjectChange {
            tick: 3,
            player: PlayerId(4),
            object: crate::output::BuildingKind::Sentry,
            id: 9
        }))
    );
}

#[test]
fn a_blocked_pass_needs_both_players_known() {
    use tf_demo_parser::demo::gameevent_gen::PassBallBlockedEvent;
    let blocked = |owner, blocker| GameEvent::PassBallBlocked(PassBallBlockedEvent { owner, blocker });
    assert_eq!(
        line_of(&blocked(10, 10)),
        Some(Line::PassBlocked(PassBlocked { tick: 3, owner: PlayerId(12), blocker: PlayerId(12) }))
    );
    assert_eq!(line_of(&blocked(10, 11)), None);
}
