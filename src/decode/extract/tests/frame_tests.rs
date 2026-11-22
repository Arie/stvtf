use tf_demo_parser::demo::data::ServerTick;
use tf_demo_parser::demo::message::packetentities::BaselineIndex;
use tf_demo_parser::demo::packet::datatable::{ClassId, ServerClassName};

use super::*;

fn update(id: u32, update_type: UpdateType) -> PacketEntity {
    PacketEntity {
        server_class: ClassId::from(1u16),
        entity_index: EntityId::from(id),
        props: Vec::new(),
        in_pvs: true,
        update_type,
        serial_number: 0,
        delay: None,
        delta: None,
        baseline_index: BaselineIndex::First,
    }
}

fn frame(x: &mut Extractor, tick: u32, delta: Option<u32>, entities: Vec<PacketEntity>) {
    x.server_tick = tick;
    for e in &entities {
        if e.update_type == UpdateType::Enter {
            let id = e.entity_index;
            x.gs.state.projectiles.entry(id).or_insert_with(|| {
                Projectile::new(id, ClassId::from(1u16), &ServerClassName::from("CTFProjectile_Rocket"))
            });
        }
    }
    let pe = PacketEntitiesMessage {
        entities,
        removed_entities: Vec::new(),
        max_entries: 0,
        delta: delta.map(ServerTick::from),
        base_line: BaselineIndex::First,
        updated_base_line: false,
    };
    x.track_frame(&pe);
}

fn shown(x: &Extractor) -> Vec<u32> {
    let mut ids: Vec<u32> = x.gs.state.projectiles.keys().map(|k| u32::from(*k)).collect();
    ids.sort_unstable();
    ids
}

#[test]
fn a_projectile_that_came_and_went_since_the_base_frame_is_gone() {
    let mut x = Extractor::default();
    frame(&mut x, 100, None, vec![update(10, UpdateType::Enter)]);
    frame(&mut x, 101, Some(100), vec![update(11, UpdateType::Enter)]);
    frame(&mut x, 102, Some(100), vec![update(11, UpdateType::Enter)]);
    assert_eq!(shown(&x), [10, 11]);
    // Fired after frame 100 and exploded: no longer in the deltas from 100, and never deleted.
    frame(&mut x, 103, Some(100), vec![]);
    assert_eq!(shown(&x), [10]);
}

#[test]
fn deletes_and_deltas_from_the_previous_frame() {
    let mut x = Extractor::default();
    frame(&mut x, 100, None, vec![update(10, UpdateType::Enter), update(11, UpdateType::Enter)]);
    frame(&mut x, 101, Some(100), vec![update(10, UpdateType::Delta)]);
    frame(&mut x, 102, Some(101), vec![update(11, UpdateType::Delete)]);
    assert_eq!(shown(&x), [10]);
    frame(&mut x, 103, Some(102), vec![]);
    assert_eq!(shown(&x), [10]);
}

#[test]
fn game_mode_entities_and_flags_that_went_since_the_base_frame_are_gone() {
    let mut x = Extractor::default();
    frame(&mut x, 100, None, vec![]);
    // A robot destruction core and a player destruction bottle, dropped and picked up since
    // 100.
    x.modes.entity("CBonusPack", 12, std::iter::empty());
    x.ents.flags.insert(13, Flag::default());
    frame(&mut x, 101, Some(100), vec![update(12, UpdateType::Delta), update(13, UpdateType::Delta)]);
    assert_eq!(x.modes.objectives(&|_| None).len(), 1);
    frame(&mut x, 102, Some(100), vec![]);
    assert!(x.modes.objectives(&|_| None).is_empty() && x.ents.flags.is_empty());
}

#[test]
fn without_the_base_frame_nothing_is_dropped() {
    let mut x = Extractor::default();
    frame(&mut x, 101, Some(100), vec![update(10, UpdateType::Enter)]);
    frame(&mut x, 102, Some(100), vec![]);
    assert_eq!(shown(&x), [10]);
}

#[test]
fn tracking_picks_up_again_after_a_lost_base_frame() {
    let mut x = Extractor::default();
    frame(&mut x, 100, None, vec![update(10, UpdateType::Enter)]);
    // Frame 101 was lost; 102 deltas from it, 103 from 102.
    frame(&mut x, 102, Some(101), vec![update(11, UpdateType::Enter)]);
    assert_eq!(shown(&x), [10, 11]);
    frame(&mut x, 103, Some(102), vec![update(12, UpdateType::Enter)]);
    // 12 came and went since 102: frame 102 was recorded, so the delta from it drops 12.
    frame(&mut x, 104, Some(102), vec![]);
    assert_eq!(shown(&x), [10, 11]);
}
