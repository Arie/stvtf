//! The clients' camera where a room has it: at a spot of the map, or along a path through it.

use std::sync::Arc;
use std::time::Instant;

use super::cursor::Cursor;
use super::player::Player;
use super::tuning::{CUE_EVERY, INTERP_RATIO, MOVE_GAP, NOT_A_FRAME};
use super::{Camera, Path, Want};
use crate::decode::Pose;

/// The clients' camera as the room has it, and what they were told of it.
#[derive(Default)]
pub(super) struct Shot {
    /// The camera they were last told to take, and when.
    pub(super) cued: Option<(Cue, Instant)>,
    /// They watch through the relay's cameraman: a full update does not take them off it.
    manned: bool,
    /// The path the room has the camera on.
    moving: Option<Moving>,
    /// Where the room has it in the frame to come.
    pub(super) pose: Option<Pose>,
    /// How many paths it was set off on.
    cuts: u8,
}

/// The path the camera is on: how far along it is (seconds), and the frame it was that far at.
struct Moving {
    path: Arc<Path>,
    secs: f64,
    tick: i64,
}

/// What the clients were last told to watch through.
#[derive(Copy, Clone, Eq, PartialEq)]
pub(super) enum Cue {
    /// A player's eyes: their user id.
    Player(u16),
    /// The relay's cameraman: its entity.
    Man(u32),
}

impl Path {
    /// Where the camera is `secs` in: at its end after it.
    fn at(&self, secs: f64, cut: u8) -> Option<Pose> {
        let i = self.steps.partition_point(|s| f64::from(s.secs) <= secs).clamp(1, self.steps.len().max(1)) - 1;
        let (a, b) = (self.steps.get(i)?, self.steps.get(i + 1).or(self.steps.get(i))?);
        let share = if b.secs > a.secs {
            ((secs - f64::from(a.secs)) / f64::from(b.secs - a.secs)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let mix = |from: [f32; 3], to: [f32; 3]| -> [f64; 3] {
            std::array::from_fn(|k| f64::from(from[k]) + f64::from(to[k] - from[k]) * share)
        };
        let (pos, aim) = (mix(a.pos, b.pos), mix(a.aim, b.aim));
        let d = [aim[0] - pos[0], aim[1] - pos[1], aim[2] - pos[2]];
        Some(Pose {
            pos: pos.map(|v| v as f32),
            pitch: (-d[2]).atan2(d[0].hypot(d[1])).to_degrees() as f32,
            yaw: d[1].atan2(d[0]).to_degrees() as f32,
            cut,
        })
    }
}

impl Player<'_> {
    /// Has the frame to come put the clients' camera where `want` has it then. A path goes on by
    /// the frames played: no further over a pause, nor over a jump.
    pub(super) fn shoot(&mut self, want: &Want) {
        let Some(c) = &mut self.cur else { return };
        let tick = c.next_tick().filter(|t| *t != NOT_A_FRAME).unwrap_or_else(|| c.tick());
        let pose = if let Some(Camera::Along(path)) = &want.camera {
            let m = match &mut self.shot.moving {
                Some(m) if m.path.id == path.id => m,
                // Set off a while ago (a relay just started): where it is by now.
                gone => {
                    self.shot.cuts = self.shot.cuts.wrapping_add(1);
                    gone.insert(Moving { path: path.clone(), secs: path.began.elapsed().as_secs_f64(), tick })
                }
            };
            let ahead = (tick - m.tick) as f64 * f64::from(c.dec.tick_interval());
            if ahead > 0.0 && ahead <= MOVE_GAP {
                m.secs += ahead;
            }
            m.tick = tick;
            m.path.at(m.secs, self.shot.cuts)
        } else {
            self.shot.moving = None;
            None
        };
        c.dec.set_shot(pose);
        // No frame to come: as if the last had it.
        let still = want.paused || (c.ended && c.next_tick().is_none());
        let moved = std::mem::replace(&mut self.shot.pose, pose) != pose;
        if moved && still && c.started && c.dec.reshoot() {
            self.rekey();
        }
    }

    /// The camera for a demo decoded on the side, which playback goes on from.
    pub(super) fn shoot_in(&self, c: &mut Cursor) {
        c.dec.set_shot(self.shot.pose);
    }

    /// Tells the clients in the game to take `camera`, when it changed and every [`CUE_EVERY`].
    pub(super) fn cue(&mut self, camera: Option<&Camera>) {
        let Some(c) = self.cur.as_ref().filter(|c| c.started) else { return };
        let Some(camera) = camera else {
            // Off the relay's own: nothing but a cue takes the clients from it, and the demo's
            // next may be long in coming. A player's eyes, until then.
            self.shot.cued = None;
            if std::mem::take(&mut self.shot.manned)
                && let Some(msg) = c.dec.living_players().first().and_then(|(player, _)| c.dec.hltv_chase(*player))
            {
                let mut hub = self.relay.hub();
                hub.queue_to_active(&msg);
                // Those waiting for a full update hear it with that.
                if hub.awaits_keyframe() {
                    hub.with_next_keyframe([msg]);
                }
            }
            return;
        };
        let cue = match camera {
            Camera::Follow { player } => Cue::Player(*player),
            // Once a frame has the cameraman.
            Camera::Along(_) => match c.dec.cameraman() {
                Some(man) => Cue::Man(man),
                None => return,
            },
        };
        if self.shot.cued.is_some_and(|(was, at)| was == cue && at.elapsed() < CUE_EVERY) {
            return;
        }
        let msg = match cue {
            Cue::Player(player) => c.dec.hltv_chase(player),
            Cue::Man(man) => c.dec.hltv_cameraman(man),
        };
        if let Some(msg) = msg {
            tracing::debug!("relay: camera {camera:?}");
            let mut hub = self.relay.hub();
            if matches!(cue, Cue::Man(_)) {
                // Between two frames back, not from the last on: a frame lost does not show.
                hub.queue_to_active(&crate::net::messages::set_convar("sv_client_min_interp_ratio", INTERP_RATIO));
            }
            hub.queue_to_active(&msg);
        }
        self.shot.manned = matches!(cue, Cue::Man(_));
        self.shot.cued = Some((cue, Instant::now()));
    }
}
