//! The clients' camera where the relay wants it, moved between frames by the game itself: a prop
//! nobody sees where the camera is, turned the way it looks, and a player nobody sees who watches
//! from it as from a map's fixed camera. Told that player is the cameraman (`hltv_cameraman`),
//! the clients see what it does.
//!
//! The director's own cues do not do for a camera that moves: `hltv_fixed` turns in whole degrees
//! and is placed per packet, and what `hltv_chase` aims at is eased by the game, and shakes. But a
//! client stays with a cameraman while there is one, its director off or not: for a camera that
//! stays, `hltv_fixed` it is.

use std::sync::Arc;

use tf_demo_parser::ParserState;
use tf_demo_parser::demo::message::packetentities::{PacketEntitiesMessage, UpdateType};
use tf_demo_parser::demo::packet::datatable::ClassId;
use tf_demo_parser::demo::sendprop::{SendProp, SendPropIdentifier, SendPropValue};
use tf_demo_parser::demo::vector::Vector;

use super::{Entity, Frame, Props, World, same};
use crate::decode::props::{EF_NODRAW, INVALID_EHANDLE, PROP_EFFECTS, PROP_ORIGIN, PROP_TEAM_NUM, prop_id, prop_index};

/// Where the clients' camera is, and the way it looks: degrees, down and counterclockwise
/// positive.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Pose {
    pub(crate) pos: [f32; 3],
    pub(crate) pitch: f32,
    pub(crate) yaw: f32,
    /// Counts the cuts, round and round: a pose of another count is not moved to from the last,
    /// it is there at once.
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
/// `m_clrRender`: white, wholly see-through. Drawn so, not left out: the game only moves
/// what it draws between frames.
const CLEAR: i64 = 0x00FF_FFFF;
const TEAM_SPECTATOR: i64 = 1;
const LIFE_DEAD: i64 = 2;
/// `m_iObserverMode`: from where its target is, looking as it does.
const OBS_FIXED: i64 = 3;

const PROP_TURN: SendPropIdentifier = prop_id("DT_BaseEntity", "m_angRotation");
/// Changed, the game takes the entity as put elsewhere: it does not move it there between frames.
const PROP_CUT: SendPropIdentifier = prop_id("DT_BaseEntity", "m_ubInterpolationFrame");
/// The values it has.
const CUTS: u8 = 4;
const PROP_MODEL: SendPropIdentifier = prop_id("DT_BaseEntity", "m_nModelIndex");
const PROP_RENDER_MODE: SendPropIdentifier = prop_id("DT_BaseEntity", "m_nRenderMode");
const PROP_COLOUR: SendPropIdentifier = prop_id("DT_BaseEntity", "m_clrRender");
const PROP_LIFE: SendPropIdentifier = prop_id("DT_BasePlayer", "m_lifeState");
const PROP_OBS_MODE: SendPropIdentifier = prop_id("DT_BasePlayer", "m_iObserverMode");
const PROP_OBS_TARGET: SendPropIdentifier = prop_id("DT_BasePlayer", "m_hObserverTarget");
const PROP_FOV: SendPropIdentifier = prop_id("DT_BasePlayer", "m_iFOV");
/// The field of view the clients see through the cameraman: `hltv_fixed`'s, which a path ends on.
const FOV: i64 = 90;
const PROP_WEAPON: SendPropIdentifier = prop_id("DT_BaseCombatCharacter", "m_hActiveWeapon");

impl World {
    /// The clients' camera from the next frame on: at `pose`, or (none) the director's again.
    /// `model` is any prop's or player's model loaded, `slots` how many players the server has
    /// room for.
    pub(crate) fn set_shot(&mut self, pose: Option<Pose>, model: Option<u16>, slots: u8) {
        self.shot.pose = pose;
        self.shot.model = model;
        self.shot.slots = u32::from(slots);
    }

    /// The same in every frame kept, as if it always was: for clients who get no frames (a room
    /// paused). Whether that changed any: every client needs a full update then.
    pub(crate) fn reshoot(&mut self, state: &ParserState) -> bool {
        let mut frames = std::mem::take(&mut self.frames);
        let mut touched = false;
        for (_, frame) in &mut frames {
            touched |= self.place(frame, state);
        }
        self.frames = frames;
        touched
    }

    /// Whether the last frame applied changed what is ours in it: the stream's own entities are
    /// for no client then.
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

    /// Puts what is ours in `frame`, which the stream's `pe` was just applied to. Whether that
    /// changed any of it from the frame it builds on.
    pub(super) fn shoot(&mut self, pe: &PacketEntitiesMessage, frame: &mut Frame, state: &ParserState) -> bool {
        let Some(rig) = self.shot.rig else { return self.place(frame, state) };
        // The stream's own entered where one of ours is: both elsewhere.
        let entered =
            |i: u32| pe.entities.iter().any(|e| e.update_type == UpdateType::Enter && u32::from(e.entity_index) == i);
        let taken = [rig.prop, rig.man].map(entered);
        if taken == [false, false] {
            return self.place(frame, state);
        }
        for (i, taken) in [rig.prop, rig.man].into_iter().zip(taken) {
            if !taken {
                frame.remove(i);
            }
        }
        self.shot.rig = None;
        self.place(frame, state);
        true
    }

    /// Ours in `frame`, as wanted: there at the pose, or gone.
    fn place(&mut self, frame: &mut Frame, state: &ParserState) -> bool {
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
            // The highest free: players come into the lowest slots, entities into the lowest
            // indexes.
            let free = |mut range: std::ops::Range<u32>| range.rfind(|i| frame.get(*i).is_none());
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
        touched |= put(
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
