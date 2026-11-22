use tf_demo_parser::ParserState;
use tf_demo_parser::demo::data::game_state::{
    PlayerClassData, PlayerCondition, PlayerState, Projectile, ProjectileType,
};
use tf_demo_parser::demo::gameevent_gen::GameEvent;
use tf_demo_parser::demo::message::PacketEntitiesMessage;
use tf_demo_parser::demo::message::packetentities::{EntityId, PacketEntity, UpdateType};
use tf_demo_parser::demo::message::usermessage::{ChatMessageKind, UserMessage};
use tf_demo_parser::demo::parser::analyser::{Class, Team};

use super::convert::{building_kind, medigun_kind, projectile_kind, resist, strip_colors, team_num, *};
use super::events::round_event;
use super::extractor::Extractor;
use super::limits::{
    COND_BITS, COND_WORDS, FLAMETHROWER_FIRING, MY_WEAPON_SLOTS, PROP_MY_WEAPONS, RESIST_BLAST, RESIST_BULLET,
};
use super::state::{Charge, ControlPoint, Flag, Medigun, RoundTimer, Sentry, StopwatchRules, TeamScore};
use crate::DEFAULT_TICK_INTERVAL;
use crate::decode::modes::Placed;
use crate::output::{
    self as out, BuildingKind, ChatColor, ChatKind, ChatPart, Condition, Line, MedigunKind, PlayerId, ProjectileKind,
    Resist, Round, Ubered, round1,
};

fn say_text2(kind: ChatMessageKind, from: &str, text: &str) -> Vec<Line> {
    let mut x = Extractor::default();
    x.user_message(&UserMessage::SayText2(Box::new(tf_demo_parser::demo::message::usermessage::SayText2Message {
        client: EntityId::from(5u32),
        raw: 0,
        kind,
        from: Some(from.into()),
        text: text.into(),
    })));
    x.drain()
}

#[test]
fn a_chat_line_keeps_its_text() {
    let lines = say_text2(ChatMessageKind::ChatAll, "Korra", "xddxdx");
    assert!(matches!(&lines[..], [Line::Chat(c)] if c.text == "xddxdx"), "{lines:?}");
}

// #TF_Halloween_Underworld and the like name a player but have no text; they are not chat.
#[test]
fn a_game_message_without_text_is_no_chat_line() {
    assert_eq!(say_text2(ChatMessageKind::ChatAll, "Korra", ""), vec![]);
}

#[test]
fn strips_chat_colors() {
    assert_eq!(
        strip_colors("\u{1}\u{1}[STV Stats] \u{7}99CCFFBLU\u{1}'s medic DROPPED!\n"),
        "[STV Stats] BLU's medic DROPPED!"
    );
    assert_eq!(strip_colors("\u{8}FF0000FFred"), "red");
    assert_eq!(strip_colors("plain"), "plain");
}

#[test]
fn chat_colour_codes_become_spans() {
    let part = |color, text: &str| ChatPart { color, text: text.to_string() };
    let parts = color_parts("\u{1}\u{1}[STV Stats] \u{7}99ccffBLU\u{1}'s medic \u{3}DROPPED!\n").unwrap();
    assert_eq!(
        parts,
        [
            part(None, "[STV Stats] "),
            part(Some(ChatColor::Rgb([0x99, 0xCC, 0xFF])), "BLU"),
            part(None, "'s medic "),
            part(Some(ChatColor::Team), "DROPPED!")
        ]
    );
    assert!(color_parts("plain text").is_none());
    assert_eq!(color_parts("\u{8}FF0000AAred").unwrap(), [part(Some(ChatColor::Rgb([0xFF, 0, 0])), "red")]);
}

#[test]
fn team_numbers_out_of_range_are_no_team() {
    assert_eq!(team_num(2u8), out::Team::Red);
    assert_eq!(team_num(3u16), out::Team::Blue);
    assert_eq!(team_num(258u16), out::Team::Unassigned, "not truncated to red");
}

#[test]
fn a_reused_entity_slot_forgets_the_medic_charge() {
    let mut x = Extractor::default();
    x.ents.medic_charge.insert(5, Charge { charge: 100, releasing: true });
    x.forget_entity(5);
    assert!(x.ents.medic_charge.is_empty());
}

/// A pyro's firing flamethrower whose slot becomes a medigun with no delete between is gone.
#[test]
fn a_slot_taken_over_by_another_class_keeps_nothing_of_the_old_one() {
    use tf_demo_parser::demo::message::packetentities::BaselineIndex;
    use tf_demo_parser::demo::packet::datatable::{ClassId, ServerClass};
    use tf_demo_parser::demo::sendprop::{SendProp, SendPropValue};

    use crate::decode::props::prop_id;

    let mut state = ParserState::new(24, |_| true, false);
    state.server_classes = ["CTFFlameThrower", "CWeaponMedigun"]
        .iter()
        .zip(0u16..)
        .map(|(name, id)| ServerClass { id: ClassId::from(id), name: (*name).into(), data_table: "".into() })
        .collect();
    let prop =
        |table, name, v| SendProp { index: 0, identifier: prop_id(table, name), value: SendPropValue::Integer(v) };
    let enter = |class: u16| PacketEntity {
        server_class: ClassId::from(class),
        entity_index: EntityId::from(300u32),
        props: vec![
            prop("DT_BaseCombatWeapon", "m_hOwner", 5),
            prop("DT_WeaponFlameThrower", "m_iWeaponState", FLAMETHROWER_FIRING),
        ],
        in_pvs: true,
        update_type: UpdateType::Enter,
        serial_number: 0,
        delay: None,
        delta: None,
        baseline_index: BaselineIndex::First,
    };
    let mut x = Extractor::default();
    x.entity(&enter(0), &state);
    assert!(x.flames_of(5).is_some());
    x.entity(&enter(1), &state);
    assert!(x.flames_of(5).is_none(), "no flames from a medigun");
    assert!(x.ents.mediguns.contains_key(&300));
    x.entity(&enter(0), &state);
    assert!(!x.ents.mediguns.contains_key(&300), "no medigun in a flamethrower's slot");
}

/// Condition words with these condition numbers set, 0-31 in `m_nPlayerCond`.
fn cond_words(conds: impl IntoIterator<Item = usize>) -> [u32; COND_WORDS] {
    let mut w = [0; COND_WORDS];
    for n in conds {
        let word = if n < COND_BITS { 0 } else { n / COND_BITS + 1 };
        w[word] |= 1 << (n % COND_BITS);
    }
    w
}

mod cond_tests;

mod koth_tests;

mod stopwatch_tests;

mod flag_tests;

mod frame_tests;

mod medic_tests;

mod sentry_tests;

mod timer_tests;

mod cap_tests;

mod voice_tests;

#[test]
fn a_set_pause_is_a_pause_line_once_per_change() {
    use tf_demo_parser::demo::message::generated::SetPauseMessage;
    let mut x = Extractor { server_tick: 9, ..Extractor::default() };
    x.set_pause(&SetPauseMessage { pause: false });
    assert_eq!(x.drain(), vec![], "not paused, as it began");
    x.set_pause(&SetPauseMessage { pause: true });
    x.set_pause(&SetPauseMessage { pause: true });
    x.set_pause(&SetPauseMessage { pause: false });
    let lines = x.drain();
    let paused: Vec<bool> = lines.iter().map(|l| matches!(l, Line::Pause(p) if p.paused && p.tick == 9)).collect();
    assert_eq!((lines.len(), paused), (2, vec![true, false]));
}
