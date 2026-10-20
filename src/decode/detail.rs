//! Heals, blast jumps and pickups: game events turned into JSON lines.

use super::extract::{strip_colors, team_num};
use serde_json::{Value, json};
use tf_demo_parser::demo::gameevent_gen::GameEvent;
use tf_demo_parser::demo::message::usermessage::UserMessage;
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

fn jump(player: u16, kind: &str, phase: &str) -> Value {
    json!({"type": "blast_jump", "player": player, "kind": kind, "phase": phase})
}

fn pickup(player: u16, item: &str, size: Option<&str>) -> Value {
    json!({"type": "pickup", "player": player, "item": item, "size": size})
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
        GameEvent::RocketJump(j) => Some(jump(j.user_id, "rocket", "start")),
        GameEvent::RocketJumpLanded(j) => Some(jump(j.user_id, "rocket", "land")),
        GameEvent::StickyJump(j) => Some(jump(j.user_id, "sticky", "start")),
        GameEvent::StickyJumpLanded(j) => Some(jump(j.user_id, "sticky", "land")),
        GameEvent::ItemPickup(i) => {
            let item = i.item.to_string();
            let size = ["small", "medium", "large", "full"].into_iter().find(|s| item.ends_with(s));
            let size = size.map(|s| if s == "large" { "full" } else { s });
            if item.starts_with("medkit") {
                Some(pickup(i.user_id, "health", size))
            } else if item.starts_with("ammopack") || item == "tf_ammo_pack" {
                Some(pickup(i.user_id, "ammo", size))
            } else {
                None
            }
        }
        GameEvent::TeamPlayCaptureBroken(b) => {
            Some(json!({"type": "capture_broken", "point": b.cp, "name": b.cp_name.to_string()}))
        }
        // The ids are entity indexes; 0 means no assister.
        GameEvent::KilledCappingPlayer(k) => Some(json!({
            "type": "killed_capper",
            "point": k.cp,
            "killer": (look.user_of_entity)(u32::from(k.killer))?,
            "victim": (look.user_of_entity)(u32::from(k.victim))?,
            "assister": (k.assister != 0).then(|| (look.user_of_entity)(u32::from(k.assister))).flatten(),
        })),
        GameEvent::MedicDefended(m) => Some(json!({"type": "medic_defended", "player": m.user_id, "medic": m.medic})),
        GameEvent::VoteOptions(v) => {
            let all = [&v.option_1, &v.option_2, &v.option_3, &v.option_4, &v.option_5];
            let options: Vec<String> = all.iter().take(usize::from(v.count)).map(ToString::to_string).collect();
            Some(json!({"type": "vote_options", "options": options}))
        }
        GameEvent::VoteCast(v) => Some(json!({
            "type": "vote_cast",
            "player": (look.user_of_entity)(v.entity_id),
            "option": v.vote_option,
            "team": team_num(v.team),
        })),
        GameEvent::TournamentStateUpdate(t) => Some(json!({
            "type": "tournament",
            "player": (t.user_id != 0).then_some(t.user_id),
            "ready": (!t.name_change).then_some(t.ready_state != 0),
        })),
        GameEvent::TeamPlayBroadcastAudio(a) => {
            Some(json!({"type": "announcer", "sound": a.sound.to_string(), "team": team_num(a.team)}))
        }
        GameEvent::TeamPlayMapTimeRemaining(t) => Some(json!({"type": "time_remaining", "seconds": t.seconds})),
        GameEvent::TfMapTimeRemaining(t) => Some(json!({"type": "time_remaining", "seconds": t.seconds})),
        _ => None,
    }
}

pub(crate) fn user_message(message: &UserMessage<'_>, look: &Lookup<'_>) -> Option<Value> {
    match message {
        UserMessage::VoiceSubtitle(v) => Some(json!({
            "type": "voice_command",
            "player": (look.user_of_entity)(u32::from(v.client))?,
            "menu": v.menu,
            "item": v.item,
        })),
        UserMessage::Text(t) => Some(json!({"type": "server_text", "text": strip_colors(t.text.as_ref())})),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tf_demo_parser::demo::message::usermessage::VoiceSubtitleMessage;

    #[test]
    fn voice_commands_name_the_player() {
        let msg = UserMessage::VoiceSubtitle(VoiceSubtitleMessage { client: 5, menu: 0, item: 1 });
        let user = |ent: u32| (ent == 5).then_some(12u16);
        let class = |_| None;
        let look = Lookup { user_of_entity: &user, class_of_user: &class };
        assert_eq!(
            user_message(&msg, &look),
            Some(json!({"type": "voice_command", "player": 12, "menu": 0, "item": 1}))
        );
        let other = UserMessage::VoiceSubtitle(VoiceSubtitleMessage { client: 6, menu: 0, item: 1 });
        assert_eq!(user_message(&other, &look), None);
    }
}
