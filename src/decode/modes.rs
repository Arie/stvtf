//! The entities of the non-standard game modes: pass time's ball and goals, robot destruction's
//! robots and cores, the robot and player destruction logics' points, and Mann vs. Machine's tank
//! and wave.

use super::extract::{as_float, as_int, opt_handle, round3, team_byte, team_num, xyz};
use crate::output::{Mvm, MvmClass, Objective, PlayerId, Points};
use std::collections::BTreeMap;
use tf_demo_parser::demo::sendprop::{SendProp, SendPropIdentifier, SendPropValue};
use tf_demo_parser::demo::vector::Vector;

const fn prop_id(table: &str, name: &str) -> SendPropIdentifier {
    SendPropIdentifier::new(table, name)
}

const PROP_TEAM_NUM: SendPropIdentifier = prop_id("DT_BaseEntity", "m_iTeamNum");
const PROP_ORIGIN: SendPropIdentifier = prop_id("DT_BaseEntity", "m_vecOrigin");
const PROP_EFFECTS: SendPropIdentifier = prop_id("DT_BaseEntity", "m_fEffects");
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
/// `m_fEffects`: not drawn (the ball between rounds).
const EF_NODRAW: i64 = 0x20;

/// A positioned entity of one team.
#[derive(Default, Clone)]
struct Placed {
    team: u8,
    pos: Vector,
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
    balls: BTreeMap<u32, Ball>,
    goals: BTreeMap<u32, Placed>,
    robots: BTreeMap<u32, Robot>,
    /// Robot destruction cores (`CBonusPack`).
    cores: BTreeMap<u32, Placed>,
    tanks: BTreeMap<u32, Tank>,
    /// Mann vs. Machine revive markers and their dead players' entities.
    revives: BTreeMap<u32, (Placed, Option<u32>)>,
    /// Mann vs. Machine money packs.
    money: BTreeMap<u32, Placed>,
    /// The robot or player destruction logic, and its entity.
    logic: Option<(u32, Logic)>,
    mvm: MvmState,
}

/// A player, for the objectives that point at one: user id and position.
pub(crate) type PlayerAt<'a> = &'a dyn Fn(u32) -> Option<(PlayerId, Vector)>;

fn placed(at: &mut Placed, p: &SendProp) {
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
            "CPasstimeBall" => {
                let ball = self.balls.entry(idx).or_default();
                for p in props {
                    placed(&mut ball.at, &p);
                    match p.identifier {
                        PROP_BALL_CARRIER => ball.carrier = opt_handle(as_int(&p.value)),
                        PROP_EFFECTS => ball.hidden = as_int(&p.value) & EF_NODRAW != 0,
                        _ => {}
                    }
                }
            }
            "CFuncPasstimeGoal" => {
                let goal = self.goals.entry(idx).or_default();
                props.for_each(|p| placed(goal, &p));
            }
            "CBonusPack" => {
                let core = self.cores.entry(idx).or_default();
                props.for_each(|p| placed(core, &p));
            }
            "CCurrencyPack" => {
                let pack = self.money.entry(idx).or_default();
                props.for_each(|p| placed(pack, &p));
            }
            "CTFReviveMarker" => {
                let (at, owner) = self.revives.entry(idx).or_default();
                for p in props {
                    placed(at, &p);
                    if p.identifier == PROP_REVIVE_OWNER {
                        *owner = opt_handle(as_int(&p.value));
                    }
                }
            }
            "CTFRobotDestruction_Robot" => {
                let robot = self.robots.entry(idx).or_default();
                for p in props {
                    placed(&mut robot.at, &p);
                    match p.identifier {
                        PROP_ROBOT_HEALTH => robot.hp = as_int(&p.value),
                        PROP_ROBOT_MAX_HEALTH => robot.max_hp = as_int(&p.value),
                        _ => {}
                    }
                }
            }
            "CTFTankBoss" => {
                let tank = self.tanks.entry(idx).or_default();
                for p in props {
                    placed(&mut tank.at, &p);
                    if p.identifier == PROP_TANK_HEALTH {
                        tank.health = as_float(&p.value);
                    }
                }
            }
            "CTFRobotDestructionLogic" | "CTFPlayerDestructionLogic" => {
                let logic = &mut self.logic.get_or_insert((idx, Logic::default())).1;
                for p in props {
                    let v = as_int(&p.value);
                    match p.identifier {
                        PROP_RED_SCORE => logic.red = v,
                        PROP_BLUE_SCORE => logic.blue = v,
                        PROP_MAX_POINTS => logic.max = v,
                        PROP_RED_LEADER => logic.leaders[0] = opt_handle(v),
                        PROP_BLUE_LEADER => logic.leaders[1] = opt_handle(v),
                        _ => {}
                    }
                }
            }
            _ => {}
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
        self.balls
            .keys()
            .chain(self.goals.keys())
            .chain(self.robots.keys())
            .chain(self.cores.keys())
            .chain(self.tanks.keys())
            .chain(self.revives.keys())
            .chain(self.money.keys())
            .copied()
    }

    pub(crate) fn forget(&mut self, idx: u32) {
        self.balls.remove(&idx);
        self.goals.remove(&idx);
        self.robots.remove(&idx);
        self.cores.remove(&idx);
        self.tanks.remove(&idx);
        self.revives.remove(&idx);
        self.money.remove(&idx);
        if self.logic.as_ref().is_some_and(|(id, _)| *id == idx) {
            self.logic = None;
        }
    }

    /// A pass time map, where goals are counted as flag captures.
    pub(crate) fn passtime(&self) -> bool {
        self.passtime
    }

    pub(crate) fn objectives(&self, player: PlayerAt<'_>) -> Vec<Objective> {
        let balls = self.balls.iter().filter_map(|(&id, b)| {
            // A carried ball is attached to its carrier, its own origin relative to them.
            let carrier = b.carrier.and_then(player);
            // The ball is not drawn while carried (the carrier's gun shows it) or between rounds.
            if b.hidden && carrier.is_none() {
                return None;
            }
            Some(Objective::Ball {
                id,
                pos: xyz(carrier.map_or(b.at.pos, |c| c.1)),
                team: team_num(b.at.team),
                carrier: carrier.map(|c| c.0),
            })
        });
        let goals = self.goals.iter().map(|(&id, g)| Objective::Goal { id, pos: xyz(g.pos), team: team_num(g.team) });
        let robots = self.robots.iter().map(|(&id, r)| Objective::Robot {
            id,
            pos: xyz(r.at.pos),
            team: team_num(r.at.team),
            hp: u16::try_from(r.hp.max(0)).unwrap_or(u16::MAX),
            max_hp: u16::try_from(r.max_hp.max(0)).unwrap_or(u16::MAX),
        });
        let cores = self.cores.iter().map(|(&id, c)| Objective::Core { id, pos: xyz(c.pos), team: team_num(c.team) });
        let revives = self.revives.iter().map(|(&id, (at, owner))| Objective::Revive {
            id,
            pos: xyz(at.pos),
            owner: owner.and_then(player).map(|p| p.0),
        });
        let money = self.money.iter().map(|(&id, m)| Objective::Money { id, pos: xyz(m.pos) });
        let tanks = self.tanks.iter().map(|(&id, t)| Objective::Tank {
            id,
            pos: xyz(t.at.pos),
            health: round3(t.health.clamp(0.0, 1.0)),
        });
        balls.chain(goals).chain(robots).chain(cores).chain(revives).chain(money).chain(tanks).collect()
    }

    pub(crate) fn points(&self, player: PlayerAt<'_>) -> Option<Points> {
        let (_, logic) = self.logic.as_ref()?;
        let leader = |team: usize| logic.leaders[team].and_then(player).map(|l| l.0);
        Some(Points {
            red: logic.red,
            blue: logic.blue,
            max: (logic.max > 0).then_some(logic.max),
            red_leader: leader(0),
            blue_leader: leader(1),
        })
    }

    /// The wave, on a Mann vs. Machine map (one with waves).
    pub(crate) fn mvm(&self) -> Option<Mvm> {
        let w = &self.mvm;
        let count = |v: i64| u16::try_from(v.max(0)).unwrap_or(u16::MAX);
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
