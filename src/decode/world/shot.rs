//! A moving camera for the relay's clients. The relay adds two entities to the frames: an
//! invisible prop at the camera's position, and a hidden player who watches from that prop. It
//! tells the clients this player is the cameraman (`hltv_cameraman`), so they see what it sees,
//! and the game moves the view smoothly between frames.
//!
//! The director's own events cannot do this: `hltv_fixed` only turns in whole degrees, and
//! `hltv_chase` shakes. But a client cannot leave a cameraman, so the relay uses the cameraman
//! only while the camera moves, and `hltv_fixed` once it stands still.

use std::sync::Arc;

use tf_demo_parser::ParserState;
use tf_demo_parser::demo::message::packetentities::{PacketEntitiesMessage, UpdateType};
use tf_demo_parser::demo::packet::datatable::ClassId;
use tf_demo_parser::demo::sendprop::{SendProp, SendPropIdentifier, SendPropValue};
use tf_demo_parser::demo::vector::Vector;

use super::{Entity, Frame, Props, World, same};
use crate::decode::props::{EF_NODRAW, INVALID_EHANDLE, PROP_EFFECTS, PROP_ORIGIN, PROP_TEAM_NUM, prop_id, prop_index};

/// Where the camera is and the way it looks (degrees, down and counterclockwise positive).
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Pose {
    pub(crate) pos: [f32; 3],
    pub(crate) pitch: f32,
    pub(crate) yaw: f32,
    /// A counter. When it changes, the camera jumps to the new pose instead of gliding there.
    pub(crate) cut: u8,
}

/// The clients' camera as the relay has it.
#[derive(Default)]
pub(super) struct Shot {
    /// Where it is to be: nowhere of the relay's choosing, with none.
    pose: Option<Pose>,
    /// What is ours in the frames for it.
    rig: Option<Rig>,
    /// The last frame applied changed what is ours in it.
    pub(super) touched: bool,
    /// Any prop's or player's model loaded.
    model: Option<u16>,
    /// How many players the server has room for.
    slots: u32,
    /// Player entities the cameraman must not use (a caster's voice has that slot).
    taken: Vec<u32>,
}

/// The entities of ours in the stream's frames: the prop, and the player who watches from it.
#[derive(Copy, Clone)]
struct Rig {
    prop: u32,
    man: u32,
}

/// Both entities' serial: with the prop's index, the handle the player watches.
const SERIAL: u32 = 777;
/// A handle's index bits.
const HANDLE_INDEX_BITS: u32 = 11;
const PROP_CLASS: &str = "CBaseAnimating";
const MAN_CLASS: &str = "CTFPlayer";
/// `m_fEffects`: casts no shadow, takes none.
const EF_NO_SHADOWS: i64 = 0x10 | 0x40;
/// `m_nRenderMode`: drawn through its colour's alpha.
const RENDER_TRANS: i64 = 2;
/// `m_clrRender`: fully transparent. It still counts as drawn, and the game only interpolates
/// entities it draws.
const CLEAR: i64 = 0x00FF_FFFF;
const TEAM_SPECTATOR: i64 = 1;
const LIFE_DEAD: i64 = 2;
/// `m_iObserverMode`: from where its target is, looking as it does.
const OBS_FIXED: i64 = 3;

const PROP_TURN: SendPropIdentifier = prop_id("DT_BaseEntity", "m_angRotation");
/// When this changes, the game moves the entity at once instead of interpolating.
const PROP_CUT: SendPropIdentifier = prop_id("DT_BaseEntity", "m_ubInterpolationFrame");
/// The values it has.
const CUTS: u8 = 4;
const PROP_MODEL: SendPropIdentifier = prop_id("DT_BaseEntity", "m_nModelIndex");
const PROP_RENDER_MODE: SendPropIdentifier = prop_id("DT_BaseEntity", "m_nRenderMode");
const PROP_COLOUR: SendPropIdentifier = prop_id("DT_BaseEntity", "m_clrRender");
const PROP_LIFE: SendPropIdentifier = prop_id("DT_BasePlayer", "m_lifeState");
const PROP_OBS_MODE: SendPropIdentifier = prop_id("DT_BasePlayer", "m_iObserverMode");
const PROP_OBS_TARGET: SendPropIdentifier = prop_id("DT_BasePlayer", "m_hObserverTarget");
const PROP_SIM_TIME: SendPropIdentifier = prop_id("DT_BaseEntity", "m_flSimulationTime");
const PROP_FOV: SendPropIdentifier = prop_id("DT_BasePlayer", "m_iFOV");
/// The cameraman's field of view, the same as `hltv_fixed` uses.
const FOV: i64 = 90;
const PROP_WEAPON: SendPropIdentifier = prop_id("DT_BaseCombatCharacter", "m_hActiveWeapon");

impl World {
    /// The camera from the next frame on: at `pose`, or (none) the director's. `model` is any
    /// model loaded, `slots` the server's player slots.
    pub(crate) fn set_shot(&mut self, pose: Option<Pose>, model: Option<u16>, slots: u8) {
        self.shot.pose = pose;
        self.shot.model = model;
        self.shot.slots = u32::from(slots);
    }

    /// The player slots (from 0) the cameraman is not to take from now on.
    pub(crate) fn keep_from_shot(&mut self, slots: &[u8]) {
        self.shot.taken = slots.iter().map(|s| u32::from(*s) + 1).collect();
    }

    /// The same in every frame kept, for a room paused. Whether any changed: a full update then.
    pub(crate) fn reshoot(&mut self, state: &ParserState) -> bool {
        let mut frames = std::mem::take(&mut self.frames);
        let mut touched = false;
        for (tick, frame) in &mut frames {
            touched |= self.place(frame, state, *tick);
        }
        self.frames = frames;
        touched
    }

    /// Whether the last frame changed ours: the stream's entities are for no client then.
    pub(crate) fn shot_touched(&self) -> bool {
        self.shot.touched
    }

    /// The player who watches for the clients, once a frame has them.
    pub(crate) fn cameraman(&self) -> Option<u32> {
        let rig = self.shot.rig.filter(|_| self.shot.pose.is_some())?;
        self.latest()?.get(rig.man).map(|_| rig.man)
    }

    /// The prop and the player of ours, once there were.
    #[cfg(test)]
    pub(crate) fn rig(&self) -> Option<(u32, u32)> {
        self.shot.rig.map(|r| (r.prop, r.man))
    }

    /// Puts ours in `frame`, after the stream's `pe`. Whether that changed any.
    pub(super) fn shoot(
        &mut self,
        pe: &PacketEntitiesMessage,
        frame: &mut Frame,
        state: &ParserState,
        tick: u32,
    ) -> bool {
        let Some(rig) = self.shot.rig else { return self.place(frame, state, tick) };
        // The stream's own entered where one of ours is: both elsewhere.
        let entered =
            |i: u32| pe.entities.iter().any(|e| e.update_type == UpdateType::Enter && u32::from(e.entity_index) == i);
        let taken = [rig.prop, rig.man].map(entered);
        if taken == [false, false] {
            return self.place(frame, state, tick);
        }
        for (i, taken) in [rig.prop, rig.man].into_iter().zip(taken) {
            if !taken {
                frame.remove(i);
            }
        }
        self.shot.rig = None;
        self.place(frame, state, tick);
        true
    }

    /// Ours in `frame`, as wanted: there at the pose, or gone.
    fn place(&mut self, frame: &mut Frame, state: &ParserState, tick: u32) -> bool {
        let Some(pose) = self.shot.pose else {
            let Some(rig) = self.shot.rig else { return false };
            let there = [rig.prop, rig.man].map(|i| frame.get(i).is_some());
            frame.remove(rig.prop);
            frame.remove(rig.man);
            return there != [false, false];
        };
        let (Some(prop_class), Some(man_class), Some(model)) =
            (class(state, PROP_CLASS), class(state, MAN_CLASS), self.shot.model)
        else {
            return false;
        };
        let rig = if let Some(rig) = self.shot.rig {
            rig
        } else {
            // Take the highest free slots: real players and entities fill from the lowest.
            let free = |mut range: std::ops::Range<u32>| {
                range.rfind(|i| frame.get(*i).is_none() && !self.shot.taken.contains(i))
            };
            let slots = self.shot.slots;
            let (Some(man), Some(prop)) = (free(1..slots + 1), free(slots + 1..u32::from(self.max_entries))) else {
                return false;
            };
            *self.shot.rig.insert(Rig { prop, man })
        };
        let mut touched = false;
        for (i, class) in [(rig.prop, prop_class), (rig.man, man_class)] {
            if frame.get(i).is_none() {
                // As a client has one entering: its class's baseline.
                let mut props = Props::default();
                let base =
                    state.send_tables.get(usize::from(class)).and_then(|t| state.get_static_baseline(class, t).ok());
                for p in base.unwrap_or_default() {
                    props.set(&p);
                }
                frame.insert(i, Arc::new(Entity { class, serial: SERIAL, props }));
                touched = true;
            }
        }
        let int = SendPropValue::Integer;
        let vector = |x, y, z| SendPropValue::Vector(Vector { x, y, z });
        let moved = put(
            frame,
            state,
            rig.prop,
            &[
                (PROP_MODEL, int(i64::from(model))),
                (PROP_EFFECTS, int(EF_NO_SHADOWS)),
                (PROP_RENDER_MODE, int(RENDER_TRANS)),
                (PROP_COLOUR, int(CLEAR)),
                (PROP_ORIGIN, vector(pose.pos[0], pose.pos[1], pose.pos[2])),
                (PROP_TURN, vector(pose.pitch.rem_euclid(360.0), pose.yaw.rem_euclid(360.0), 0.0)),
                (PROP_CUT, int(i64::from(pose.cut % CUTS))),
            ],
        );
        // The time it moved, as a real server sends it. The game interpolates between these.
        if moved {
            put(frame, state, rig.prop, &[(PROP_SIM_TIME, int(sim_time(tick, rig.prop)))]);
        }
        touched |= moved;
        touched |= put(
            frame,
            state,
            rig.man,
            &[
                (PROP_EFFECTS, int(EF_NODRAW)),
                (PROP_TEAM_NUM, int(TEAM_SPECTATOR)),
                (PROP_LIFE, int(LIFE_DEAD)),
                (PROP_OBS_MODE, int(OBS_FIXED)),
                (PROP_OBS_TARGET, int(i64::from(rig.prop | SERIAL << HANDLE_INDEX_BITS))),
                (PROP_WEAPON, int(INVALID_EHANDLE)),
                (PROP_FOV, int(FOV)),
            ],
        );
        touched
    }
}

/// `m_flSimulationTime` as a server sends it: ticks since the entity's last hundredth one.
fn sim_time(tick: u32, entity: u32) -> i64 {
    let base = 100 * (tick.saturating_sub(entity % 32) / 100);
    i64::from((tick - base) & 0xFF)
}

/// The class named `name`.
fn class(state: &ParserState, name: &str) -> Option<ClassId> {
    let at = state.server_classes.iter().position(|c| c.name.as_str() == name)?;
    Some(ClassId::from(u16::try_from(at).ok()?))
}

/// Gives the entity at `index` each of `props` its class has; whether any was not so already.
fn put(frame: &mut Frame, state: &ParserState, index: u32, props: &[(SendPropIdentifier, SendPropValue)]) -> bool {
    let Some(e) = frame.get_mut(index) else { return false };
    let Some(table) = state.send_tables.get(usize::from(e.class)) else { return false };
    let mut touched = false;
    for (id, value) in props {
        let Some(at) = prop_index(table, *id) else { continue };
        if e.props.get(at).is_some_and(|p| same(&p.value, value)) {
            continue;
        }
        Arc::make_mut(e).props.set(&SendProp { index: at, identifier: *id, value: value.clone() });
        touched = true;
    }
    touched
}
