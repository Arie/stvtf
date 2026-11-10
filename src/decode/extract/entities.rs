//! Entity updates: per-class handlers that read the props of one entity.

use super::convert::{alive, builder_entity, flag_status, round_from_raw};
use super::state::{ControlPoint, Pose, Weapon};
use super::{
    COND_WORDS, Extractor, FLAMETHROWER_AIRBLAST, MAX_CONTROL_POINTS, MAX_TEAMS, MY_WEAPON_SLOTS, PROP_MY_WEAPONS,
    debug, debug_entity,
};
use crate::decode::modes;
use crate::decode::props::{
    BLUE_SLOT, EF_NODRAW, PROP_EFFECTS, PROP_ORIGIN, PROP_TEAM_NUM, RED_SLOT, as_float, as_int, opt_handle, prop_id,
    prop_vec, team_byte,
};
use crate::output::{self as out, Airblast, BuildingKind, Line};
use std::collections::HashSet;
use tf_demo_parser::ParserState;
use tf_demo_parser::demo::data::game_state::Building;
use tf_demo_parser::demo::message::packetentities::{EntityId, UpdateType};
use tf_demo_parser::demo::message::{PacketEntitiesMessage, PacketEntity};
use tf_demo_parser::demo::sendprop::{SendProp, SendPropIdentifier, SendPropValue};

/// `Weapon::clip` of a weapon without a clip.
const NO_CLIP: i64 = -1;
/// The props of a player's condition words, in `COND_WORDS` order.
const PROP_CONDS: [SendPropIdentifier; COND_WORDS] = [
    prop_id("DT_TFPlayerShared", "m_nPlayerCond"),
    prop_id("DT_TFPlayerConditionListExclusive", "_condition_bits"),
    prop_id("DT_TFPlayerShared", "m_nPlayerCondEx"),
    prop_id("DT_TFPlayerShared", "m_nPlayerCondEx2"),
    prop_id("DT_TFPlayerShared", "m_nPlayerCondEx3"),
    prop_id("DT_TFPlayerShared", "m_nPlayerCondEx4"),
];
const PROP_TEAM_TEAM_NUM: SendPropIdentifier = prop_id("DT_Team", "m_iTeamNum");
const PROP_TEAM_SCORE: SendPropIdentifier = prop_id("DT_Team", "m_iScore");
const PROP_FLAG_CAPTURES: SendPropIdentifier = prop_id("DT_TFTeam", "m_nFlagCaptures");
const PROP_TIMER_END_TIME: SendPropIdentifier = prop_id("DT_TeamRoundTimer", "m_flTimerEndTime");
const PROP_TIMER_REMAINING: SendPropIdentifier = prop_id("DT_TeamRoundTimer", "m_flTimeRemaining");
const PROP_TIMER_PAUSED: SendPropIdentifier = prop_id("DT_TeamRoundTimer", "m_bTimerPaused");
const PROP_TIMER_SHOW_IN_HUD: SendPropIdentifier = prop_id("DT_TeamRoundTimer", "m_bShowInHUD");
const PROP_TIMER_DISABLED: SendPropIdentifier = prop_id("DT_TeamRoundTimer", "m_bIsDisabled");
const PROP_TIMER_STATE: SendPropIdentifier = prop_id("DT_TeamRoundTimer", "m_nState");
const PROP_TIMER_STOPWATCH: SendPropIdentifier = prop_id("DT_TeamRoundTimer", "m_bStopWatchTimer");
const PROP_TIMER_WATCHING_CAPS: SendPropIdentifier = prop_id("DT_TeamRoundTimer", "m_bInCaptureWatchState");
const PROP_TIMER_TOTAL: SendPropIdentifier = prop_id("DT_TeamRoundTimer", "m_flTotalTime");
const PROP_TEAM_ROLE: SendPropIdentifier = prop_id("DT_TFTeam", "m_iRole");
const PROP_STOPWATCH: SendPropIdentifier = prop_id("DT_TeamplayRoundBasedRules", "m_bStopWatch");
const PROP_STOPWATCH_STATE: SendPropIdentifier = prop_id("DT_TFGameRules", "m_nStopWatchState");
const PROP_SENTRY_POSE_PITCH: SendPropIdentifier = prop_id("m_flPoseParameter", "000");
const PROP_SENTRY_POSE_YAW: SendPropIdentifier = prop_id("m_flPoseParameter", "001");
const PROP_SENTRY_ENEMY: SendPropIdentifier = prop_id("DT_ObjectSentrygun", "m_hEnemy");
const PROP_SENTRY_STATE: SendPropIdentifier = prop_id("DT_ObjectSentrygun", "m_iState");
const PROP_SENTRY_SHELLS: SendPropIdentifier = prop_id("DT_ObjectSentrygun", "m_iAmmoShells");
const PROP_WATCHER_PROGRESS: SendPropIdentifier = prop_id("DT_TeamTrainWatcher", "m_flTotalProgress");
const PROP_WATCHER_CAPPERS: SendPropIdentifier = prop_id("DT_TeamTrainWatcher", "m_nNumCappers");
const PROP_WATCHER_SPEED: SendPropIdentifier = prop_id("DT_TeamTrainWatcher", "m_iTrainSpeedLevel");
const PROP_WATCHER_GLOW: SendPropIdentifier = prop_id("DT_TeamTrainWatcher", "m_hGlowEnt");
const PROP_OWNER_ENTITY: SendPropIdentifier = prop_id("DT_BaseEntity", "m_hOwnerEntity");
const PROP_FLAG_STATUS: SendPropIdentifier = prop_id("DT_CaptureFlag", "m_nFlagStatus");
const PROP_FLAG_DISABLED: SendPropIdentifier = prop_id("DT_CaptureFlag", "m_bDisabled");
const PROP_FLAG_RESET_TIME: SendPropIdentifier = prop_id("DT_CaptureFlag", "m_flResetTime");
const PROP_FLAG_POINTS: SendPropIdentifier = prop_id("DT_CaptureFlag", "m_nPointValue");
const PROP_ROUND_STATE: SendPropIdentifier = prop_id("DT_TeamplayRoundBasedRules", "m_iRoundState");
const PROP_CARRYING: SendPropIdentifier = prop_id("DT_TFPlayerShared", "m_bCarryingObject");
const PROP_CARRIED: SendPropIdentifier = prop_id("DT_TFPlayerShared", "m_hCarriedObject");
const PROP_RED_KOTH_TIMER: SendPropIdentifier = prop_id("DT_TFGameRules", "m_hRedKothTimer");
const PROP_BLUE_KOTH_TIMER: SendPropIdentifier = prop_id("DT_TFGameRules", "m_hBlueKothTimer");
const PROP_MAP_RESET_TIME: SendPropIdentifier = prop_id("DT_TeamplayRoundBasedRules", "m_flMapResetTime");
const PROP_STATE_TRANSITION_TIME: SendPropIdentifier = prop_id("DT_TeamplayRoundBasedRules", "m_flStateTransitionTime");
const PROP_WEAPON_OWNER: SendPropIdentifier = prop_id("DT_BaseCombatWeapon", "m_hOwner");
const PROP_ACTIVE_WEAPON: SendPropIdentifier = prop_id("DT_BaseCombatCharacter", "m_hActiveWeapon");
const PROP_CLIP: SendPropIdentifier = prop_id("DT_LocalWeaponData", "m_iClip1");
const PROP_ITEM_DEFINITION: SendPropIdentifier = prop_id("DT_ScriptCreatedItem", "m_iItemDefinitionIndex");
const PROP_MEDIGUN_RELEASING: SendPropIdentifier = prop_id("DT_WeaponMedigun", "m_bChargeRelease");
const PROP_MEDIGUN_RESIST: SendPropIdentifier = prop_id("DT_WeaponMedigun", "m_nChargeResistType");
const PROP_MEDIGUN_TARGET: SendPropIdentifier = prop_id("DT_WeaponMedigun", "m_hHealingTarget");
const PROP_FLAMETHROWER_STATE: SendPropIdentifier = prop_id("DT_WeaponFlameThrower", "m_iWeaponState");
const PROP_FLAMETHROWER_CRIT: SendPropIdentifier = prop_id("DT_WeaponFlameThrower", "m_bCritFire");
const PROP_FLAMETHROWER_HIT: SendPropIdentifier = prop_id("DT_WeaponFlameThrower", "m_bHitTarget");

/// Recent frames kept to resolve deltas against.
const MAX_FRAMES: usize = 128;

const PROP_ROTATION: SendPropIdentifier = prop_id("DT_BaseEntity", "m_angRotation");

impl Extractor {
    /// One prop of `CTFObjectiveResource`. Its per point arrays come as elements named
    /// after the array with the index as name, so they go by name rather than identifier.
    fn objective_prop(&mut self, p: &SendProp) {
        let Some((table, name)) = p.identifier.names() else { return };
        if name.as_str() == "m_vCPPositions" {
            if let SendPropValue::Array(items) = &p.value {
                if self.control_points.len() < items.len() {
                    self.control_points.resize(items.len().min(MAX_CONTROL_POINTS), ControlPoint::default());
                }
                for (cp, item) in self.control_points.iter_mut().zip(items) {
                    if let SendPropValue::Vector(v) = item {
                        cp.pos = Some(*v);
                    }
                }
            }
            return;
        }
        if name.as_str() == "m_iStopWatchTimer" {
            self.stopwatch.timer = u32::try_from(as_int(&p.value)).ok().filter(|i| *i != 0);
            return;
        }
        if name.as_str() == "m_iNumControlPoints" {
            if let SendPropValue::Integer(n) = p.value {
                self.num_control_points = usize::try_from(n).unwrap_or(0).min(MAX_CONTROL_POINTS);
            }
            return;
        }
        let Ok(idx) = name.as_str().parse::<usize>() else { return };
        if matches!(table.as_str(), "m_flTeamCapTime" | "m_iNumTeamMembers") {
            let (team, point) = (idx / MAX_CONTROL_POINTS, idx % MAX_CONTROL_POINTS);
            if team >= MAX_TEAMS {
                return;
            }
            let cp = self.control_point_mut(point);
            match &p.value {
                SendPropValue::Float(f) if table.as_str() == "m_flTeamCapTime" => cp.cap_time[team] = *f,
                SendPropValue::Integer(i) => cp.members[team] = u8::try_from((*i).max(0)).unwrap_or(u8::MAX),
                _ => {}
            }
            return;
        }
        if idx >= MAX_CONTROL_POINTS {
            return;
        }
        let cp = self.control_point_mut(idx);
        let int = as_int(&p.value);
        match table.as_str() {
            "m_vCPPositions" => {
                if let SendPropValue::Vector(v) = p.value {
                    cp.pos = Some(v);
                }
            }
            "m_iOwner" => cp.owner = team_byte(int),
            "m_iCappingTeam" => {
                if cp.capping_team != team_byte(int) {
                    cp.progress = 0.0;
                }
                cp.capping_team = team_byte(int);
            }
            "m_flLazyCapPerc" => {
                if let SendPropValue::Float(f) = p.value {
                    cp.progress = (1.0 - f).clamp(0.0, 1.0);
                }
            }
            "m_iTeamInZone" => cp.team_in_zone = team_byte(int),
            "m_bCPLocked" => cp.locked = int != 0,
            "m_bBlocked" => cp.blocked = int != 0,
            _ => {}
        }
    }

    /// A delta names only what changed since its base frame, so an entity that came and went in
    /// between (a rocket fired and exploded since then) is in neither, and no delete for it is
    /// ever sent. `SourceTV` relays often send deltas from a frame a few ticks back; demo files
    /// from the previous tick, where every delete arrives. Like the game client, this frame's
    /// entities are the base frame's plus those that entered, less those that left: projectiles
    /// and buildings outside it are gone. Without the base frame (just connected, or one lost to
    /// a decode error), what it held is unknown: it is taken to be the entities known now, so
    /// nothing is dropped, and the next delta, from this frame, finds its base.
    pub(super) fn track_frame(&mut self, pe: &PacketEntitiesMessage) {
        let base = match pe.delta {
            None => Some(HashSet::new()),
            Some(t) => self.frames.iter().rev().find(|(tick, _)| *tick == u32::from(t)).map(|(_, f)| f.clone()),
        };
        if base.is_none() {
            self.lost_frame.set(true);
        }
        let mut frame = base.unwrap_or_else(|| self.tracked().collect());
        for e in &pe.entities {
            let id = u32::from(e.entity_index);
            match e.update_type {
                UpdateType::Enter | UpdateType::Delta => frame.insert(id),
                UpdateType::Leave | UpdateType::Delete => frame.remove(&id),
            };
        }
        for id in &pe.removed_entities {
            frame.remove(&u32::from(*id));
        }
        let gone: Vec<u32> = self.tracked().filter(|id| !frame.contains(id)).collect();
        for id in gone {
            self.gs.state.projectile_destroy(EntityId::from(id));
            self.gs.state.remove_building(EntityId::from(id));
            self.forget_entity(id);
        }
        // A later delta never comes from a frame older than this one's base.
        if let Some(t) = pe.delta {
            self.frames.retain(|(tick, _)| *tick >= u32::from(t));
        }
        self.frames.push_back((self.server_tick, frame));
        if self.frames.len() > MAX_FRAMES {
            self.frames.pop_front();
        }
    }

    pub(super) fn packet_entities(&mut self, pe: &PacketEntitiesMessage, state: &ParserState) {
        if let Some(ent) = debug_entity() {
            for e in pe.entities.iter().filter(|e| u32::from(e.entity_index) == ent) {
                let class = state.server_classes.get(usize::from(e.server_class)).map_or("?", |c| c.name.as_str());
                let props: Vec<String> = e
                    .props
                    .iter()
                    .map(|p| match p.identifier.names() {
                        Some((table, name)) => format!("{table}.{name}={:?}", p.value),
                        None => format!("{:?}={:?}", p.identifier, p.value),
                    })
                    .collect();
                eprintln!(
                    "tick {} ent {ent} {:?} {class} props {} delta {:?} {}",
                    self.server_tick,
                    e.update_type,
                    e.props.len(),
                    pe.delta,
                    props.join(" ")
                );
            }
            if pe.removed_entities.iter().any(|id| u32::from(*id) == ent) {
                eprintln!("tick {} ent {ent} removed", self.server_tick);
            }
        }
        for id in &pe.removed_entities {
            self.forget_entity(u32::from(*id));
        }
        for e in &pe.entities {
            self.entity(e, state);
        }
        self.track_frame(pe);
        for p in &self.gs.state.players {
            if alive(p) {
                self.ents.last_alive.insert(u32::from(p.entity), Pose::of(p));
            }
        }
        self.medic_changes();
        for (user, mut spawn) in std::mem::take(&mut self.pending_spawns) {
            spawn.pos = self.user_pos(user);
            self.push(Line::Spawn(spawn));
        }
    }

    fn entity(&mut self, e: &PacketEntity, state: &ParserState) {
        let idx = u32::from(e.entity_index);
        if e.update_type == UpdateType::Delete {
            self.forget_entity(idx);
            return;
        }
        // An entity leaving SourceTV's view is not updated (or deleted) any more;
        // tf-demo-parser keeps projectiles frozen where they were.
        if e.update_type == UpdateType::Leave {
            self.gs.state.projectiles.remove(&e.entity_index);
            return;
        }
        // Every entity class has its own `moveparent` prop, so this one goes by name.
        if let Some(parent) =
            e.props.iter().find(|p| p.identifier.names().is_some_and(|(_, n)| n.as_str() == "moveparent"))
        {
            match opt_handle(as_int(&parent.value)) {
                None => self.ents.parents.remove(&idx),
                Some(p) => self.ents.parents.insert(idx, p),
            };
        }
        let class = state.server_classes.get(usize::from(e.server_class));
        if matches!(class.map(|c| c.name.as_str()), Some("CPhysicsProp" | "CDynamicProp" | "CPhysicsPropMultiplayer"))
            && let Some(v) = prop_vec(&e.props, PROP_ORIGIN)
        {
            self.ents.prop_origins.insert(idx, v);
        }
        // A slot taken over by an entity of another class (an implicit delete); tf-demo-parser
        // keeps the old projectile or building, and applied this update to the old projectile:
        // a new projectile is built again from it, so it keeps its team and owner.
        if self.gs.state.projectiles.get(&e.entity_index).is_some_and(|p| p.class != e.server_class) {
            self.gs.state.projectiles.remove(&e.entity_index);
            if class.is_some_and(|c| {
                c.name.starts_with("CTFProjectile_") || c.name.as_str() == "CTFGrenadePipebombProjectile"
            }) {
                self.gs.handle_entity(e, state);
            }
        }
        let Some(class) = class else { return };
        if e.update_type == UpdateType::Enter {
            // A slot reused without a delete: what was there is gone (an Enter carries the full state).
            self.modes.forget(idx);
            self.ents.flags.remove(&idx);
            self.ents.sentries.remove(&idx);
            let same = |b: &Building| {
                matches!(
                    (b, class.name.as_str()),
                    (Building::Sentry(_), "CObjectSentrygun")
                        | (Building::Dispenser(_), "CObjectDispenser")
                        | (Building::Teleporter(_), "CObjectTeleporter")
                )
            };
            if self.gs.state.buildings.get(&e.entity_index).is_some_and(|b| !same(b)) {
                self.gs.state.buildings.remove(&e.entity_index);
            }
        }
        if debug(class.name.as_str()) {
            for p in e.props(state) {
                eprintln!("{}#{idx} {:?} = {:?}", class.name.as_str(), p.identifier.names(), p.value);
            }
        }
        if self.is_weapon_class(usize::from(e.server_class), state) {
            // The baseline only matters on Enter; later updates are deltas.
            if e.update_type == UpdateType::Enter {
                // A reused slot may have had no Delete.
                self.ents.weapons.remove(&idx);
                self.on_weapon(idx, e.props(state).map(|p| (p.identifier, as_int(&p.value))));
            } else {
                self.on_weapon(idx, e.props.iter().map(|p| (p.identifier, as_int(&p.value))));
            }
        }
        let props = e.props(state);
        match class.name.as_str() {
            "CTFPlayer" => self.on_player(idx, props),
            "CTFPlayerResource" => self.on_player_resource(props),
            "CTFTeam" => self.on_team(idx, props),
            "CTeamRoundTimer" => self.on_round_timer(idx, props),
            "CObjectSentrygun" => {
                self.building_kinds.insert(idx, BuildingKind::Sentry);
                self.on_sentry(idx, props);
            }
            "CObjectDispenser" => {
                self.building_kinds.insert(idx, BuildingKind::Dispenser);
            }
            "CObjectTeleporter" => {
                self.building_kinds.insert(idx, BuildingKind::Teleporter);
            }
            "CTeamTrainWatcher" => self.on_train_watcher(idx, props),
            "CCaptureFlag" => self.on_flag(idx, props),
            "CTFGameRulesProxy" => self.on_game_rules(props),
            "CWeaponMedigun" => self.on_medigun(idx, props),
            "CTFFlameThrower" => self.on_flamethrower(idx, props),
            "CTFObjectiveResource" => self.on_objective_resource(props),
            other => self.modes.entity(other, idx, props),
        }
    }

    /// Any weapon class carries these props.
    fn on_weapon(&mut self, idx: u32, props: impl Iterator<Item = (SendPropIdentifier, i64)>) {
        for (id, v) in props {
            match id {
                PROP_ITEM_DEFINITION => self.weapon(idx).item = v,
                PROP_CLIP => {
                    let w = self.weapon(idx);
                    // networked as value + 1 so -1 ("no clip") fits unsigned
                    w.clip = v - 1;
                }
                _ => {}
            }
        }
    }

    fn is_weapon_class(&mut self, class: usize, state: &ParserState) -> bool {
        if self.weapon_classes.len() <= class {
            self.weapon_classes.resize(class + 1, None);
        }
        *self.weapon_classes[class].get_or_insert_with(|| {
            state
                .send_tables
                .get(class)
                .is_some_and(|t| t.flattened_props.iter().any(|d| d.identifier == PROP_ITEM_DEFINITION))
        })
    }

    fn weapon(&mut self, idx: u32) -> &mut Weapon {
        self.ents.weapons.entry(idx).or_insert(Weapon { item: 0, clip: NO_CLIP })
    }

    fn on_player(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let words = self.ents.conds.entry(idx).or_default();
        for p in props {
            if let Some(i) = PROP_CONDS.iter().position(|id| *id == p.identifier) {
                // A bit field: the bits are wanted, not the number.
                words[i] = as_int(&p.value) as u32;
            } else if p.identifier == PROP_ACTIVE_WEAPON {
                match opt_handle(as_int(&p.value)) {
                    Some(w) => self.ents.active_weapons.insert(idx, w),
                    None => self.ents.active_weapons.remove(&idx),
                };
            } else if p.identifier == PROP_CARRYING {
                self.stats.entry(idx).or_default().carrying = as_int(&p.value) != 0;
            } else if p.identifier == PROP_CARRIED {
                match opt_handle(as_int(&p.value)) {
                    Some(b) => self.carried.insert(idx, b),
                    None => self.carried.remove(&idx),
                };
            } else if let Some(slot) = PROP_MY_WEAPONS.iter().position(|id| *id == p.identifier) {
                self.ents.my_weapons.entry(idx).or_insert([None; MY_WEAPON_SLOTS])[slot] = opt_handle(as_int(&p.value));
            }
        }
    }

    /// Arrays indexed by player entity, one table per value.
    fn on_player_resource(&mut self, props: impl Iterator<Item = SendProp>) {
        let mut restarted = HashSet::new();
        for p in props {
            let Some((table, name)) = p.identifier.names() else { continue };
            let Ok(ent) = name.as_str().parse::<u32>() else { continue };
            let v = as_int(&p.value);
            let st = self.stats.entry(ent).or_default();
            // The match totals going down is the match restarting (after the warm-up, say).
            let mut total = |old: &mut i64| {
                if v < *old {
                    restarted.insert(ent);
                }
                *old = v;
            };
            match table.as_str() {
                "m_iTotalScore" => total(&mut st.points),
                "m_iScore" => total(&mut st.kills),
                "m_iDeaths" => total(&mut st.deaths),
                "m_iActiveDominations" => st.dominations = v,
                "m_iStreaks" => st.streak = v,
                "m_flNextRespawnTime" => st.respawn_at = as_float(&p.value),
                _ => {}
            }
        }
        for ent in restarted {
            if let Some(st) = self.stats.get_mut(&ent) {
                (st.damage, st.healing, st.assists) = (0, 0, 0);
            }
        }
    }

    fn on_team(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let t = self.ents.teams.entry(idx).or_default();
        for p in props {
            match p.identifier {
                PROP_TEAM_NUM | PROP_TEAM_TEAM_NUM => t.team = team_byte(as_int(&p.value)),
                PROP_TEAM_SCORE => t.score = as_int(&p.value),
                PROP_FLAG_CAPTURES => t.captures = as_int(&p.value),
                PROP_TEAM_ROLE => t.role = as_int(&p.value),
                _ => {}
            }
        }
    }

    fn on_round_timer(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let t = self.ents.timers.entry(idx).or_default();
        for p in props {
            let f = as_float(&p.value);
            match p.identifier {
                PROP_TEAM_NUM => t.team = team_byte(as_int(&p.value)),
                PROP_TIMER_END_TIME => t.end_time = f,
                PROP_TIMER_REMAINING => t.remaining = f,
                PROP_TIMER_PAUSED => t.paused = f != 0.0,
                PROP_TIMER_SHOW_IN_HUD => t.show = f != 0.0,
                PROP_TIMER_DISABLED => t.disabled = f != 0.0,
                PROP_TIMER_STATE => t.setup = f == 0.0,
                PROP_TIMER_STOPWATCH => t.stopwatch = f != 0.0,
                PROP_TIMER_WATCHING_CAPS => t.watching_caps = f != 0.0,
                PROP_TIMER_TOTAL => t.total = f,
                _ => {}
            }
        }
    }

    fn on_sentry(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let sentry = self.ents.sentries.entry(idx).or_default();
        let mut fired = 0;
        for p in props {
            let v = as_int(&p.value);
            match p.identifier {
                PROP_SENTRY_POSE_PITCH => sentry.pose[0] = as_float(&p.value),
                PROP_SENTRY_POSE_YAW => sentry.pose[1] = as_float(&p.value),
                PROP_ROTATION => {
                    if let SendPropValue::Vector(a) = p.value {
                        sentry.yaw = a.y;
                    }
                }
                PROP_SENTRY_ENEMY => sentry.enemy = opt_handle(v),
                PROP_SENTRY_STATE => sentry.state = v,
                PROP_SENTRY_SHELLS => {
                    if let Some(before) = sentry.shells
                        && v < before
                    {
                        fired = before - v;
                    }
                    sentry.shells = Some(v);
                }
                _ => {}
            }
        }
        if fired > 0 {
            let target = sentry.enemy;
            let builder = self.gs.state.buildings.get(&EntityId::from(idx)).map(Building::builder);
            self.push(Line::SentryShot(out::SentryShot {
                tick: self.server_tick,
                sentry: idx,
                builder: builder.and_then(|b| self.user_of_entity(builder_entity(b))),
                target: target.and_then(|t| self.user_of_entity(t)),
                shells: fired,
            }));
        }
    }

    fn on_train_watcher(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let w = self.ents.train_watchers.entry(idx).or_default();
        for p in props {
            let v = as_int(&p.value);
            match p.identifier {
                PROP_TEAM_NUM => w.team = team_byte(v),
                PROP_WATCHER_PROGRESS => w.progress = as_float(&p.value),
                PROP_WATCHER_CAPPERS => w.cappers = v,
                PROP_WATCHER_SPEED => w.speed = v,
                PROP_WATCHER_GLOW => w.glow = opt_handle(v),
                _ => {}
            }
        }
    }

    fn on_flag(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let f = self.ents.flags.entry(idx).or_default();
        for p in props {
            modes::placed(&mut f.at, &p);
            let v = as_int(&p.value);
            match p.identifier {
                PROP_FLAG_STATUS => f.status = flag_status(v),
                PROP_FLAG_DISABLED => f.disabled = v != 0,
                PROP_OWNER_ENTITY => f.carrier = opt_handle(v),
                PROP_FLAG_RESET_TIME => f.reset_time = as_float(&p.value),
                PROP_FLAG_POINTS => f.points = v,
                PROP_EFFECTS => f.hidden = v & EF_NODRAW != 0,
                _ => {}
            }
        }
    }

    fn on_game_rules(&mut self, props: impl Iterator<Item = SendProp>) {
        for p in props {
            match p.identifier {
                PROP_ROUND_STATE => self.round_state = round_from_raw(as_int(&p.value)),
                PROP_RED_KOTH_TIMER => self.koth_timers[RED_SLOT] = opt_handle(as_int(&p.value)),
                PROP_BLUE_KOTH_TIMER => self.koth_timers[BLUE_SLOT] = opt_handle(as_int(&p.value)),
                PROP_MAP_RESET_TIME => self.match_start = as_float(&p.value),
                PROP_STATE_TRANSITION_TIME => self.state_transition = as_float(&p.value),
                PROP_STOPWATCH => self.stopwatch.on = as_int(&p.value) != 0,
                PROP_STOPWATCH_STATE => self.stopwatch.state = as_int(&p.value),
                _ => {}
            }
        }
        self.track_round_time();
    }

    fn on_medigun(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let m = self.ents.mediguns.entry(idx).or_default();
        for p in props {
            let v = as_int(&p.value);
            match p.identifier {
                PROP_WEAPON_OWNER => m.owner = opt_handle(v),
                PROP_ITEM_DEFINITION => m.item = v,
                PROP_MEDIGUN_RELEASING => m.releasing = v != 0,
                PROP_MEDIGUN_RESIST => m.resist = v,
                PROP_MEDIGUN_TARGET => m.target = opt_handle(v),
                _ => {}
            }
        }
    }

    fn on_flamethrower(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let f = self.ents.flamethrowers.entry(idx).or_default();
        let before = f.state;
        for p in props {
            let v = as_int(&p.value);
            match p.identifier {
                PROP_WEAPON_OWNER => f.owner = opt_handle(v),
                PROP_FLAMETHROWER_STATE => f.state = v,
                PROP_FLAMETHROWER_CRIT => f.crit = v != 0,
                PROP_FLAMETHROWER_HIT => f.hit = v != 0,
                _ => {}
            }
        }
        if f.state == FLAMETHROWER_AIRBLAST
            && before != FLAMETHROWER_AIRBLAST
            && let Some(player) = f.owner.and_then(|o| self.user_of_entity(o))
        {
            self.push(Line::Airblast(Airblast { tick: self.server_tick, player }));
        }
    }

    fn on_objective_resource(&mut self, props: impl Iterator<Item = SendProp>) {
        // Capping-team changes reset progress, so they go before the cap percentages.
        let mut props: Vec<SendProp> = props.collect();
        props.sort_by_key(|p| p.identifier.names().is_none_or(|(t, _)| t.as_str() != "m_iCappingTeam"));
        for p in &props {
            self.objective_prop(p);
            self.modes.objective_prop(p);
        }
    }
}
