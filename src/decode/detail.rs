//! Heals, blast jumps, pickups, the game modes' events and the HUD's messages: game events and
//! user messages turned into lines.

use super::extract::{building_kind, opt_user, strip_colors, team_num};
use super::props::{origin_parts, prop, prop_id, prop_vec};
use crate::output::{
    Announcer, BallCarrierKilled, BlastJump, CaptureBroken, CaptureStarted, ChatColor, Heal, HealSource, Hint, HudText,
    Impact, ImpactKind, JumpKind, JumpPhase, KilledCapper, Line, Mark, MedicDefended, NpcDamage, ObjectChange,
    PassBlocked, PassCaught, PassFree, PassGet, PassScore, PassStolen, Pickup, PickupItem, PickupSize, PlayerId,
    PlayerMoment, PointLocked, PointsScored, RobotKilled, RocketOpen, RoundSelected, Sapped, ScoreMethod, ServerText,
    Stun, TeamLeaderKilled, Teleported, TimeRemaining, Tournament, Upgrade, VoiceCommand, VoteCast, VoteFailed,
    VoteOptions, VotePassed, VoteStarted, round1, round2,
};
use bitbuffer::{BitRead, LittleEndian};
use std::collections::HashMap;
use tf_demo_parser::demo::gameevent_gen::GameEvent;
use tf_demo_parser::demo::gamevent::{GameEventEntry, GameEventType, GameEventValue, GameEventValueType, RawGameEvent};
use tf_demo_parser::demo::message::usermessage::UserMessage;
use tf_demo_parser::demo::parser::analyser::Class;
use tf_demo_parser::demo::sendprop::{SendProp, SendPropIdentifier, SendPropValue};

/// The field names of each game event tf-demo-parser has no type for, from the demo's own event
/// list, as tf-demo-parser keeps them (hashed): their values come in that order.
pub(crate) type RawNames = HashMap<String, Vec<u64>>;

/// A field name's hash, as tf-demo-parser keeps it in an event's definition.
pub(crate) fn field_hash(name: &str) -> u64 {
    GameEventEntry::new(name, GameEventValueType::None).hash
}

/// Lookups the extractor owns: entity index to user id, a user's current class, the tick and the
/// names of raw events' fields.
pub(crate) struct Lookup<'a> {
    pub user_of_entity: &'a dyn Fn(u32) -> Option<PlayerId>,
    pub class_of_user: &'a dyn Fn(u16) -> Option<Class>,
    pub tick: u32,
    pub raw_names: &'a RawNames,
}

/// User message types (TF2's `usermessages` order) read here by hand.
const USER_MESSAGE_HINT_TEXT: u8 = 19;
const USER_MESSAGE_HUD_MSG: u8 = 21;
const USER_MESSAGE_VOTE_START: u8 = 46;
const USER_MESSAGE_VOTE_PASS: u8 = 47;
const USER_MESSAGE_VOTE_FAILED: u8 = 48;
const USER_MESSAGE_MVM_UPGRADED: u8 = 60;
/// An entity index field meaning nobody.
const NO_ENTITY: u16 = u16::MAX;

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

const BLOOD_ORIGIN: [SendPropIdentifier; 3] = origin_parts("DT_TETFBlood");
const SPARKS_ORIGIN: [SendPropIdentifier; 3] = origin_parts("DT_TEParticleSystem");
const RICOCHET_POS: SendPropIdentifier = prop_id("DT_TEMetalSparks", "m_vecPos");
const WORLD_DECAL_ORIGIN: SendPropIdentifier = prop_id("DT_TEWorldDecal", "m_vecOrigin");
const DECAL_ORIGIN: SendPropIdentifier = prop_id("DT_TEDecal", "m_vecOrigin");

fn vector(props: &[SendProp], id: SendPropIdentifier) -> Option<[f32; 3]> {
    prop_vec(props, id).map(|v| [v.x, v.y, v.z])
}

fn parts(props: &[SendProp], ids: [SendPropIdentifier; 3]) -> Option<[f32; 3]> {
    let mut out = [0.0; 3];
    for (o, id) in out.iter_mut().zip(ids) {
        *o = match prop(props, id)? {
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

fn score_method(method: u16) -> ScoreMethod {
    match method {
        0 => ScoreMethod::ReactorCaptured,
        1 => ScoreMethod::CoresCollected,
        2 => ScoreMethod::ReactorReturned,
        3 => ScoreMethod::ReactorStolen,
        _ => ScoreMethod::Unknown,
    }
}

/// A game event the extractor does not turn into a line itself.
pub(crate) fn game_event(event: &GameEvent, look: &Lookup<'_>) -> Option<Line> {
    heal_event(event, look)
        .or_else(|| movement_event(event, look))
        .or_else(|| objective_event(event, look))
        .or_else(|| match_event(event, look))
        .or_else(|| building_event(event, look))
        .or_else(|| pass_time_event(event, look))
        .or_else(|| destruction_event(event, look))
}

/// Heals: medigun, dispenser, crossbow and on-hit.
fn heal_event(event: &GameEvent, look: &Lookup<'_>) -> Option<Line> {
    let tick = look.tick;
    let user = |ent: u8| (look.user_of_entity)(u32::from(ent));
    match event {
        GameEvent::PlayerHealed(h) => {
            // The event doesn't say what healed: infer it from the healer's class. A player healing
            // themselves (a medic's regeneration) has no healer.
            let healer = (h.healer != 0 && h.healer != h.patient).then_some(PlayerId(h.healer));
            let source = match healer.and_then(|healer| (look.class_of_user)(healer.0)) {
                Some(Class::Medic) => HealSource::Medigun,
                Some(Class::Engineer) => HealSource::Dispenser,
                _ => HealSource::Other,
            };
            heal(tick, Some(PlayerId(h.patient)), healer, i64::from(h.amount), source)
        }
        // `healer` and `target` are entity indexes.
        GameEvent::CrossbowHeal(c) => {
            heal(tick, user(c.target), user(c.healer), i64::from(c.amount), HealSource::Crossbow)
        }
        GameEvent::PlayerHealOnHit(h) => heal(tick, user(h.ent_index), None, i64::from(h.amount), HealSource::OnHit),
        _ => None,
    }
}

/// Blast jumps, rocket packs, teleports, stuns and pickups.
fn movement_event(event: &GameEvent, look: &Lookup<'_>) -> Option<Line> {
    let tick = look.tick;
    match event {
        GameEvent::RocketJump(j) => Some(jump(tick, j.user_id, JumpKind::Rocket, JumpPhase::Start)),
        GameEvent::RocketJumpLanded(j) => Some(jump(tick, j.user_id, JumpKind::Rocket, JumpPhase::Land)),
        GameEvent::StickyJump(j) => Some(jump(tick, j.user_id, JumpKind::Sticky, JumpPhase::Start)),
        GameEvent::StickyJumpLanded(j) => Some(jump(tick, j.user_id, JumpKind::Sticky, JumpPhase::Land)),
        GameEvent::RocketPackLaunch(j) => Some(jump(tick, j.user_id, JumpKind::RocketPack, JumpPhase::Start)),
        GameEvent::RocketPackLanded(j) => Some(jump(tick, j.user_id, JumpKind::RocketPack, JumpPhase::Land)),
        GameEvent::PlayerTeleported(t) => Some(Line::Teleported(Teleported {
            tick,
            player: PlayerId(t.user_id),
            builder: opt_user(t.builder_id),
            dist: round1(t.dist),
        })),
        GameEvent::PlayerStunned(st) => Some(Line::Stun(Stun {
            tick,
            stunner: opt_user(st.stunner),
            victim: PlayerId(st.victim),
            capping: st.victim_capping,
            big: st.big_stun,
        })),
        GameEvent::ItemPickup(i) => {
            let (item, size) = pickup(i.item.as_ref())?;
            Some(Line::Pickup(Pickup { tick, player: PlayerId(i.user_id), item, size }))
        }
        _ => None,
    }
}

/// Control points: captures started, broken and locked, and their defenders.
fn objective_event(event: &GameEvent, look: &Lookup<'_>) -> Option<Line> {
    let tick = look.tick;
    let user = |ent: u8| (look.user_of_entity)(u32::from(ent));
    match event {
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
        // `cappers` holds the cappers' entity indexes as characters, as for `point_captured`.
        GameEvent::TeamPlayPointStartCapture(c) => Some(Line::CaptureStarted(CaptureStarted {
            tick,
            point: c.cp,
            name: c.cp_name.to_string(),
            team: team_num(c.cap_team),
            owner: team_num(c.team),
            cappers: c.cappers.as_bytes().iter().map(|&e| user(e)).collect(),
        })),
        GameEvent::TeamPlayPointLocked(l) => {
            Some(Line::PointLocked(PointLocked { tick, point: l.cp, name: l.cp_name.to_string(), locked: true }))
        }
        GameEvent::TeamPlayPointUnlocked(l) => {
            Some(Line::PointLocked(PointLocked { tick, point: l.cp, name: l.cp_name.to_string(), locked: false }))
        }
        _ => None,
    }
}

/// Votes, tournament mode, the announcer and the match clock.
fn match_event(event: &GameEvent, look: &Lookup<'_>) -> Option<Line> {
    let tick = look.tick;
    match event {
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
        GameEvent::TeamPlayRoundSelected(r) => {
            Some(Line::RoundSelected(RoundSelected { tick, round: r.round.to_string() }))
        }
        _ => None,
    }
}

/// Buildings sapped, removed and detonated, and stickies set off.
fn building_event(event: &GameEvent, look: &Lookup<'_>) -> Option<Line> {
    let tick = look.tick;
    match event {
        GameEvent::PlayerSappedObject(sa) => Some(Line::Sapped(Sapped {
            tick,
            player: PlayerId(sa.user_id),
            owner: PlayerId(sa.owner_id),
            object: building_kind(u16::from(sa.object)),
            sapper: sa.sapper_id,
        })),
        GameEvent::ObjectRemoved(o) => Some(Line::BuildingRemoved(ObjectChange {
            tick,
            player: PlayerId(o.user_id),
            object: building_kind(o.object_type),
            id: o.index,
        })),
        GameEvent::ObjectDetonated(o) => Some(Line::BuildingDetonated(ObjectChange {
            tick,
            player: PlayerId(o.user_id),
            object: building_kind(o.object_type),
            id: o.index,
        })),
        GameEvent::DemomanDetStickies(d) => Some(Line::DetStickies(PlayerMoment { tick, player: PlayerId(d.player) })),
        _ => None,
    }
}

/// Pass time: the ball passed, caught, stolen and scored.
fn pass_time_event(event: &GameEvent, look: &Lookup<'_>) -> Option<Line> {
    let tick = look.tick;
    // Pass time names players by entity index.
    let ent = |ent: u16| (ent != NO_ENTITY).then(|| (look.user_of_entity)(u32::from(ent))).flatten();
    match event {
        GameEvent::PassGet(g) => Some(Line::PassGet(PassGet { tick, player: ent(g.owner)? })),
        GameEvent::PassFree(f) => {
            Some(Line::PassFree(PassFree { tick, player: ent(f.owner)?, attacker: ent(f.attacker) }))
        }
        GameEvent::PassPassCaught(c) => Some(Line::PassCaught(PassCaught {
            tick,
            passer: ent(c.passer)?,
            catcher: ent(c.catcher)?,
            dist: round1(c.dist),
            duration: round2(c.duration),
        })),
        GameEvent::PassScore(sc) => Some(Line::PassScore(PassScore {
            tick,
            scorer: ent(sc.scorer)?,
            assister: ent(sc.assister),
            points: sc.points,
        })),
        GameEvent::PassBallStolen(st) => {
            Some(Line::PassStolen(PassStolen { tick, victim: ent(st.victim)?, attacker: ent(st.attacker)? }))
        }
        GameEvent::PassBallBlocked(b) => {
            Some(Line::PassBlocked(PassBlocked { tick, owner: ent(b.owner)?, blocker: ent(b.blocker)? }))
        }
        GameEvent::Unknown(raw) => raw_event(raw, look),
        _ => None,
    }
}

/// Robot and player destruction, Doomsday and Mann vs. Machine.
fn destruction_event(event: &GameEvent, look: &Lookup<'_>) -> Option<Line> {
    let tick = look.tick;
    let user = |ent: u8| (look.user_of_entity)(u32::from(ent));
    match event {
        GameEvent::RdRobotKilled(r) => {
            Some(Line::RobotKilled(RobotKilled { tick, attacker: opt_user(r.attacker), weapon: r.weapon.to_string() }))
        }
        GameEvent::RdPlayerScorePoints(p) => Some(Line::PointsScored(PointsScored {
            tick,
            player: PlayerId(p.player),
            method: score_method(p.method),
            amount: p.amount.cast_signed(),
        })),
        // Player destruction names players by entity index.
        GameEvent::SpecialScore(sc) => Some(Line::SpecialScore(PlayerMoment { tick, player: user(sc.player)? })),
        GameEvent::TeamLeaderKilled(k) => {
            Some(Line::TeamLeaderKilled(TeamLeaderKilled { tick, killer: user(k.killer), victim: user(k.victim)? }))
        }
        GameEvent::DoomsdayRocketOpen(r) => Some(Line::RocketOpen(RocketOpen { tick, team: team_num(r.team) })),
        GameEvent::NpcHurt(n) => Some(Line::NpcDamage(NpcDamage {
            tick,
            id: n.ent_index,
            attacker: opt_user(n.attacker_player),
            amount: n.damage_amount,
            health: n.health,
            crit: n.crit,
            weapon_id: n.weapon_id,
            boss: n.boss,
        })),
        _ => None,
    }
}

/// An event tf-demo-parser has no type for, read by its field names.
fn raw_event(raw: &RawGameEvent, look: &Lookup<'_>) -> Option<Line> {
    let GameEventType::Unknown(name) = &raw.event_type else { return None };
    let names = look.raw_names.get(name)?;
    let number = |field: &str| match raw.values.get(names.iter().position(|&n| n == field_hash(field))?)? {
        GameEventValue::Short(v) => Some(u32::from(*v)),
        GameEventValue::Long(v) => Some(*v),
        GameEventValue::Byte(v) => Some(u32::from(*v)),
        _ => None,
    };
    let ent = |field: &str| number(field).filter(|&e| e != u32::from(NO_ENTITY)).and_then(look.user_of_entity);
    match name.as_str() {
        // Pass time; entity indexes.
        "killed_ball_carrier" => Some(Line::BallCarrierKilled(BallCarrierKilled {
            tick: look.tick,
            attacker: ent("attacker"),
            victim: ent("victim")?,
            assister: ent("assister"),
        })),
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
        UserMessage::Unknown(u) => raw_user_message(u.raw_type, &mut u.data.clone(), look),
        _ => None,
    }
}

type Stream<'a> = bitbuffer::BitReadStream<'a, LittleEndian>;

fn read<'a, T: BitRead<'a, LittleEndian>>(data: &mut Stream<'a>) -> Option<T> {
    data.read().ok()
}

/// A null-terminated string; bitbuffer reads one cut short by the end of the data as whatever is there.
fn string(data: &mut Stream<'_>) -> Option<String> {
    let mut bytes = Vec::new();
    loop {
        match read::<u8>(data)? {
            0 => return Some(String::from_utf8_lossy(&bytes).into_owned()),
            b => bytes.push(b),
        }
    }
}

/// A user message tf-demo-parser has no type for; short or malformed data gives none.
fn raw_user_message(kind: u8, data: &mut Stream<'_>, look: &Lookup<'_>) -> Option<Line> {
    let tick = look.tick;
    match kind {
        USER_MESSAGE_HINT_TEXT => Some(Line::Hint(Hint { tick, text: string(data)? })),
        // channel, x, y, colour, second colour, effect, fade in, fade out, hold time, effect time, text.
        USER_MESSAGE_HUD_MSG => {
            let channel = read::<u8>(data)?;
            let (left, top) = (read::<f32>(data)?, read::<f32>(data)?);
            let [red, green, blue, _alpha] = read::<[u8; 4]>(data)?;
            let _second_color = read::<[u8; 4]>(data)?;
            let _effect = read::<u8>(data)?;
            let [_fade_in, _fade_out, hold, _effect_time] = read::<[f32; 4]>(data)?;
            let text = string(data)?;
            Some(Line::HudText(HudText {
                tick,
                channel,
                pos: [round2(left), round2(top)],
                color: ChatColor::Rgb([red, green, blue]),
                hold: round2(hold),
                text,
            }))
        }
        // team, vote index, the caller's entity, issue, details, a one-bit yes/no flag and the
        // target's entity (none from servers that predate it).
        USER_MESSAGE_VOTE_START => {
            let team = team_num(read::<u8>(data)?);
            let _vote = read::<u32>(data)?;
            let caller = (look.user_of_entity)(u32::from(read::<u8>(data)?));
            let (issue, details) = (string(data)?, string(data)?);
            let yes_no = read::<bool>(data)?;
            let target = read::<u8>(data).and_then(|e| (look.user_of_entity)(u32::from(e)));
            Some(Line::VoteStarted(VoteStarted { tick, team, caller, issue, details, yes_no, target }))
        }
        // team, vote index, issue, details.
        USER_MESSAGE_VOTE_PASS => {
            let team = team_num(read::<u8>(data)?);
            let _vote = read::<u32>(data)?;
            let (issue, details) = (string(data)?, string(data)?);
            Some(Line::VotePassed(VotePassed { tick, team, issue, details }))
        }
        // The player's entity, a byte, the item definition, the attribute, a byte, the cost: checked
        // against the money the upgraded item then holds as refundable (`m_nRefundableCurrency`).
        USER_MESSAGE_MVM_UPGRADED => {
            let player = (look.user_of_entity)(u32::from(read::<u8>(data)?))?;
            let _unknown = read::<u8>(data)?;
            let (item, attribute) = (read::<u16>(data)?, read::<u16>(data)?);
            let _unknown = read::<u8>(data)?;
            Some(Line::Upgrade(Upgrade { tick, player, item, attribute, cost: read::<u16>(data)? }))
        }
        // team, vote index, reason.
        USER_MESSAGE_VOTE_FAILED => {
            let team = team_num(read::<u8>(data)?);
            let _vote = read::<u32>(data)?;
            Some(Line::VoteFailed(VoteFailed { tick, team, reason: read::<u8>(data)? }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tf_demo_parser::demo::message::usermessage::VoiceSubtitleMessage;

    /// Runs `f` with lookups that know players by `user` and nobody's class, at tick 3.
    fn with_lookup<R>(
        user: impl Fn(u32) -> Option<PlayerId>,
        raw_names: &RawNames,
        f: impl FnOnce(&Lookup<'_>) -> R,
    ) -> R {
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

    fn hex(text: &str) -> Vec<u8> {
        text.split(' ').map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    #[test]
    fn hud_messages_and_votes_are_read_by_hand() {
        // A dom_canalzone score counter, as recorded.
        let hud = hex(
            "02 66 66 26 3f 85 eb 51 3f ff 55 42 00 ff ff ff 00 00 00 00 00 3f 00 00 00 3f 00 00 80 3f 00 00 80 3e 30 00",
        );
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
        let hud = hex(
            "02 66 66 26 3f 85 eb 51 3f ff 55 42 00 ff ff ff 00 00 00 00 00 3f 00 00 00 3f 00 00 80 3f 00 00 80 3e 30 00",
        );
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
}
