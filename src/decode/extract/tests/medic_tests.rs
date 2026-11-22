use tf_demo_parser::demo::data::game_state::Player;
use tf_demo_parser::demo::parser::analyser::UserInfo;

use super::*;

fn with_medic(charge: u8, releasing: bool) -> Extractor {
    let mut x = Extractor::default();
    let mut p = Player::default();
    p.entity = EntityId::from(5u32);
    p.class = Class::Medic;
    p.state = PlayerState::Alive;
    p.class_data = PlayerClassData::default_for_class(Class::Medic);
    set_charge(&mut p, charge);
    p.info = Some(UserInfo {
        classes: tf_demo_parser::demo::parser::analyser::ClassList::default(),
        name: "medic".into(),
        user_id: 7u16.into(),
        steam_id: "BOT".into(),
        entity_id: EntityId::from(5u32),
        team: Team::Red,
    });
    x.gs.state.players.push(p);
    x.ents.mediguns.insert(9, Medigun { owner: Some(5), item: 35, releasing, ..Default::default() });
    let mut held = [None; MY_WEAPON_SLOTS];
    held[1] = Some(9);
    x.ents.my_weapons.insert(5, held);
    x
}

fn set_charge(p: &mut Player, to: u8) {
    if let PlayerClassData::Medic { charge, .. } = &mut p.class_data {
        *charge = to;
    }
}

fn step(x: &mut Extractor, charge: u8, releasing: bool) -> Vec<Line> {
    set_charge(&mut x.gs.state.players[0], charge);
    x.ents.mediguns.get_mut(&9).unwrap().releasing = releasing;
    x.medic_changes();
    x.drain()
}

#[test]
fn the_weapon_list_props_are_known() {
    assert!(PROP_MY_WEAPONS.iter().all(|id| id.names().is_some()));
}

#[test]
fn the_medigun_is_the_one_in_the_weapon_list() {
    let mut x = with_medic(0, false);
    x.ents.mediguns.insert(4, Medigun { owner: Some(5), item: 998, ..Default::default() });
    assert_eq!(x.medigun_of(5).map(|m| m.kind), Some(MedigunKind::Kritzkrieg));
    x.ents.my_weapons.get_mut(&5).unwrap()[1] = Some(4);
    assert_eq!(x.medigun_of(5).map(|m| m.kind), Some(MedigunKind::Vaccinator));
}

#[test]
fn ready_once_when_full_and_ended_when_the_charge_runs_out() {
    let mut x = with_medic(90, false);
    let charge = out::MedicCharge { tick: 0, medic: PlayerId(7), medigun: Some(MedigunKind::Kritzkrieg) };
    assert!(step(&mut x, 95, false).is_empty(), "the first sighting only records the state");
    assert_eq!(step(&mut x, 100, false), [Line::UberReady(charge.clone())]);
    assert!(step(&mut x, 100, false).is_empty(), "ready only once");
    assert!(step(&mut x, 98, true).is_empty(), "deploying is the game's own uber event");
    assert_eq!(step(&mut x, 0, false), [Line::UberEnded(charge)]);
}
