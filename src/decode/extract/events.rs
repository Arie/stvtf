//! Game events, temp entities and user messages: the discrete lines.

use super::convert::{
    building_kind, chat_kind, class_num, color_parts, medigun_kind, opt_user, strip_colors, team_num, xyz,
};
use super::state::Stats;
use super::{Extractor, debug};
use crate::decode::detail;
use crate::decode::props::{origin_parts, prop_f, prop_i, prop_id, prop_vec};
use crate::output::{self as out, BannerKind, ChatKind, Line, PlayerId, round1};
use tf_demo_parser::ParserState;
use tf_demo_parser::demo::gameevent_gen::{GameEvent, PlayerDeathEvent};
use tf_demo_parser::demo::message::TempEntitiesMessage;
use tf_demo_parser::demo::message::usermessage::UserMessage;
use tf_demo_parser::demo::sendprop::{SendProp, SendPropIdentifier};

/// TF2 sets `DMG_ACID` on critical hits.
const DMG_CRIT: u32 = 1 << 20;

/// `player_death` `death_flags`.
const DEATH_DOMINATION: u16 = 0x1;
const DEATH_REVENGE: u16 = 0x4;
const DEATH_FEIGN: u16 = 0x20;

/// `player_death` customkill values.
const CUSTOM_KILL_HEADSHOT: u16 = 1;
const CUSTOM_KILL_BACKSTAB: u16 = 2;

/// User message id of `SayText` (console `say` and server notices).
const USER_MESSAGE_SAY_TEXT: u8 = 3;

/// `player_spawn` events waiting for a position; more than this means entity updates stopped.
const MAX_PENDING_SPAWNS: usize = 256;

/// `player_death` `assister` without an assister.
const NO_ASSISTER: u16 = u16::MAX;
const PROP_BULLETS_PLAYER: SendPropIdentifier = prop_id("DT_TEFireBullets", "m_iPlayer");
const PROP_BULLETS_ORIGIN: SendPropIdentifier = prop_id("DT_TEFireBullets", "m_vecOrigin");
const PROP_BULLETS_PITCH: SendPropIdentifier = prop_id("DT_TEFireBullets", "m_vecAngles[0]");
const PROP_BULLETS_YAW: SendPropIdentifier = prop_id("DT_TEFireBullets", "m_vecAngles[1]");
const PROP_BULLETS_WEAPON: SendPropIdentifier = prop_id("DT_TEFireBullets", "m_iWeaponID");
const PROP_BULLETS_CRIT: SendPropIdentifier = prop_id("DT_TEFireBullets", "m_bCritical");
const PROP_EXPLOSION_ORIGIN: [SendPropIdentifier; 3] = origin_parts("DT_TETFExplosion");
const PROP_EXPLOSION_WEAPON: SendPropIdentifier = prop_id("DT_TETFExplosion", "m_iWeaponID");

fn assister(d: &PlayerDeathEvent) -> Option<PlayerId> {
    if d.assister == NO_ASSISTER { None } else { opt_user(d.assister) }
}

/// A round starting, going live, ending and the match ending.
pub(super) fn round_event(event: &GameEvent, tick: u32) -> Option<Line> {
    Some(match event {
        GameEvent::TeamPlayRoundStart(r) => Line::RoundStart(out::RoundStart { tick, full_reset: r.full_reset }),
        GameEvent::TeamPlayRoundActive(_) => Line::RoundActive(out::Moment { tick }),
        GameEvent::TeamPlaySetupFinished(_) => Line::SetupFinished(out::Moment { tick }),
        GameEvent::TeamPlayRoundWin(w) => Line::RoundWin(out::RoundWin {
            tick,
            team: team_num(w.team),
            reason: w.win_reason,
            round_time: f64::from(w.round_time),
        }),
        GameEvent::TeamPlayRoundStalemate(_) => Line::Stalemate(out::Moment { tick }),
        GameEvent::TeamPlayOvertimeBegin(_) => Line::Overtime(out::Moment { tick }),
        GameEvent::TeamPlayWinPanel(w) => {
            Line::Scores(out::Scores { tick, red: w.red_score, blue: w.blue_score, game_over: w.game_over != 0 })
        }
        GameEvent::TeamPlayGameOver(g) => Line::GameOver(out::GameOver { tick, reason: g.reason.to_string() }),
        GameEvent::TfGameOver(g) => Line::GameOver(out::GameOver { tick, reason: g.reason.to_string() }),
        _ => return None,
    })
}

impl Extractor {
    pub(super) fn game_event(&mut self, event: &GameEvent) {
        self.count_score(event);
        match event {
            GameEvent::PlayerSpawn(s) => self.hold_spawn(s.user_id, s.team, s.class),
            GameEvent::PlayerDisconnect(d) => {
                self.left.insert(d.user_id);
            }
            _ => {}
        }
        let line = self
            .player_event(event)
            .or_else(|| self.building_event(event))
            .or_else(|| self.objective_event(event))
            .or_else(|| round_event(event, self.server_tick))
            .or_else(|| self.with_lookup(|look| detail::game_event(event, look)));
        if let Some(line) = line {
            self.push(line);
        }
    }

    /// `player_spawn` arrives before the player's new position: it waits for the next entity update.
    fn hold_spawn(&mut self, user_id: u16, team: u16, class: u16) {
        if self.pending_spawns.len() < MAX_PENDING_SPAWNS {
            let player = PlayerId(user_id);
            let spawn =
                out::Spawn { tick: self.server_tick, player, team: team_num(team), class: class_num(class), pos: None };
            self.pending_spawns.push((user_id, spawn));
        }
    }

    /// Kills, damage, übers and players coming, going and changing team or class.
    fn player_event(&self, event: &GameEvent) -> Option<Line> {
        let tick = self.server_tick;
        let user = PlayerId;
        Some(match event {
            GameEvent::PlayerDeath(d) => {
                // A suicide names the victim as attacker: it has none, nor a position for one.
                let attacker = if d.attacker == d.user_id { 0 } else { d.attacker };
                // Building kills (sentries) report the building as inflictor; attacker stays the engineer.
                Line::Kill(out::Kill {
                    tick,
                    victim: user(d.user_id),
                    attacker: opt_user(attacker),
                    assister: assister(d),
                    weapon: d.weapon.to_string(),
                    crit: d.damage_bits & DMG_CRIT != 0,
                    headshot: d.custom_kill == CUSTOM_KILL_HEADSHOT,
                    backstab: d.custom_kill == CUSTOM_KILL_BACKSTAB,
                    suicide: d.attacker == 0 || d.attacker == d.user_id,
                    dominated: d.death_flags & DEATH_DOMINATION != 0,
                    revenge: d.death_flags & DEATH_REVENGE != 0,
                    // A spy's Dead Ringer: the victim is not actually dead.
                    feign: d.death_flags & DEATH_FEIGN != 0,
                    // The victim was blast jumping: an airshot for projectiles.
                    airborne: d.rocket_jump,
                    victim_pos: self.user_pos(d.user_id),
                    attacker_pos: self.user_pos(attacker),
                })
            }
            GameEvent::PlayerHurt(h) => Line::Damage(out::Damage {
                tick,
                victim: user(h.user_id),
                attacker: opt_user(h.attacker),
                amount: h.damage_amount,
                health: h.health,
                crit: h.crit,
                minicrit: h.mini_crit,
                weapon_id: h.weapon_id,
            }),
            GameEvent::PlayerChargeDeployed(c) => Line::Uber(out::Uber {
                tick,
                medic: user(c.user_id),
                target: opt_user(c.target_id),
                medigun: self
                    .entity_of_user(c.user_id)
                    .and_then(|e| self.medigun_owned_by(e))
                    .map(|m| medigun_kind(m.item)),
            }),
            GameEvent::MedicDeath(m) => Line::MedicDeath(out::MedicDeath {
                tick,
                medic: user(m.user_id),
                attacker: opt_user(m.attacker),
                healing: m.healing,
                // Died with a full charge: a dropped über.
                drop: m.charged,
            }),
            GameEvent::PlayerTeam(t) => Line::TeamChange(out::TeamChange {
                tick,
                player: user(t.user_id),
                team: team_num(t.team),
                old_team: team_num(t.old_team),
                disconnect: t.disconnect,
            }),
            GameEvent::PlayerChangeClass(c) => {
                Line::ClassChange(out::ClassChange { tick, player: user(c.user_id), class: class_num(c.class) })
            }
            GameEvent::PlayerDisconnect(d) => Line::PlayerLeave(out::PlayerLeave {
                tick,
                player: user(d.user_id),
                name: d.name.to_string(),
                reason: d.reason.to_string(),
            }),
            GameEvent::ObjectDeflected(o) => Line::Reflect(out::Reflect {
                tick,
                player: user(o.user_id),
                owner: opt_user(o.owner_id),
                weapon_id: o.weapon_id,
            }),
            GameEvent::DeployBuffBanner(b) => Line::Banner(out::Banner {
                tick,
                player: user(b.buff_owner),
                kind: match b.buff_type {
                    1 => BannerKind::BuffBanner,
                    2 => BannerKind::BattalionsBackup,
                    3 => BannerKind::Concheror,
                    _ => BannerKind::Other,
                },
            }),
            _ => return None,
        })
    }

    /// Engineers' buildings built, upgraded, repaired and destroyed.
    fn building_event(&self, event: &GameEvent) -> Option<Line> {
        let tick = self.server_tick;
        Some(match event {
            GameEvent::PlayerBuiltObject(b) => Line::Build(out::ObjectChange {
                tick,
                player: PlayerId(b.user_id),
                object: building_kind(b.object),
                id: b.index,
            }),
            GameEvent::PlayerUpgradedObject(u) => Line::BuildingUpgrade(out::ObjectChange {
                tick,
                player: PlayerId(u.user_id),
                object: building_kind(u.object),
                id: u.index,
            }),
            GameEvent::BuildingHealed(b) => {
                Line::BuildingRepair(out::BuildingRepair { tick, id: b.building, amount: b.amount })
            }
            GameEvent::ObjectDestroyed(o) => Line::BuildingDestroyed(out::BuildingDestroyed {
                tick,
                owner: PlayerId(o.user_id),
                attacker: opt_user(o.attacker),
                object: building_kind(o.object_type),
                id: o.index,
                weapon: o.weapon.to_string(),
                was_building: o.was_building,
            }),
            _ => return None,
        })
    }

    /// Points captured and blocked, flags moving and time added to a round timer.
    fn objective_event(&self, event: &GameEvent) -> Option<Line> {
        let tick = self.server_tick;
        Some(match event {
            GameEvent::TeamPlayPointCaptured(c) => Line::PointCaptured(out::PointCaptured {
                tick,
                point: c.cp,
                name: c.cp_name.to_string(),
                team: team_num(c.team),
                cappers: c.cappers.as_bytes().iter().map(|&e| self.user_of_entity(u32::from(e))).collect(),
            }),
            GameEvent::TeamPlayCaptureBlocked(b) => Line::CaptureBlocked(out::CaptureBlocked {
                tick,
                point: b.cp,
                name: b.cp_name.to_string(),
                blocker: self.user_of_entity(u32::from(b.blocker)),
                victim: self.user_of_entity(u32::from(b.victim)),
            }),
            // `player` and `carrier` are entity indexes here, not user ids.
            GameEvent::TeamPlayFlagEvent(f) => Line::Flag(out::Flag {
                tick,
                event: match f.event_type {
                    1 => out::FlagAction::Pickup,
                    2 => out::FlagAction::Captured,
                    3 => out::FlagAction::Defended,
                    4 => out::FlagAction::Dropped,
                    5 => out::FlagAction::Returned,
                    _ => return None,
                },
                player: self.user_of_entity(u32::from(f.player)),
                carrier: (f.carrier != 0).then(|| self.user_of_entity(u32::from(f.carrier))).flatten(),
                team: team_num(f.team),
            }),
            GameEvent::TeamPlayTimerTimeAdded(t) => Line::TimeAdded(out::TimeAdded {
                tick,
                team: self.timer_team(u32::from(t.timer)),
                seconds: t.seconds_added,
            }),
            _ => return None,
        })
    }

    fn temp_entity(&mut self, class: &str, props: &[SendProp]) {
        if debug(class) {
            for p in props {
                eprintln!("{class} {:?} = {:?}", p.identifier.names(), p.value);
            }
        }
        let tick = self.server_tick;
        match class {
            "CTEFireBullets" => {
                // m_iPlayer is the player's entity index minus one.
                let player = u32::try_from(prop_i(props, PROP_BULLETS_PLAYER))
                    .ok()
                    .and_then(|i| i.checked_add(1))
                    .and_then(|ent| self.user_of_entity(ent));
                self.push(Line::Shot(out::Shot {
                    tick,
                    player,
                    origin: xyz(prop_vec(props, PROP_BULLETS_ORIGIN).unwrap_or_default()),
                    pitch: round1(prop_f(props, PROP_BULLETS_PITCH)),
                    yaw: round1(prop_f(props, PROP_BULLETS_YAW)),
                    weapon_id: prop_i(props, PROP_BULLETS_WEAPON),
                    crit: prop_i(props, PROP_BULLETS_CRIT) != 0,
                }));
            }
            "CTETFExplosion" => {
                self.push(Line::Explosion(out::Explosion {
                    tick,
                    origin: PROP_EXPLOSION_ORIGIN.map(|id| round1(prop_f(props, id))),
                    weapon_id: prop_i(props, PROP_EXPLOSION_WEAPON),
                }));
            }
            _ => {
                if let Some(line) = detail::temp_entity(class, props, tick) {
                    self.push(line);
                }
            }
        }
    }

    /// The scoreboard's counted columns, like logs.tf: damage dealt to others, healing given and
    /// assists, from the events since the match started (or since connecting).
    fn count_score(&mut self, event: &GameEvent) {
        let (user, amount, column): (PlayerId, i64, fn(&mut Stats) -> &mut i64) = match event {
            GameEvent::PlayerHurt(h) if h.attacker != 0 && h.attacker != h.user_id => {
                (PlayerId(h.attacker), i64::from(h.damage_amount), |s| &mut s.damage)
            }
            GameEvent::PlayerHealed(h) if h.healer != 0 && h.healer != h.patient => {
                (PlayerId(h.healer), i64::from(h.amount), |s| &mut s.healing)
            }
            GameEvent::PlayerDeath(d) if d.death_flags & DEATH_FEIGN == 0 => match assister(d) {
                Some(a) => (a, 1, |s| &mut s.assists),
                None => return,
            },
            _ => return,
        };
        if let Some(ent) = self.entity_of_user(user.0) {
            *column(self.stats.entry(ent).or_default()) += amount;
        }
    }

    pub(super) fn temp_entities(&mut self, te: &TempEntitiesMessage, state: &ParserState) {
        // An event of the same class as the one before it in the message is
        // delta-encoded against it; any other against zeros, so a missing prop is 0.
        let mut prev: Option<(u16, Vec<SendProp>)> = None;
        for ev in &te.events {
            let class_id = u16::from(ev.class_id);
            let mut props = match prev.take() {
                Some((c, p)) if c == class_id => p,
                _ => Vec::new(),
            };
            for p in &ev.props {
                match props.iter_mut().find(|q| q.identifier == p.identifier) {
                    Some(q) => q.value = p.value.clone(),
                    None => props.push(p.clone()),
                }
            }
            if let Some(class) = state.server_classes.get(usize::from(class_id)) {
                self.temp_entity(class.name.as_str(), &props);
            }
            prev = Some((class_id, props));
        }
    }

    pub(super) fn user_message(&mut self, message: &UserMessage<'_>) {
        match message {
            UserMessage::Unknown(u) if u.raw_type == USER_MESSAGE_SAY_TEXT => {
                // SayText: console `say` and server notices. u8 client, string text.
                let mut d = u.data.clone();
                let client: u8 = d.read().unwrap_or(0);
                let text: String = d.read().unwrap_or_default();
                self.push(Line::Chat(out::Chat {
                    tick: self.server_tick,
                    player: if client == 0 { None } else { self.user_of_entity(u32::from(client)) },
                    name: None,
                    text: strip_colors(&text),
                    parts: color_parts(&text),
                    kind: ChatKind::SayText,
                }));
            }
            UserMessage::SayText2(m) => {
                // Not chat: the parser reads a game message's format string (such as
                // #TF_Halloween_Underworld, "has escaped the underworld") as ChatAll, and its name
                // as the sender, leaving no text.
                let text = strip_colors(m.text.as_ref());
                if text.is_empty() {
                    return;
                }
                let player = self.user_of_entity(u32::from(m.client));
                self.push(Line::Chat(out::Chat {
                    tick: self.server_tick,
                    player,
                    name: m.from.as_ref().map(ToString::to_string),
                    text,
                    parts: color_parts(m.text.as_ref()),
                    kind: chat_kind(m.kind),
                }));
            }
            _ => {
                if let Some(line) = self.with_lookup(|look| detail::user_message(message, look)) {
                    self.push(line);
                }
            }
        }
    }
}
