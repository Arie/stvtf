//! Turns parser state into the lines the overview consumes: periodic snapshots
//! plus discrete events (kills, damage, shots, explosions, ubers, rounds, chat...).

use super::detail;
use super::modes::Modes;
use super::props::RED_SLOT;
use crate::DEFAULT_TICK_INTERVAL;
use crate::output::{self as out, BuildingKind, Line, MedigunKind, PlayerId, Position, ProjectileKind, Round};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::LazyLock;
use tf_demo_parser::ParserState;
use tf_demo_parser::demo::data::DemoTick;
use tf_demo_parser::demo::data::game_state::{GameState, Player, PlayerState, Projectile};
use tf_demo_parser::demo::gamevent::GameEventType;
use tf_demo_parser::demo::header::Header;
use tf_demo_parser::demo::message::{Message, MessageType, ServerInfoMessage, SetConVarMessage};
use tf_demo_parser::demo::packet::datatable::{ParseSendTable, ServerClass};
use tf_demo_parser::demo::packet::message::MessagePacketMeta;
use tf_demo_parser::demo::packet::stringtable::StringTableEntry;
use tf_demo_parser::demo::parser::MessageHandler;
use tf_demo_parser::demo::parser::analyser::Class;
use tf_demo_parser::demo::parser::gamestateanalyser::GameStateAnalyser;
use tf_demo_parser::demo::parser::handler::BorrowMessageHandler;
use tf_demo_parser::demo::sendprop::SendPropIdentifier;
use tf_demo_parser::demo::vector::Vector;

mod convert;
mod entities;
mod events;
mod snapshot;
mod state;
#[cfg(test)]
mod tests;

pub(super) use convert::{building_kind, opt_user, strip_colors, team_num, xyz};
use convert::{medigun_kind, player_id, projectile_kind, resist, uber_charge};
use state::{
    Charge, ControlPoint, Flag, Flamethrower, Medigun, Pose, RoundTimer, Sentry, Stats, TeamScore, TrainWatcher, Weapon,
};

/// `CTFObjectiveResource` has room for this many control points, and its per team arrays for
/// this many teams: element `team * MAX_CONTROL_POINTS + point` (`TEAM_ARRAY`).
const MAX_CONTROL_POINTS: usize = 8;
const MAX_TEAMS: usize = 4;
/// A player's condition words: `m_nPlayerCond` and `_condition_bits` (both conditions 0-31),
/// then `m_nPlayerCondEx..Ex4`, 32 conditions each.
const COND_WORDS: usize = 6;
const COND_BITS: usize = 32;
/// A medigun's charge when full, in percent.
const FULL_CHARGE: u8 = 100;
const SECONDS_PER_MINUTE: f32 = 60.0;

/// `STVTF_DEBUG`, read once: a server class, temp entity class, `ent:N` or `gameevents`.
static DEBUG: LazyLock<Option<String>> = LazyLock::new(|| std::env::var("STVTF_DEBUG").ok());

/// Whether `STVTF_DEBUG` asks for this topic.
pub(crate) fn debug(topic: &str) -> bool {
    DEBUG.as_deref() == Some(topic)
}

/// The entity `STVTF_DEBUG=ent:N` asks to trace.
fn debug_entity() -> Option<u32> {
    DEBUG.as_deref()?.strip_prefix("ent:")?.parse().ok()
}

const MY_WEAPON_SLOTS: usize = 48;
static PROP_MY_WEAPONS: LazyLock<[SendPropIdentifier; MY_WEAPON_SLOTS]> =
    LazyLock::new(|| std::array::from_fn(|i| SendPropIdentifier::new("m_hMyWeapons", &format!("{i:03}"))));
/// `m_iWeaponState`: idle, starting to fire, firing, airblast.
const FLAMETHROWER_STARTING: i64 = 1;
const FLAMETHROWER_FIRING: i64 = 2;
const FLAMETHROWER_AIRBLAST: i64 = 3;
/// `m_nChargeResistType`: bullet, blast, fire.
const RESIST_BULLET: i64 = 0;
const RESIST_BLAST: i64 = 1;

/// The per-entity maps, declared once so that forgetting an entity forgets it in each of them.
macro_rules! per_entity {
    ($($(#[$doc:meta])* $field:ident: $map:ty,)*) => {
        #[derive(Default)]
        struct Entities {
            $($(#[$doc])* $field: $map,)*
        }

        impl Entities {
            fn forget(&mut self, idx: u32) {
                $(self.$field.remove(&idx);)*
            }
        }
    };
}

per_entity! {
    /// Condition words per player entity: `m_nPlayerCond`, _`condition_bits`, `m_nPlayerCondEx..Ex4`.
    /// (tf-demo-parser 0.6.4's own `conditions()` decodes these wrongly.)
    conds: HashMap<u32, [u32; COND_WORDS]>,
    mediguns: HashMap<u32, Medigun>,
    flamethrowers: HashMap<u32, Flamethrower>,
    weapons: HashMap<u32, Weapon>,
    active_weapons: HashMap<u32, u32>,
    my_weapons: HashMap<u32, [Option<u32>; MY_WEAPON_SLOTS]>,
    teams: BTreeMap<u32, TeamScore>,
    timers: BTreeMap<u32, RoundTimer>,
    /// Where each player entity last was while alive: a dead player's own position is
    /// their spectator camera, which follows teammates.
    last_alive: HashMap<u32, Pose>,
    /// Per medic entity, to report changes.
    medic_charge: HashMap<u32, Charge>,
    /// Payload: tf-demo-parser takes every `func_tracktrain` for a cart, but only a payload
    /// map has a `team_train_watcher` (`KotH` and other maps use trains for lifts and props).
    train_watchers: BTreeMap<u32, TrainWatcher>,
    /// Entity -> the entity it is attached to (`moveparent`), to find a watcher's cart.
    parents: HashMap<u32, u32>,
    /// World positions of prop entities: a payload cart can be a free physics prop.
    prop_origins: HashMap<u32, Vector>,
    sentries: HashMap<u32, Sentry>,
    /// CTF flags and the bball ball (`item_teamflag`), by entity.
    flags: BTreeMap<u32, Flag>,
}

#[derive(Default)]
pub(crate) struct Extractor {
    gs: GameStateAnalyser,
    /// Latest server tick seen in a `net_Tick`.
    pub(crate) server_tick: u32,
    events: RefCell<Vec<Line>>,
    world_sent: Cell<bool>,
    /// A delta came from a frame never seen: the entities may be missing what it held.
    lost_frame: Cell<bool>,
    control_points: Vec<ControlPoint>,
    num_control_points: usize,
    /// Per server class id: whether its send table has an item definition index.
    weapon_classes: Vec<Option<bool>>,
    class_names: Vec<String>,
    /// Seconds per tick from `ServerInfo`.
    tick_interval: Option<f32>,
    /// King of the hill: each team's own timer, which runs while that team holds the point.
    koth_timers: [Option<u32>; 2],
    round_state: Round,
    /// Server time the match started (`m_flMapResetTime`) and the current round went live.
    match_start: f32,
    round_live_since: Option<f32>,
    /// When the live round ended (win, stalemate): its time stops there.
    round_ended_at: Option<f32>,
    state_transition: f32,
    /// `mp_timelimit` (minutes) and `mp_winlimit`, replicated to clients as convars.
    time_limit: Option<f32>,
    win_limit: Option<i64>,
    /// `player_spawn` arrives before the player's new position: held until the next entity update.
    pending_spawns: Vec<(u16, out::Spawn)>,
    /// The entities in each recent frame, by server tick, to find the ones a delta drops silently.
    frames: VecDeque<(u32, HashSet<u32>)>,
    /// Players who disconnected: the server keeps their slot, and so their entity and info, for
    /// a while after. A user id is not reused while the server runs.
    left: HashSet<u16>,
    /// Scoreboard lines from the player resource, by player entity.
    stats: HashMap<u32, Stats>,
    /// Engineers carrying a building: player entity -> the building's entity.
    carried: HashMap<u32, u32>,
    /// What each building entity is: a carried building is not sent while it is carried.
    building_kinds: HashMap<u32, BuildingKind>,
    /// What is known about each entity, forgotten together when it goes.
    ents: Entities,
    /// Pass time, robot and player destruction, Mann vs. Machine.
    modes: Modes,
    raw_names: detail::RawNames,
}

impl Extractor {
    fn state(&self) -> &GameState {
        &self.gs.state
    }

    /// Seconds per server tick.
    pub(crate) fn interval(&self) -> f32 {
        self.tick_interval.unwrap_or(DEFAULT_TICK_INTERVAL)
    }

    fn projectile_kind(&self, pr: &Projectile) -> ProjectileKind {
        projectile_kind(pr.ty, self.class_names.get(usize::from(pr.class)).map_or("", String::as_str))
    }

    /// While running, the last state transition is when the round went live; a win or
    /// stalemate stops its clock and the next pre-round clears it.
    fn track_round_time(&mut self) {
        let now = self.server_time();
        match self.round_state {
            Round::Running => (self.round_live_since, self.round_ended_at) = (Some(self.state_transition), None),
            Round::Preround => (self.round_live_since, self.round_ended_at) = (None, None),
            _ if self.round_live_since.is_some() && self.round_ended_at.is_none() => self.round_ended_at = Some(now),
            _ => {}
        }
    }

    /// Seconds left of `mp_timelimit`, which counts from the start of the match.
    fn time_left(&self) -> Option<f32> {
        let limit = self.time_limit?;
        (self.match_start > 0.0)
            .then(|| (limit * SECONDS_PER_MINUTE - (self.server_time() - self.match_start)).max(0.0))
    }

    /// Server time at the latest tick, in seconds: what the game's own times count in.
    fn server_time(&self) -> f32 {
        self.server_tick as f32 * self.interval()
    }

    fn round_time(&self) -> Option<f32> {
        let now = self.server_time();
        self.round_live_since.map(|t| (self.round_ended_at.unwrap_or(now) - t).max(0.0))
    }

    /// `uber_ready` when a medic's charge fills up, `uber_ended` when a released charge runs out.
    fn medic_changes(&mut self) {
        let mut events = Vec::new();
        for p in &self.gs.state.players {
            let ent = u32::from(p.entity);
            let Some(info) = p.info.as_ref() else { continue };
            if p.class != Class::Medic || p.state != PlayerState::Alive {
                self.ents.medic_charge.remove(&ent);
                continue;
            }
            let gun = self.medigun_owned_by(ent);
            let now = Charge { charge: uber_charge(p), releasing: gun.is_some_and(|m| m.releasing) };
            if let Some(&before) = self.ents.medic_charge.get(&ent) {
                let charge = out::MedicCharge {
                    tick: self.server_tick,
                    medic: PlayerId(u16::from(info.user_id)),
                    medigun: gun.map(|m| medigun_kind(m.item)),
                };
                if before.charge < FULL_CHARGE && now.charge >= FULL_CHARGE && !now.releasing {
                    events.push(Line::UberReady(charge.clone()));
                }
                if before.releasing && !now.releasing {
                    events.push(Line::UberEnded(charge));
                }
            }
            self.ents.medic_charge.insert(ent, now);
        }
        for e in events {
            self.push(e);
        }
    }

    /// The entities `track_frame` drops when a frame leaves them out.
    fn tracked(&self) -> impl Iterator<Item = u32> + '_ {
        let state = &self.gs.state;
        (state.projectiles.keys().chain(state.buildings.keys()).map(|id| u32::from(*id)))
            .chain(self.ents.flags.keys().copied())
            .chain(self.modes.ids())
    }

    fn forget_entity(&mut self, idx: u32) {
        self.ents.forget(idx);
        self.modes.forget(idx);
    }

    fn player_by_user(&self, user_id: u16) -> Option<&Player> {
        self.state().players.iter().find(|p| p.info.as_ref().is_some_and(|i| u16::from(i.user_id) == user_id))
    }

    fn player_by_entity(&self, ent: u32) -> Option<&Player> {
        self.state().players.iter().find(|p| u32::from(p.entity) == ent)
    }

    fn entity_of_user(&self, user_id: u16) -> Option<u32> {
        self.player_by_user(user_id).map(|p| u32::from(p.entity))
    }

    fn medigun_owned_by(&self, player_ent: u32) -> Option<&Medigun> {
        let held = self.ents.my_weapons.get(&player_ent)?;
        held.iter().flatten().find_map(|w| self.ents.mediguns.get(w).filter(|m| m.owner == Some(player_ent)))
    }

    /// The team of a round timer: on king of the hill each team has its own.
    fn timer_team(&self, idx: u32) -> out::Team {
        match self.koth_timers.iter().position(|k| *k == Some(idx)) {
            Some(RED_SLOT) => out::Team::Red,
            Some(_) => out::Team::Blue,
            None => self.ents.timers.get(&idx).map_or(out::Team::Unassigned, |t| team_num(t.team)),
        }
    }

    /// Runs `f` with what the detail lines look up in the extractor.
    fn with_lookup<R>(&self, f: impl FnOnce(&detail::Lookup<'_>) -> R) -> R {
        let user_of_entity = |ent: u32| self.user_of_entity(ent);
        let class_of_user = |id: u16| self.player_by_user(id).map(|p| p.class);
        f(&detail::Lookup {
            user_of_entity: &user_of_entity,
            class_of_user: &class_of_user,
            tick: self.server_tick,
            raw_names: &self.raw_names,
        })
    }

    /// The control point at `index`, made when not seen yet; its progress rolled forward to now.
    fn control_point_mut(&mut self, index: usize) -> &mut ControlPoint {
        if self.control_points.len() <= index {
            self.control_points.resize(index + 1, ControlPoint::default());
        }
        let (tick, interval) = (self.server_tick, self.interval());
        let cp = &mut self.control_points[index];
        cp.advance(tick, interval);
        cp
    }

    fn weapon_of(&self, player_ent: u32) -> Option<out::Weapon> {
        let w = self.ents.weapons.get(self.ents.active_weapons.get(&player_ent)?)?;
        Some(out::Weapon { item: w.item, clip: (w.clip >= 0).then_some(w.clip) })
    }

    fn medigun_of(&self, player_ent: u32) -> Option<out::Medigun> {
        let m = self.medigun_owned_by(player_ent)?;
        let kind = medigun_kind(m.item);
        Some(out::Medigun {
            kind,
            releasing: m.releasing,
            resist: (kind == MedigunKind::Vaccinator).then(|| resist(m.resist)),
            target: m.target.and_then(|t| self.user_of_entity(t)),
        })
    }

    /// A pyro's flamethrower while it is firing.
    fn flames_of(&self, player_ent: u32) -> Option<out::Flames> {
        let f =
            self.ents.flamethrowers.values().find(|f| {
                f.owner == Some(player_ent) && matches!(f.state, FLAMETHROWER_STARTING | FLAMETHROWER_FIRING)
            })?;
        Some(out::Flames { crit: f.crit, hit: f.hit })
    }

    /// Queues an event.
    fn push(&self, line: Line) {
        self.events.borrow_mut().push(line);
    }

    /// Takes the queued events.
    pub(crate) fn drain(&self) -> Vec<Line> {
        self.events.take()
    }

    /// Whether a delta came from a frame never seen since this was last asked.
    pub(crate) fn take_lost_frame(&self) -> bool {
        self.lost_frame.take()
    }

    fn user_pos(&self, user_id: u16) -> Option<Position> {
        self.player_by_user(user_id).map(|p| xyz(p.position))
    }

    fn user_of_entity(&self, ent: u32) -> Option<PlayerId> {
        self.player_by_entity(ent).and_then(player_id)
    }

    fn convars(&mut self, c: &SetConVarMessage) {
        let dbg = debug("convars");
        for v in &c.vars {
            if dbg {
                // Debug formatting escapes control characters from the server.
                eprintln!("convar {:?} = {:?}", v.key, v.value);
            }
            match v.key.as_str() {
                "mp_timelimit" => self.time_limit = v.value.parse().ok().filter(|m: &f32| *m > 0.0),
                "mp_winlimit" => self.win_limit = v.value.parse().ok().filter(|w: &i64| *w > 0),
                _ => {}
            }
        }
    }

    fn server_info(&mut self, info: &ServerInfoMessage) {
        if info.interval_per_tick > 0.0 {
            self.tick_interval = Some(info.interval_per_tick);
        }
        self.push(Line::Server(out::Server {
            tick: self.server_tick,
            map: info.map.clone(),
            hostname: info.server_name.clone(),
            // Widened, as the output has always written it (0.014999999664723873, not 0.015).
            tick_interval: f64::from(info.interval_per_tick),
            max_players: info.max_player_count,
            spawn_count: info.server_count,
        }));
    }
}

impl MessageHandler for Extractor {
    type Output = Self;

    fn does_handle(t: MessageType) -> bool {
        GameStateAnalyser::does_handle(t)
            || matches!(
                t,
                MessageType::NetTick
                    | MessageType::SignOnState
                    | MessageType::TempEntities
                    | MessageType::UserMessage
                    | MessageType::ServerInfo
                    | MessageType::SetConVar
                    | MessageType::GameEventList
            )
    }

    fn handle_header(&mut self, header: &Header) {
        self.gs.handle_header(header);
    }

    fn handle_message(&mut self, message: &Message<'_>, tick: DemoTick, state: &ParserState) {
        if GameStateAnalyser::does_handle(message.get_message_type()) {
            self.gs.handle_message(message, tick, state);
        }
        match message {
            Message::PacketEntities(pe) => self.packet_entities(pe, state),
            Message::NetTick(t) => self.server_tick = u32::from(t.tick),
            Message::SetConVar(c) => self.convars(c),
            Message::ServerInfo(info) => self.server_info(info),
            Message::GameEvent(e) => {
                if debug("gameevents") {
                    eprintln!("gameevent {:?}", e.event.event_type());
                }
                self.game_event(&e.event);
            }
            Message::TempEntities(te) => self.temp_entities(te, state),
            Message::UserMessage(m) => self.user_message(m),
            Message::GameEventList(list) => {
                self.raw_names = list
                    .event_list
                    .iter()
                    .filter_map(|d| match &d.event_type {
                        GameEventType::Unknown(name) => {
                            Some((name.clone(), d.entries.iter().map(|e| e.hash).collect()))
                        }
                        _ => None,
                    })
                    .collect();
            }
            _ => {}
        }
    }

    fn handle_string_entry(&mut self, table: &str, index: usize, entry: &StringTableEntry<'_>, state: &ParserState) {
        self.gs.handle_string_entry(table, index, entry, state);
    }

    fn handle_data_tables(&mut self, tables: &[ParseSendTable], classes: &[ServerClass], state: &ParserState) {
        self.class_names = classes.iter().map(|c| c.name.as_str().to_string()).collect();
        self.gs.handle_data_tables(tables, classes, state);
    }

    fn handle_packet_meta(&mut self, tick: DemoTick, meta: &MessagePacketMeta, state: &ParserState) {
        self.gs.handle_packet_meta(tick, meta, state);
    }

    fn into_output(self, _state: &ParserState) -> Self::Output {
        self
    }
}

impl BorrowMessageHandler for Extractor {
    fn borrow_output(&self, _state: &ParserState) -> &Self::Output {
        self
    }
}
