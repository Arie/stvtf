//! The match's own state from its entities: objectives, flags, the round timers, teams and rules.

use tf_demo_parser::demo::sendprop::{SendProp, SendPropValue};

use super::{
    PROP_BLUE_KOTH_TIMER, PROP_FLAG_CAPTURES, PROP_FLAG_DISABLED, PROP_FLAG_POINTS, PROP_FLAG_RESET_TIME,
    PROP_FLAG_STATUS, PROP_MAP_RESET_TIME, PROP_OWNER_ENTITY, PROP_RED_KOTH_TIMER, PROP_ROUND_STATE,
    PROP_STATE_TRANSITION_TIME, PROP_STOPWATCH, PROP_STOPWATCH_STATE, PROP_TEAM_ROLE, PROP_TEAM_SCORE,
    PROP_TEAM_TEAM_NUM, PROP_TIMER_DISABLED, PROP_TIMER_END_TIME, PROP_TIMER_PAUSED, PROP_TIMER_REMAINING,
    PROP_TIMER_SHOW_IN_HUD, PROP_TIMER_STATE, PROP_TIMER_STOPWATCH, PROP_TIMER_TOTAL, PROP_TIMER_WATCHING_CAPS,
    PROP_WATCHER_CAPPERS, PROP_WATCHER_GLOW, PROP_WATCHER_PROGRESS, PROP_WATCHER_SPEED,
};
use crate::decode::extract::Extractor;
use crate::decode::extract::convert::{flag_status, round_from_raw};
use crate::decode::extract::limits::{MAX_CONTROL_POINTS, MAX_TEAMS};
use crate::decode::extract::state::ControlPoint;
use crate::decode::modes;
use crate::decode::props::{
    BLUE_SLOT, EF_NODRAW, PROP_EFFECTS, PROP_TEAM_NUM, RED_SLOT, as_float, as_int, opt_handle, team_byte,
};

impl Extractor {
    /// One prop of `CTFObjectiveResource`. Its per point arrays come as elements named
    /// after the array with the index as name, so they go by name rather than identifier.
    pub(super) fn objective_prop(&mut self, p: &SendProp) {
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

    pub(super) fn on_team(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
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

    pub(super) fn on_round_timer(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
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

    pub(super) fn on_train_watcher(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
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

    pub(super) fn on_flag(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
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

    pub(super) fn on_game_rules(&mut self, props: impl Iterator<Item = SendProp>) {
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

    pub(super) fn on_objective_resource(&mut self, props: impl Iterator<Item = SendProp>) {
        // Capping-team changes reset progress, so they go before the cap percentages.
        let mut props: Vec<SendProp> = props.collect();
        props.sort_by_key(|p| p.identifier.names().is_none_or(|(t, _)| t.as_str() != "m_iCappingTeam"));
        for p in &props {
            self.objective_prop(p);
            self.modes.objective_prop(p);
        }
    }
}
