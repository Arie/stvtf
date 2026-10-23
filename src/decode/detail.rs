//! Heals, blast jumps and pickups: game events turned into lines.

use super::extract::{round1, strip_colors, team_num};
use crate::output::{
    Announcer, BlastJump, CaptureBroken, Heal, HealSource, Impact, ImpactKind, JumpKind, JumpPhase, KilledCapper, Line,
    Mark, MedicDefended, Pickup, PickupItem, PickupSize, PlayerId, ServerText, TimeRemaining, Tournament, VoiceCommand,
    VoteCast, VoteOptions,
};
use tf_demo_parser::demo::gameevent_gen::GameEvent;
use tf_demo_parser::demo::message::usermessage::UserMessage;
use tf_demo_parser::demo::parser::analyser::Class;
use tf_demo_parser::demo::sendprop::{SendProp, SendPropIdentifier, SendPropValue};

/// Lookups the extractor owns: entity index to user id, a user's current class, and the tick.
pub(crate) struct Lookup<'a> {
    pub user_of_entity: &'a dyn Fn(u32) -> Option<PlayerId>,
    pub class_of_user: &'a dyn Fn(u16) -> Option<Class>,
    pub tick: u32,
}

fn heal(
    tick: u32,
    patient: Option<PlayerId>,
    healer: Option<PlayerId>,
    amount: i64,
    source: HealSource,
) -> Option<Line> {
    if amount <= 0 {
        return None;
    }
    Some(Line::Heal(Heal { tick, patient: patient?, healer, amount, source }))
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

fn impact(tick: u32, kind: ImpactKind, pos: Option<[f32; 3]>) -> Option<Line> {
    Some(Line::Impact(Impact { tick, kind, pos: pos?.map(round1) }))
}

fn mark(tick: u32, pos: Option<[f32; 3]>) -> Option<Line> {
    Some(Line::Mark(Mark { tick, pos: pos?.map(round1) }))
}

/// Where bullets landed and what they left behind. `props` already has omitted values filled in.
pub(crate) fn temp_entity(class: &str, props: &[SendProp], tick: u32) -> Option<Line> {
    match class {
        "CTETFBlood" => impact(tick, ImpactKind::Blood, parts(props, BLOOD_ORIGIN)),
        "CTESparks" => impact(tick, ImpactKind::Sparks, parts(props, SPARKS_ORIGIN)),
        "CTEMetalSparks" => impact(tick, ImpactKind::Sparks, vector(props, RICOCHET_POS)),
        "CTEArmorRicochet" => impact(tick, ImpactKind::Ricochet, vector(props, RICOCHET_POS)),
        "CTEWorldDecal" => mark(tick, vector(props, WORLD_DECAL_ORIGIN)),
        "CTEDecal" => mark(tick, vector(props, DECAL_ORIGIN)),
        _ => None,
    }
}

fn pickup(item: &str) -> Option<(PickupItem, Option<PickupSize>)> {
    let size = if item.ends_with("small") {
        Some(PickupSize::Small)
    } else if item.ends_with("medium") {
        Some(PickupSize::Medium)
    } else if item.ends_with("large") || item.ends_with("full") {
        Some(PickupSize::Full)
    } else {
        None
    };
    if item.starts_with("medkit") {
        Some((PickupItem::Health, size))
    } else if item.starts_with("ammopack") || item == "tf_ammo_pack" {
        Some((PickupItem::Ammo, size))
    } else {
        None
    }
}

pub(crate) fn game_event(event: &GameEvent, look: &Lookup<'_>) -> Option<Line> {
    let tick = look.tick;
    let user = |ent: u8| (look.user_of_entity)(u32::from(ent));
    match event {
        GameEvent::PlayerHealed(h) => {
            // The event doesn't say what healed: infer it from the healer's class.
            let source = match (look.class_of_user)(h.healer) {
                Some(Class::Medic) => HealSource::Medigun,
                Some(Class::Engineer) => HealSource::Dispenser,
                _ => HealSource::Other,
            };
            let healer = (h.healer != 0).then_some(PlayerId(h.healer));
            heal(tick, Some(PlayerId(h.patient)), healer, i64::from(h.amount), source)
        }
        // `healer` and `target` are entity indexes.
        GameEvent::CrossbowHeal(c) => {
            heal(tick, user(c.target), user(c.healer), i64::from(c.amount), HealSource::Crossbow)
        }
        GameEvent::PlayerHealOnHit(h) => heal(tick, user(h.ent_index), None, i64::from(h.amount), HealSource::OnHit),
        GameEvent::RocketJump(j) => Some(jump(tick, j.user_id, JumpKind::Rocket, JumpPhase::Start)),
        GameEvent::RocketJumpLanded(j) => Some(jump(tick, j.user_id, JumpKind::Rocket, JumpPhase::Land)),
        GameEvent::StickyJump(j) => Some(jump(tick, j.user_id, JumpKind::Sticky, JumpPhase::Start)),
        GameEvent::StickyJumpLanded(j) => Some(jump(tick, j.user_id, JumpKind::Sticky, JumpPhase::Land)),
        GameEvent::ItemPickup(i) => {
            let (item, size) = pickup(i.item.as_ref())?;
            Some(Line::Pickup(Pickup { tick, player: PlayerId(i.user_id), item, size }))
        }
        GameEvent::TeamPlayCaptureBroken(b) => {
            Some(Line::CaptureBroken(CaptureBroken { tick, point: b.cp, name: b.cp_name.to_string() }))
        }
        // The ids are entity indexes; 0 means no assister.
        GameEvent::KilledCappingPlayer(k) => Some(Line::KilledCapper(KilledCapper {
            tick,
            point: k.cp,
            killer: user(k.killer)?,
            victim: user(k.victim)?,
            assister: (k.assister != 0).then(|| user(k.assister)).flatten(),
        })),
        GameEvent::MedicDefended(m) => {
            Some(Line::MedicDefended(MedicDefended { tick, player: PlayerId(m.user_id), medic: PlayerId(m.medic) }))
        }
        GameEvent::VoteOptions(v) => {
            let all = [&v.option_1, &v.option_2, &v.option_3, &v.option_4, &v.option_5];
            let options = all.iter().take(usize::from(v.count)).map(ToString::to_string).collect();
            Some(Line::VoteOptions(VoteOptions { tick, options }))
        }
        GameEvent::VoteCast(v) => Some(Line::VoteCast(VoteCast {
            tick,
            player: (look.user_of_entity)(v.entity_id),
            option: v.vote_option,
            team: team_num(v.team),
        })),
        GameEvent::TournamentStateUpdate(t) => Some(Line::Tournament(Tournament {
            tick,
            player: (t.user_id != 0).then_some(PlayerId(t.user_id)),
            ready: (!t.name_change).then_some(t.ready_state != 0),
        })),
        GameEvent::TeamPlayBroadcastAudio(a) => {
            Some(Line::Announcer(Announcer { tick, sound: a.sound.to_string(), team: team_num(a.team) }))
        }
        GameEvent::TeamPlayMapTimeRemaining(t) => {
            Some(Line::TimeRemaining(TimeRemaining { tick, seconds: u32::from(t.seconds) }))
        }
        GameEvent::TfMapTimeRemaining(t) => Some(Line::TimeRemaining(TimeRemaining { tick, seconds: t.seconds })),
        _ => None,
    }
}

fn jump(tick: u32, user: u16, kind: JumpKind, phase: JumpPhase) -> Line {
    Line::BlastJump(BlastJump { tick, player: PlayerId(user), kind, phase })
}

pub(crate) fn user_message(message: &UserMessage<'_>, look: &Lookup<'_>) -> Option<Line> {
    let tick = look.tick;
    match message {
        UserMessage::VoiceSubtitle(v) => Some(Line::VoiceCommand(VoiceCommand {
            tick,
            player: (look.user_of_entity)(u32::from(v.client))?,
            menu: v.menu,
            item: v.item,
        })),
        UserMessage::Text(t) => Some(Line::ServerText(ServerText { tick, text: strip_colors(t.text.as_ref()) })),
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
        let user = |ent: u32| (ent == 5).then_some(PlayerId(12));
        let class = |_| None;
        let look = Lookup { user_of_entity: &user, class_of_user: &class, tick: 3 };
        assert_eq!(
            user_message(&msg, &look),
            Some(Line::VoiceCommand(VoiceCommand { tick: 3, player: PlayerId(12), menu: 0, item: 1 }))
        );
        let other = UserMessage::VoiceSubtitle(VoiceSubtitleMessage { client: 6, menu: 0, item: 1 });
        assert_eq!(user_message(&other, &look), None);
    }
}
