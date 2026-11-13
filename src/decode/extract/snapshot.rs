//! The periodic snapshot of everything on the map.

use super::Extractor;
use super::convert::{
    alive, builder_entity, building_kind_of, class, conditions, player_id, team, team_num, uber_charge, ubered, xyz,
};
use super::state::{Pose, Stats, TrainWatcher};
use crate::output::{self as out, Line, PlayerId, round1, round2, round3};
use std::collections::HashSet;
use tf_demo_parser::demo::data::game_state::{Building, Player};
use tf_demo_parser::demo::parser::analyser::{Class, Team};

/// `CObjectSentrygun` `m_iState` while shooting at a target.
const SENTRY_STATE_ATTACKING: i64 = 2;

/// TF2's team numbers (0 unassigned, 1 spectator).
const TEAM_RED: u8 = 2;
const TEAM_BLUE: u8 = 3;

impl Extractor {
    /// Emits a `world` event once the world bounds are known.
    pub(crate) fn announce(&self) {
        if !self.world_sent.get()
            && let Some(w) = &self.state().world
        {
            let world = out::World { tick: self.server_tick, min: xyz(w.boundary_min), max: xyz(w.boundary_max) };
            self.world_sent.set(true);
            self.push(Line::World(world));
        }
    }

    pub(crate) fn snapshot(&self) -> out::Snapshot {
        let player_at = |ent: u32| {
            let p = self.player_by_entity(ent)?;
            Some((player_id(p)?, p.position))
        };
        let mut objectives = self.control_point_objectives();
        objectives.extend(self.carts());
        objectives.extend(self.flag_objectives());
        objectives.extend(self.modes.objectives(&player_at));
        let team_score = |team| self.ents.teams.values().find(|t| t.team == team).copied().unwrap_or_default();
        let (red, blue) = (team_score(TEAM_RED), team_score(TEAM_BLUE));
        out::Snapshot {
            tick: self.server_tick,
            players: self.state().players.iter().filter_map(|p| self.player(p)).collect(),
            projectiles: self.projectiles(),
            buildings: self.buildings(),
            objectives,
            round: self.round_state,
            round_time: self.round_time().map(round1),
            time_left: self.time_left().map(round1),
            win_limit: self.win_limit,
            match_time: (self.match_start > 0.0).then(|| round1((self.server_time() - self.match_start).max(0.0))),
            score: out::Score { red: red.score, blue: blue.score },
            captures: (!self.ents.flags.is_empty() || self.modes.passtime())
                .then_some(out::Score { red: red.captures, blue: blue.captures }),
            points: self.modes.points(&player_at),
            mvm: self.modes.mvm().map(Box::new),
            timers: self.hud_timers(),
            stopwatch: self.stopwatch(),
        }
    }

    /// The entity of the `SourceTV` bot itself (a BOT on no playing team), if it is in the match.
    pub(crate) fn sourcetv_entity(&self) -> Option<u32> {
        let bot = self.state().players.iter().find(|p| {
            p.info.as_ref().is_some_and(|i| i.steam_id == "BOT" && !self.left.contains(&u16::from(i.user_id)))
                && !matches!(p.team, Team::Red | Team::Blue)
                && p.class == Class::Other
        })?;
        Some(u32::from(bot.entity))
    }

    /// The player slots nobody is in (no player, no bot, nobody still leaving), highest first:
    /// players come into the lowest.
    pub(crate) fn spare_slots(&self) -> Vec<u8> {
        let taken: Vec<u32> =
            self.state().players.iter().filter(|p| p.info.is_some()).map(|p| u32::from(p.entity)).collect();
        (0..self.max_players).rev().filter(|slot| !taken.contains(&(u32::from(*slot) + 1))).collect()
    }

    /// A player as the snapshot shows them; none for players who left and the `SourceTV` bot.
    fn player(&self, p: &Player) -> Option<out::Player> {
        let info = p.info.as_ref()?;
        if self.left.contains(&u16::from(info.user_id)) {
            return None;
        }
        // The SourceTV bot itself is a BOT on no playing team.
        if info.steam_id == "BOT" && !matches!(p.team, Team::Red | Team::Blue) && p.class == Class::Other {
            return None;
        }
        let ent = u32::from(p.entity);
        let alive = alive(p);
        // Dead: where they died, or nothing if that was before connecting.
        let last = if alive { Some(Pose::of(p)) } else { self.ents.last_alive.get(&ent).copied() };
        let conds = self.ents.conds.get(&ent);
        let stats = self.stats.get(&ent);
        Some(out::Player {
            id: PlayerId(u16::from(info.user_id)),
            ent,
            name: info.name.clone(),
            steamid: info.steam_id.clone(),
            team: team(p.team),
            class: class(p.class),
            alive,
            hp: p.health,
            max_hp: p.max_health,
            pos: last.map(|l| xyz(l.pos)),
            yaw: last.map(|l| round1(l.yaw)),
            pitch: last.map(|l| round1(l.pitch)),
            uber: uber_charge(p),
            ping: p.ping,
            conds: conds.map(conditions).unwrap_or_default(),
            ubered: conds.and_then(ubered),
            medigun: (p.class == Class::Medic).then(|| self.medigun_of(ent)).flatten(),
            weapon: self.weapon_of(ent),
            flames: (p.class == Class::Pyro && alive).then(|| self.flames_of(ent)).flatten(),
            respawn: stats
                .filter(|_| !alive)
                .map(|st| round1(st.respawn_at - self.server_time()))
                .filter(|left| *left > 0.0),
            score: stats.map(Stats::score),
            carrying: self
                .carried
                .get(&ent)
                .filter(|_| alive && stats.is_some_and(|st| st.carrying))
                .and_then(|b| self.building_kinds.get(b).copied()),
        })
    }

    fn projectiles(&self) -> Vec<out::Projectile> {
        let gs = self.state();
        gs.projectiles
            .values()
            .map(|pr| out::Projectile {
                id: u32::from(pr.id),
                kind: self.projectile_kind(pr),
                yaw: round1(pr.rotation.y),
                team: team(pr.team),
                pos: xyz(pr.position),
                owner: gs
                    .players
                    .iter()
                    .find(|p| pr.launcher.0 != 0 && p.weapons.contains(&pr.launcher))
                    .and_then(player_id),
                crit: pr.critical,
            })
            .collect()
    }

    /// The buildings that stand: one being carried is not sent while it is carried.
    fn buildings(&self) -> Vec<out::Building> {
        let carried: HashSet<u32> = self.carried.values().copied().collect();
        self.state()
            .buildings
            .values()
            .filter(|b| !carried.contains(&u32::from(b.entity_id())))
            .map(|b| self.building(b))
            .collect()
    }

    fn building(&self, b: &Building) -> out::Building {
        let mut building = out::Building {
            id: u32::from(b.entity_id()),
            kind: building_kind_of(b),
            team: team(b.team()),
            level: b.level(),
            hp: b.health(),
            max_hp: b.max_health(),
            pos: xyz(b.position()),
            yaw: round1(b.angle()),
            builder: self.user_of_entity(builder_entity(b.builder())),
            sapped: b.sapped(),
            entrance: None,
            mini: None,
            building: None,
            aim: None,
            target: None,
            attacking: None,
        };
        if let Building::Teleporter(t) = b {
            building.entrance = Some(t.is_entrance);
        }
        if let Building::Sentry(s) = b {
            building.mini = Some(s.is_mini);
            building.building = Some(s.building);
            if let Some(t) = self.ents.sentries.get(&u32::from(b.entity_id())) {
                building.yaw = round1(t.yaw);
                building.aim = Some(round1(t.aim()));
                building.target = Some(t.enemy.and_then(|e| self.user_of_entity(e)));
                building.attacking = Some(t.state == SENTRY_STATE_ATTACKING);
            }
        }
        building
    }

    fn control_point_objectives(&self) -> Vec<out::Objective> {
        let (tick, interval) = (self.server_tick, self.interval());
        self.control_points
            .iter()
            .take(self.num_control_points)
            .zip(0..)
            .map(|(cp, index)| out::Objective::ControlPoint {
                index,
                pos: cp.pos.map(xyz),
                owner: team_num(cp.owner),
                capping_team: team_num(cp.capping_team),
                // m_flLazyCapPerc counts down from 1.0 while a team caps; it is stale otherwise.
                cap_progress: round2(cp.progress_at(tick, interval)),
                cap_rate: round3(cp.cap_rate()),
                locked: cp.locked,
                blocked: cp.blocked,
                on_point: out::OnPoint {
                    red: cp.members[usize::from(TEAM_RED)],
                    blue: cp.members[usize::from(TEAM_BLUE)],
                },
            })
            .collect()
    }

    /// Payload carts, by their teams' train watchers.
    fn carts(&self) -> Vec<out::Objective> {
        let carts: Vec<_> =
            self.state().objectives.iter().filter_map(|(id, o)| o.as_cart().map(|c| (u32::from(*id), c))).collect();
        // A payload's watcher belongs to the pushing team; maps use watchers of no team for other
        // things on a track (pd_watergate's UFO).
        let mut watchers: Vec<&TrainWatcher> =
            self.ents.train_watchers.values().filter(|w| w.team == TEAM_RED || w.team == TEAM_BLUE).collect();
        watchers.sort_by_key(|w| w.team);
        let mut out = Vec::new();
        for (i, w) in watchers.iter().enumerate() {
            // The cart model either rides on a func_tracktrain (its moveparent) or is a free
            // physics prop with its own position; without either, pair trains by order.
            let on_train =
                w.glow.and_then(|g| self.ents.parents.get(&g)).and_then(|t| carts.iter().find(|(id, _)| id == t));
            let (id, pos) = match (on_train, w.glow.and_then(|g| self.ents.prop_origins.get(&g).map(|p| (g, *p)))) {
                (Some((id, c)), _) => (*id, c.position),
                (None, Some(free)) => free,
                (None, None) if carts.len() == watchers.len() => (carts[i].0, carts[i].1.position),
                _ => continue,
            };
            out.push(out::Objective::Cart {
                id,
                pos: xyz(pos),
                team: team_num(w.team),
                progress: round2(w.progress),
                cappers: w.cappers,
                speed: w.speed,
            });
        }
        out
    }

    /// The flags in play, at their carriers while carried.
    fn flag_objectives(&self) -> Vec<out::Objective> {
        let now = self.server_time();
        self.ents
            .flags
            .iter()
            .filter(|(_, f)| !f.disabled)
            .filter_map(|(&id, f)| {
                // A carried flag is attached to its carrier, its own origin relative to them.
                let holder =
                    f.carrier.filter(|_| f.status == out::FlagStatus::Carried).and_then(|c| self.player_by_entity(c));
                if f.hidden && holder.is_none() {
                    return None;
                }
                Some(out::Objective::Flag {
                    id,
                    pos: xyz(holder.map_or(f.at.pos, |p| p.position)),
                    team: team_num(f.at.team),
                    status: f.status,
                    carrier: holder.and_then(player_id),
                    returns_in: (f.status == out::FlagStatus::Dropped && f.reset_time > now)
                        .then(|| round1(f.reset_time - now)),
                    points: (f.points != 0).then_some(f.points),
                })
            })
            .collect()
    }

    /// The round timers the HUD shows. On king of the hill those are only the teams' own;
    /// others are left from earlier rounds.
    pub(super) fn hud_timers(&self) -> Vec<out::Timer> {
        let koth = self.koth_timers != [None; 2];
        self.ents
            .timers
            .iter()
            .filter(|(idx, t)| t.show && !t.disabled && (!koth || self.koth_timers.contains(&Some(**idx))))
            .map(|(idx, t)| out::Timer {
                team: self.timer_team(*idx),
                remaining: round1(t.remaining_at(self.server_tick, self.interval())),
                paused: t.paused,
                setup: t.setup,
            })
            .collect()
    }
}
