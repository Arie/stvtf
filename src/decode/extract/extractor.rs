//! The extractor: turns the game's messages and entities into the output lines of the match.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

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
use tf_demo_parser::demo::vector::Vector;

use super::super::detail;
use super::super::modes::Modes;
use super::super::props::RED_SLOT;
use super::convert::{medigun_kind, player_id, projectile_kind, resist, uber_charge};
pub(super) use super::convert::{team_num, xyz};
use super::debug;
use super::limits::{
    COND_WORDS, FLAMETHROWER_FIRING, FLAMETHROWER_STARTING, FULL_CHARGE, MY_WEAPON_SLOTS, SECONDS_PER_MINUTE,
};
use super::state::{
    Charge, ControlPoint, Flag, Flamethrower, Medigun, Pose, RoundTimer, Sentry, Stats, StopwatchRules, TeamScore,
    TrainWatcher, Weapon,
};
use crate::DEFAULT_TICK_INTERVAL;
use crate::output::{self as out, BuildingKind, Line, MedigunKind, PlayerId, Position, ProjectileKind, Round};

/// The per-entity maps, declared once so that forgetting an entity forgets it in each of them.
macro_rules! per_entity {
    ($($(#[$doc:meta])* $field:ident: $map:ty,)*) => {
        #[derive(Default)]
        pub(super) struct Entities {
            $($(#[$doc])* pub(super) $field: $map,)*
        }

        impl Entities {
            pub(super) fn forget(&mut self, idx: u32) {
                $(self.$field.remove(&idx);)*
            }
        }
    };
}

per_entity! {
    /// Condition words per player entity (tf-demo-parser's own `conditions()` decodes them
    /// wrongly).
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
    /// The last charge seen per medic entity, to report changes.
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
    pub(super) gs: GameStateAnalyser,
    /// Latest server tick seen in a `net_Tick`.
    pub(crate) server_tick: u32,
    pub(super) events: RefCell<Vec<Line>>,
    pub(super) world_sent: Cell<bool>,
    /// A delta came from a frame never seen: the entities may be missing what it held.
    pub(super) lost_frame: Cell<bool>,
    pub(super) control_points: Vec<ControlPoint>,
    pub(super) num_control_points: usize,
    /// Per server class id: whether its send table has an item definition index.
    pub(super) weapon_classes: Vec<Option<bool>>,
    pub(super) class_names: Vec<String>,
    /// Seconds per tick from `ServerInfo`.
    pub(super) tick_interval: Option<f32>,
    /// The server's player slots, once it said.
    pub(super) max_players: u8,
    /// King of the hill: each team's own timer, which runs while that team holds the point.
    pub(super) koth_timers: [Option<u32>; 2],
    pub(super) stopwatch: StopwatchRules,
    pub(super) round_state: Round,
    /// Server time the match started (`m_flMapResetTime`) and the current round went live.
    pub(super) match_start: f32,
    pub(super) round_live_since: Option<f32>,
    /// When the live round ended (win, stalemate): its time stops there.
    pub(super) round_ended_at: Option<f32>,
    pub(super) state_transition: f32,
    /// `mp_timelimit` (minutes) and `mp_winlimit`, replicated to clients as convars.
    pub(super) time_limit: Option<f32>,
    pub(super) win_limit: Option<i64>,
    /// `player_spawn` arrives before the player's new position: held until the next entity update.
    pub(super) pending_spawns: Vec<(u16, out::Spawn)>,
    /// The entities in each recent frame, by server tick, to find the ones a delta drops silently.
    pub(super) frames: VecDeque<(u32, HashSet<u32>)>,
    /// Players who disconnected: the server keeps their slot, and so their entity and info, for
    /// a while after. A user id is not reused while the server runs.
    pub(super) left: HashSet<u16>,
    /// Whether the game is paused, as last told.
    pub(super) paused: bool,
    /// Scoreboard lines from the player resource, by player entity.
    pub(super) stats: HashMap<u32, Stats>,
    /// Engineers carrying a building: player entity -> the building's entity.
    pub(super) carried: HashMap<u32, u32>,
    /// What each building entity is: a carried building is not sent while it is carried.
    pub(super) building_kinds: HashMap<u32, BuildingKind>,
    /// What is known about each entity, forgotten together when it goes.
    pub(super) ents: Entities,
    /// Pass time, robot and player destruction, Mann vs. Machine.
    pub(super) modes: Modes,
    pub(super) raw_names: detail::RawNames,
}

impl Extractor {
    pub(super) fn state(&self) -> &GameState {
        &self.gs.state
    }

    /// Seconds per server tick.
    pub(crate) fn interval(&self) -> f32 {
        self.tick_interval.unwrap_or(DEFAULT_TICK_INTERVAL)
    }

    pub(super) fn projectile_kind(&self, pr: &Projectile) -> ProjectileKind {
        projectile_kind(pr.ty, self.class_names.get(usize::from(pr.class)).map_or("", String::as_str))
    }

    /// While running, the last state transition is when the round went live; a win or
    /// stalemate stops its clock and the next pre-round clears it.
    pub(super) fn track_round_time(&mut self) {
        let now = self.server_time();
        match self.round_state {
            Round::Running => (self.round_live_since, self.round_ended_at) = (Some(self.state_transition), None),
            Round::Preround => (self.round_live_since, self.round_ended_at) = (None, None),
            _ if self.round_live_since.is_some() && self.round_ended_at.is_none() => self.round_ended_at = Some(now),
            _ => {}
        }
    }

    /// Seconds left of `mp_timelimit`, which counts from the start of the match.
    pub(super) fn time_left(&self) -> Option<f32> {
        let limit = self.time_limit?;
        (self.match_start > 0.0)
            .then(|| (limit * SECONDS_PER_MINUTE - (self.server_time() - self.match_start)).max(0.0))
    }

    /// Server time at the latest tick, in seconds: what the game's own times count in.
    pub(super) fn server_time(&self) -> f32 {
        self.server_tick as f32 * self.interval()
    }

    pub(super) fn round_time(&self) -> Option<f32> {
        let now = self.server_time();
        self.round_live_since.map(|t| (self.round_ended_at.unwrap_or(now) - t).max(0.0))
    }

    /// `uber_ready` when a medic's charge fills up, `uber_ended` when a released charge runs out.
    pub(super) fn medic_changes(&mut self) {
        let mut events = Vec::new();
        for p in &self.gs.state.players {
            let ent = u32::from(p.entity);
            let Some(medic) = player_id(p) else { continue };
            if p.class != Class::Medic || p.state != PlayerState::Alive {
                self.ents.medic_charge.remove(&ent);
                continue;
            }
            let gun = self.medigun_owned_by(ent);
            let now = Charge { charge: uber_charge(p), releasing: gun.is_some_and(|m| m.releasing) };
            if let Some(&before) = self.ents.medic_charge.get(&ent) {
                let charge =
                    out::MedicCharge { tick: self.server_tick, medic, medigun: gun.map(|m| medigun_kind(m.item)) };
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

    /// The entities `track_frame` drops when a frame leaves them out: projectiles, buildings,
    /// flags (player destruction's pickups) and the game modes' entities.
    pub(super) fn tracked(&self) -> impl Iterator<Item = u32> + '_ {
        let state = &self.gs.state;
        (state.projectiles.keys().chain(state.buildings.keys()).map(|id| u32::from(*id)))
            .chain(self.ents.flags.keys().copied())
            .chain(self.modes.ids())
    }

    pub(super) fn forget_entity(&mut self, idx: u32) {
        self.ents.forget(idx);
        self.modes.forget(idx);
    }

    pub(super) fn player_by_user(&self, user_id: u16) -> Option<&Player> {
        self.state().players.iter().find(|p| p.info.as_ref().is_some_and(|i| u16::from(i.user_id) == user_id))
    }

    pub(super) fn player_by_entity(&self, ent: u32) -> Option<&Player> {
        self.state().players.iter().find(|p| u32::from(p.entity) == ent)
    }

    pub(super) fn entity_of_user(&self, user_id: u16) -> Option<u32> {
        self.player_by_user(user_id).map(|p| u32::from(p.entity))
    }

    pub(super) fn medigun_owned_by(&self, player_ent: u32) -> Option<&Medigun> {
        let held = self.ents.my_weapons.get(&player_ent)?;
        held.iter().flatten().find_map(|w| self.ents.mediguns.get(w).filter(|m| m.owner == Some(player_ent)))
    }

    /// The team of a round timer: on king of the hill each team has its own.
    pub(super) fn timer_team(&self, idx: u32) -> out::Team {
        match self.koth_timers.iter().position(|k| *k == Some(idx)) {
            Some(RED_SLOT) => out::Team::Red,
            Some(_) => out::Team::Blue,
            None => self.ents.timers.get(&idx).map_or(out::Team::Unassigned, |t| team_num(t.team)),
        }
    }

    /// Runs `f` with what the detail lines look up in the extractor.
    pub(super) fn with_lookup<R>(&self, f: impl FnOnce(&detail::Lookup<'_>) -> R) -> R {
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
    pub(super) fn control_point_mut(&mut self, index: usize) -> &mut ControlPoint {
        if self.control_points.len() <= index {
            self.control_points.resize(index + 1, ControlPoint::default());
        }
        let (tick, interval) = (self.server_tick, self.interval());
        let cp = &mut self.control_points[index];
        cp.advance(tick, interval);
        cp
    }

    pub(super) fn weapon_of(&self, player_ent: u32) -> Option<out::Weapon> {
        let w = self.ents.weapons.get(self.ents.active_weapons.get(&player_ent)?)?;
        Some(out::Weapon { item: w.item, clip: (w.clip >= 0).then_some(w.clip) })
    }

    pub(super) fn medigun_of(&self, player_ent: u32) -> Option<out::Medigun> {
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
    pub(super) fn flames_of(&self, player_ent: u32) -> Option<out::Flames> {
        let f =
            self.ents.flamethrowers.values().find(|f| {
                f.owner == Some(player_ent) && matches!(f.state, FLAMETHROWER_STARTING | FLAMETHROWER_FIRING)
            })?;
        Some(out::Flames { crit: f.crit, hit: f.hit })
    }

    /// Queues an event.
    pub(super) fn push(&self, line: Line) {
        self.events.borrow_mut().push(line);
    }

    /// Records that the game paused or resumed, and queues a line when that changed.
    pub(super) fn set_pause(&mut self, p: &tf_demo_parser::demo::message::generated::SetPauseMessage) {
        if p.pause != self.paused {
            self.paused = p.pause;
            self.push(Line::Pause(out::Pause { tick: self.server_tick, paused: p.pause }));
        }
    }

    /// Queues a voice packet as a line, with its Opus frames in base64. The speaker is the player
    /// in the packet's slot, as the game shows them talking (the `SteamID` in the packet comes
    /// from the speaker's own client), or else the player with that `SteamID`. The line has that
    /// player's own `SteamID`, or the packet's when no player matches. A packet without a sample
    /// rate or of another codec is skipped.
    pub(super) fn voice(&self, v: &tf_demo_parser::demo::message::voice::VoiceDataMessage<'_>) {
        use base64::Engine;
        let mut s = v.data.clone();
        let bytes: Vec<u8> = (0..usize::from(v.length) / 8).map_while(|_| s.read_int::<u8>(8).ok()).collect();
        let Some(voice) = super::voice::steam_voice(&bytes).filter(|v| v.rate > 0) else { return };
        let claimed = format!("[U:1:{}]", voice.steamid & 0xFFFF_FFFF);
        let here = |i: &&tf_demo_parser::demo::parser::analyser::UserInfo| !self.left.contains(&u16::from(i.user_id));
        let info =
            self.player_by_entity(u32::from(v.client) + 1).and_then(|p| p.info.as_ref()).filter(here).or_else(|| {
                self.state().players.iter().filter_map(|p| p.info.as_ref()).filter(here).find(|i| i.steam_id == claimed)
            });
        let b64 = base64::engine::general_purpose::STANDARD;
        self.push(Line::Voice(out::Voice {
            tick: self.server_tick,
            player: info.map(|i| PlayerId(u16::from(i.user_id))),
            steamid: info.map_or(claimed, |i| i.steam_id.clone()),
            rate: voice.rate,
            frames: voice.frames.iter().map(|f| b64.encode(f)).collect(),
            injected: false,
            pause: false,
            team: None,
            name: None,
        }));
    }

    /// Takes the queued events.
    pub(crate) fn drain(&self) -> Vec<Line> {
        self.events.take()
    }

    /// Whether a delta came from a frame never seen since this was last asked.
    pub(crate) fn take_lost_frame(&self) -> bool {
        self.lost_frame.take()
    }

    pub(super) fn user_pos(&self, user_id: u16) -> Option<Position> {
        self.player_by_user(user_id).map(|p| xyz(p.position))
    }

    pub(super) fn user_of_entity(&self, ent: u32) -> Option<PlayerId> {
        self.player_by_entity(ent).and_then(player_id)
    }

    pub(super) fn convars(&mut self, c: &SetConVarMessage) {
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

    pub(super) fn server_info(&mut self, info: &ServerInfoMessage) {
        if info.interval_per_tick > 0.0 {
            self.tick_interval = Some(info.interval_per_tick);
        }
        self.max_players = info.max_player_count;
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

#[cfg(test)]
pub(in crate::decode) fn steam_voice_parts(data: &[u8]) -> Option<(u64, u16, Vec<Vec<u8>>)> {
    super::voice::steam_voice(data).map(|v| (v.steamid, v.rate, v.frames))
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
                    | MessageType::VoiceData
                    | MessageType::SetPause
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
            Message::VoiceData(v) => self.voice(v),
            Message::SetPause(p) => self.set_pause(p),
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
