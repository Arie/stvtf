//! Heals, blast jumps and pickups: game events turned into JSON lines.

use serde_json::{Value, json};
use tf_demo_parser::demo::gameevent_gen::GameEvent;
use tf_demo_parser::demo::parser::analyser::Class;
use tf_demo_parser::demo::sendprop::{SendProp, SendPropIdentifier, SendPropValue};

/// Lookups the extractor owns: entity index to user id, and a user's current class.
pub(crate) struct Lookup<'a> {
    pub user_of_entity: &'a dyn Fn(u32) -> Option<u16>,
    pub class_of_user: &'a dyn Fn(u16) -> Option<Class>,
}

fn heal(patient: Option<u16>, healer: Option<u16>, amount: i64, source: &str) -> Option<Value> {
    if amount <= 0 {
        return None;
    }
    Some(json!({"type": "heal", "patient": patient?, "healer": healer, "amount": amount, "source": source}))
}

fn jump(player: u16, kind: &str, phase: &str) -> Option<Value> {
    Some(json!({"type": "blast_jump", "player": player, "kind": kind, "phase": phase}))
}

fn pickup(player: u16, item: &str, size: Option<&str>) -> Option<Value> {
    Some(json!({"type": "pickup", "player": player, "item": item, "size": size}))
}

const fn origin_parts(table: &str) -> [SendPropIdentifier; 3] {
    [
        SendPropIdentifier::new(table, "m_vecOrigin[0]"),
        SendPropIdentifier::new(table, "m_vecOrigin[1]"),
        SendPropIdentifier::new(table, "m_vecOrigin[2]"),
    ]
}

const BLOOD_ORIGIN: [SendPropIdentifier; 3] = origin_parts("DT_TETFBlood");
const SPARKS_ORIGIN: [SendPropIdentifier; 3] = origin_parts("DT_TEParticleSystem");
const RICOCHET_POS: SendPropIdentifier = SendPropIdentifier::new("DT_TEMetalSparks", "m_vecPos");
const WORLD_DECAL_ORIGIN: SendPropIdentifier = SendPropIdentifier::new("DT_TEWorldDecal", "m_vecOrigin");
const DECAL_ORIGIN: SendPropIdentifier = SendPropIdentifier::new("DT_TEDecal", "m_vecOrigin");

fn value(props: &[SendProp], id: SendPropIdentifier) -> Option<&SendPropValue> {
    props.iter().find(|p| p.identifier == id).map(|p| &p.value)
}

fn vector(props: &[SendProp], id: SendPropIdentifier) -> Option<[f32; 3]> {
    match value(props, id)? {
        SendPropValue::Vector(v) => Some([v.x, v.y, v.z]),
        _ => None,
    }
}

fn parts(props: &[SendProp], ids: [SendPropIdentifier; 3]) -> Option<[f32; 3]> {
    let mut out = [0.0; 3];
    for (o, id) in out.iter_mut().zip(ids) {
        *o = match value(props, id)? {
            SendPropValue::Float(f) => *f,
            _ => return None,
        };
    }
    Some(out)
}

fn round1(f: f32) -> f64 {
    (f64::from(f) * 10.0).round() / 10.0
}

fn impact(kind: &str, pos: Option<[f32; 3]>) -> Option<Value> {
    Some(json!({"type": "impact", "kind": kind, "pos": pos?.map(round1)}))
}

/// Where bullets landed and what they left behind. `props` already has omitted values filled in.
pub(crate) fn temp_entity(class: &str, props: &[SendProp]) -> Option<Value> {
    match class {
        "CTETFBlood" => impact("blood", parts(props, BLOOD_ORIGIN)),
        "CTESparks" => impact("sparks", parts(props, SPARKS_ORIGIN)),
        "CTEMetalSparks" => impact("sparks", vector(props, RICOCHET_POS)),
        "CTEArmorRicochet" => impact("ricochet", vector(props, RICOCHET_POS)),
        "CTEWorldDecal" => Some(json!({"type": "mark", "pos": vector(props, WORLD_DECAL_ORIGIN)?.map(round1)})),
        "CTEDecal" => Some(json!({"type": "mark", "pos": vector(props, DECAL_ORIGIN)?.map(round1)})),
        _ => None,
    }
}

pub(crate) fn game_event(event: &GameEvent, look: &Lookup<'_>) -> Option<Value> {
    match event {
        GameEvent::PlayerHealed(h) => {
            // The event doesn't say what healed: infer it from the healer's class.
            let source = match (look.class_of_user)(h.healer) {
                Some(Class::Medic) => "medigun",
                Some(Class::Engineer) => "dispenser",
                _ => "other",
            };
            heal(Some(h.patient), (h.healer != 0).then_some(h.healer), i64::from(h.amount), source)
        }
        // `healer` and `target` are entity indexes.
        GameEvent::CrossbowHeal(c) => heal(
            (look.user_of_entity)(u32::from(c.target)),
            (look.user_of_entity)(u32::from(c.healer)),
            i64::from(c.amount),
            "crossbow",
        ),
        GameEvent::PlayerHealOnHit(h) => {
            heal((look.user_of_entity)(u32::from(h.ent_index)), None, i64::from(h.amount), "on_hit")
        }
        GameEvent::RocketJump(j) => jump(j.user_id, "rocket", "start"),
        GameEvent::RocketJumpLanded(j) => jump(j.user_id, "rocket", "land"),
        GameEvent::StickyJump(j) => jump(j.user_id, "sticky", "start"),
        GameEvent::StickyJumpLanded(j) => jump(j.user_id, "sticky", "land"),
        GameEvent::ItemPickup(i) => {
            let item = i.item.to_string();
            let size = ["small", "medium", "large", "full"].into_iter().find(|s| item.ends_with(s));
            let size = size.map(|s| if s == "large" { "full" } else { s });
            if item.starts_with("medkit") {
                pickup(i.user_id, "health", size)
            } else if item.starts_with("ammopack") || item == "tf_ammo_pack" {
                pickup(i.user_id, "ammo", size)
            } else {
                None
            }
        }
        _ => None,
    }
}
