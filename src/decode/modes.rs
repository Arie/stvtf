//! The entities of the non-standard game modes: pass time's ball and goals, robot destruction's
//! robots and cores, the robot and player destruction logics' points, and Mann vs. Machine's tank
//! and wave.

use super::extract::{team_num, xyz};
use super::props::{
    BLUE_SLOT, EF_NODRAW, PROP_EFFECTS, PROP_ORIGIN, PROP_TEAM_NUM, RED_SLOT, as_float, as_int, opt_handle, prop_id,
    saturating_u16, team_byte,
};
use crate::output::{Mvm, MvmClass, Objective, PlayerId, Points, round3};
use std::collections::BTreeMap;
use tf_demo_parser::demo::sendprop::{SendProp, SendPropIdentifier, SendPropValue};
use tf_demo_parser::demo::vector::Vector;

const PROP_BALL_CARRIER: SendPropIdentifier = prop_id("DT_PasstimeBall", "m_hCarrier");
const PROP_ROBOT_HEALTH: SendPropIdentifier = prop_id("DT_TFRobotDestruction_Robot", "m_iHealth");
const PROP_ROBOT_MAX_HEALTH: SendPropIdentifier = prop_id("DT_TFRobotDestruction_Robot", "m_iMaxHealth");
const PROP_TANK_HEALTH: SendPropIdentifier = prop_id("DT_TFBaseBoss", "m_lastHealthPercentage");
const PROP_RED_SCORE: SendPropIdentifier = prop_id("DT_TFRobotDestructionLogic", "m_nRedScore");
const PROP_BLUE_SCORE: SendPropIdentifier = prop_id("DT_TFRobotDestructionLogic", "m_nBlueScore");
const PROP_MAX_POINTS: SendPropIdentifier = prop_id("DT_TFRobotDestructionLogic", "m_nMaxPoints");
const PROP_RED_LEADER: SendPropIdentifier = prop_id("DT_TFPlayerDestructionLogic", "m_hRedTeamLeader");
const PROP_BLUE_LEADER: SendPropIdentifier = prop_id("DT_TFPlayerDestructionLogic", "m_hBlueTeamLeader");
const PROP_MVM_WAVE: SendPropIdentifier = prop_id("DT_TFObjectiveResource", "m_nMannVsMachineWaveCount");
const PROP_MVM_WAVES: SendPropIdentifier = prop_id("DT_TFObjectiveResource", "m_nMannVsMachineMaxWaveCount");
const PROP_MVM_ENEMIES: SendPropIdentifier = prop_id("DT_TFObjectiveResource", "m_nMannVsMachineWaveEnemyCount");
const PROP_MVM_BETWEEN_WAVES: SendPropIdentifier = prop_id("DT_TFObjectiveResource", "m_bMannVsMachineBetweenWaves");
const PROP_MVM_MONEY: SendPropIdentifier = prop_id("DT_TFObjectiveResource", "m_nMvMWorldMoney");
const PROP_MVM_CLASS_NAMES: SendPropIdentifier = prop_id("DT_TFObjectiveResource", "m_iszMannVsMachineWaveClassNames");
const PROP_MVM_CLASS_NAMES_2: SendPropIdentifier =
    prop_id("DT_TFObjectiveResource", "m_iszMannVsMachineWaveClassNames2");
const PROP_REVIVE_OWNER: SendPropIdentifier = prop_id("DT_TFReviveMarker", "m_hOwner");

/// A positioned entity of one team.
#[derive(Default, Clone)]
pub(crate) struct Placed {
    pub(crate) team: u8,
    pub(crate) pos: Vector,
}

#[derive(Default, Clone)]
struct Ball {
    at: Placed,
    carrier: Option<u32>,
    hidden: bool,
}

#[derive(Default, Clone)]
struct Robot {
    at: Placed,
    hp: i64,
    max_hp: i64,
}

#[derive(Default, Clone)]
struct Tank {
    at: Placed,
    health: f32,
}

#[derive(Default, Clone)]
struct Revive {
    at: Placed,
    /// The dead player's entity.
    owner: Option<u32>,
}

/// An entity followed here. In the order the snapshot lists them.
#[derive(Clone)]
enum Followed {
    Ball(Ball),
    /// A pass time goal.
    Goal(Placed),
    Robot(Robot),
    /// A robot destruction core (`CBonusPack`).
    Core(Placed),
    /// A Mann vs. Machine revive marker.
    Revive(Revive),
    /// A Mann vs. Machine money pack.
    Money(Placed),
    Tank(Tank),
}

impl Followed {
    /// What an entity of this server class is, before its props.
    fn of_class(class: &str) -> Option<Self> {
        Some(match class {
            "CPasstimeBall" => Followed::Ball(Ball::default()),
            "CFuncPasstimeGoal" => Followed::Goal(Placed::default()),
            "CTFRobotDestruction_Robot" => Followed::Robot(Robot::default()),
            "CBonusPack" => Followed::Core(Placed::default()),
            "CTFReviveMarker" => Followed::Revive(Revive::default()),
            "CCurrencyPack" => Followed::Money(Placed::default()),
            "CTFTankBoss" => Followed::Tank(Tank::default()),
            _ => return None,
        })
    }

    /// Its place in the snapshot's list.
    fn rank(&self) -> u8 {
        match self {
            Followed::Ball(_) => 0,
            Followed::Goal(_) => 1,
            Followed::Robot(_) => 2,
            Followed::Core(_) => 3,
            Followed::Revive(_) => 4,
            Followed::Money(_) => 5,
            Followed::Tank(_) => 6,
        }
    }

    fn at_mut(&mut self) -> &mut Placed {
        match self {
            Followed::Ball(Ball { at, .. })
            | Followed::Robot(Robot { at, .. })
            | Followed::Revive(Revive { at, .. })
            | Followed::Tank(Tank { at, .. })
            | Followed::Goal(at)
            | Followed::Core(at)
            | Followed::Money(at) => at,
        }
    }

    fn update(&mut self, p: &SendProp) {
        placed(self.at_mut(), p);
        let v = as_int(&p.value);
        match (self, p.identifier) {
            (Followed::Ball(ball), PROP_BALL_CARRIER) => ball.carrier = opt_handle(v),
            (Followed::Ball(ball), PROP_EFFECTS) => ball.hidden = v & EF_NODRAW != 0,
            (Followed::Revive(revive), PROP_REVIVE_OWNER) => revive.owner = opt_handle(v),
            (Followed::Robot(robot), PROP_ROBOT_HEALTH) => robot.hp = v,
            (Followed::Robot(robot), PROP_ROBOT_MAX_HEALTH) => robot.max_hp = v,
            (Followed::Tank(tank), PROP_TANK_HEALTH) => tank.health = as_float(&p.value),
            _ => {}
        }
    }

    fn objective(&self, id: u32, player: PlayerAt<'_>) -> Option<Objective> {
        Some(match self {
            Followed::Ball(b) => {
                // A carried ball is attached to its carrier, its own origin relative to them.
                let carrier = b.carrier.and_then(player);
                // The ball is not drawn while carried (the carrier's gun shows it) or between rounds.
                if b.hidden && carrier.is_none() {
                    return None;
                }
                Objective::Ball {
                    id,
                    pos: xyz(carrier.map_or(b.at.pos, |c| c.1)),
                    team: team_num(b.at.team),
                    carrier: carrier.map(|c| c.0),
                }
            }
            Followed::Goal(g) => Objective::Goal { id, pos: xyz(g.pos), team: team_num(g.team) },
            Followed::Robot(r) => Objective::Robot {
                id,
                pos: xyz(r.at.pos),
                team: team_num(r.at.team),
                hp: saturating_u16(r.hp),
                max_hp: saturating_u16(r.max_hp),
            },
            Followed::Core(c) => Objective::Core { id, pos: xyz(c.pos), team: team_num(c.team) },
            Followed::Revive(r) => {
                Objective::Revive { id, pos: xyz(r.at.pos), owner: r.owner.and_then(player).map(|p| p.0) }
            }
            Followed::Money(m) => Objective::Money { id, pos: xyz(m.pos) },
            Followed::Tank(t) => Objective::Tank { id, pos: xyz(t.at.pos), health: round3(t.health.clamp(0.0, 1.0)) },
        })
    }
}

/// The robot or player destruction logic.
#[derive(Default, Clone)]
struct Logic {
    red: i64,
    blue: i64,
    max: i64,
    leaders: [Option<u32>; 2],
}

#[derive(Default, Clone)]
struct MvmState {
    wave: i64,
    waves: i64,
    enemies: i64,
    between_waves: bool,
    money: i64,
    /// The wave's robot kinds, in the HUD's order: two arrays of 12 (the second's props end in 2).
    classes: [WaveClass; 2 * WAVE_CLASSES],
}

#[derive(Default, Clone)]
struct WaveClass {
    name: String,
    count: i64,
    flags: i64,
    active: bool,
}

/// Robot kinds per array of `CTFObjectiveResource`'s wave props.
const WAVE_CLASSES: usize = 12;
/// `m_nMannVsMachineWaveClassFlags` bits (`MVM_CLASS_FLAG_*`).
const MVM_SUPPORT: i64 = 1 << 1;
const MVM_MISSION: i64 = 1 << 2;
const MVM_MINIBOSS: i64 = 1 << 3;
const MVM_ALWAYS_CRIT: i64 = 1 << 4;
const MVM_SUPPORT_LIMITED: i64 = 1 << 5;

#[derive(Default)]
pub(crate) struct Modes {
    /// A pass time map: its logic entity exists.
    passtime: bool,
    entities: BTreeMap<u32, Followed>,
    /// The robot or player destruction logic, and its entity.
    logic: Option<(u32, Logic)>,
    mvm: MvmState,
}

/// A player, for the objectives that point at one: user id and position.
pub(crate) type PlayerAt<'a> = &'a dyn Fn(u32) -> Option<(PlayerId, Vector)>;

/// The team and origin props of a positioned entity.
pub(crate) fn placed(at: &mut Placed, p: &SendProp) {
    match p.identifier {
        PROP_TEAM_NUM => at.team = team_byte(as_int(&p.value)),
        PROP_ORIGIN => {
            if let SendPropValue::Vector(v) = p.value {
                at.pos = v;
            }
        }
        _ => {}
    }
}

impl Modes {
    /// One update of an entity of `class`; other classes are not followed here.
    pub(crate) fn entity(&mut self, class: &str, idx: u32, props: impl Iterator<Item = SendProp>) {
        match class {
            "CTFPasstimeLogic" => self.passtime = true,
            "CTFRobotDestructionLogic" | "CTFPlayerDestructionLogic" => self.logic(idx, props),
            _ => {
                let Some(fresh) = Followed::of_class(class) else { return };
                let followed = self.entities.entry(idx).or_insert_with(|| fresh.clone());
                // A slot taken over by another kind of entity starts again.
                if std::mem::discriminant(followed) != std::mem::discriminant(&fresh) {
                    *followed = fresh;
                }
                props.for_each(|p| followed.update(&p));
            }
        }
    }

    fn logic(&mut self, idx: u32, props: impl Iterator<Item = SendProp>) {
        let logic = &mut self.logic.get_or_insert((idx, Logic::default())).1;
        for p in props {
            let v = as_int(&p.value);
            match p.identifier {
                PROP_RED_SCORE => logic.red = v,
                PROP_BLUE_SCORE => logic.blue = v,
                PROP_MAX_POINTS => logic.max = v,
                PROP_RED_LEADER => logic.leaders[RED_SLOT] = opt_handle(v),
                PROP_BLUE_LEADER => logic.leaders[BLUE_SLOT] = opt_handle(v),
                _ => {}
            }
        }
    }

    /// One prop of `CTFObjectiveResource`: Mann vs. Machine's wave.
    pub(crate) fn objective_prop(&mut self, p: &SendProp) {
        let v = as_int(&p.value);
        match p.identifier {
            PROP_MVM_WAVE => self.mvm.wave = v,
            PROP_MVM_WAVES => self.mvm.waves = v,
            PROP_MVM_ENEMIES => self.mvm.enemies = v,
            PROP_MVM_BETWEEN_WAVES => self.mvm.between_waves = v != 0,
            PROP_MVM_MONEY => self.mvm.money = v,
            PROP_MVM_CLASS_NAMES | PROP_MVM_CLASS_NAMES_2 => {
                let offset = if p.identifier == PROP_MVM_CLASS_NAMES { 0 } else { WAVE_CLASSES };
                if let SendPropValue::Array(names) = &p.value {
                    for (class, name) in self.mvm.classes[offset..].iter_mut().zip(names.iter().take(WAVE_CLASSES)) {
                        if let SendPropValue::String(name) = name {
                            class.name.clone_from(name);
                        }
                    }
                }
            }
            // The per-kind arrays come as elements named after the array, with the index as name.
            _ => {
                let Some((table, index)) = p.identifier.names() else { return };
                let Ok(index) = index.as_str().parse::<usize>() else { return };
                let (array, second) = match table.as_str().strip_suffix('2') {
                    Some(first) => (first, true),
                    None => (table.as_str(), false),
                };
                let Some(class) = self.mvm.classes.get_mut(index + if second { WAVE_CLASSES } else { 0 }) else {
                    return;
                };
                match array {
                    "m_nMannVsMachineWaveClassCounts" => class.count = v,
                    "m_nMannVsMachineWaveClassFlags" => class.flags = v,
                    "m_bMannVsMachineWaveClassActive" => class.active = v != 0,
                    _ => {}
                }
            }
        }
    }

    /// The entities followed here.
    pub(crate) fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.entities.keys().copied()
    }

    pub(crate) fn forget(&mut self, idx: u32) {
        self.entities.remove(&idx);
        if self.logic.as_ref().is_some_and(|(id, _)| *id == idx) {
            self.logic = None;
        }
    }

    /// A pass time map, where goals are counted as flag captures.
    pub(crate) fn passtime(&self) -> bool {
        self.passtime
    }

    pub(crate) fn objectives(&self, player: PlayerAt<'_>) -> Vec<Objective> {
        let mut objectives: Vec<(u8, Objective)> =
            self.entities.iter().filter_map(|(&id, e)| Some((e.rank(), e.objective(id, player)?))).collect();
        // Stable: within a kind, by entity.
        objectives.sort_by_key(|(rank, _)| *rank);
        objectives.into_iter().map(|(_, o)| o).collect()
    }

    pub(crate) fn points(&self, player: PlayerAt<'_>) -> Option<Points> {
        let (_, logic) = self.logic.as_ref()?;
        let leader = |team: usize| logic.leaders[team].and_then(player).map(|l| l.0);
        Some(Points {
            red: logic.red,
            blue: logic.blue,
            max: (logic.max > 0).then_some(logic.max),
            red_leader: leader(RED_SLOT),
            blue_leader: leader(BLUE_SLOT),
        })
    }

    /// The wave, on a Mann vs. Machine map (one with waves).
    pub(crate) fn mvm(&self) -> Option<Mvm> {
        let w = &self.mvm;
        let count = saturating_u16;
        (w.waves > 0).then(|| Mvm {
            wave: count(w.wave),
            waves: count(w.waves),
            enemies: count(w.enemies),
            between_waves: w.between_waves,
            money: w.money,
            classes: w
                .classes
                .iter()
                .filter(|c| !c.name.is_empty())
                .map(|c| MvmClass {
                    name: c.name.clone(),
                    count: count(c.count),
                    active: c.active,
                    giant: c.flags & MVM_MINIBOSS != 0,
                    crit: c.flags & MVM_ALWAYS_CRIT != 0,
                    support: c.flags & (MVM_SUPPORT | MVM_SUPPORT_LIMITED) != 0,
                    mission: c.flags & MVM_MISSION != 0,
                })
                .collect(),
        })
    }
}
