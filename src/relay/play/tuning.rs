//! How a demo plays: how long it waits, naps and keeps.

use std::time::Duration;

/// Off from the wanted moment by more than this, playback jumps instead of catching up.
pub(super) const JUMP: Duration = Duration::from_secs(1);

/// What [`Cursor::next_tick`](super::cursor::Cursor::next_tick) gives for a packet that is not a
/// frame: due at once.
pub(super) const NOT_A_FRAME: i64 = i64::MIN;

/// How long the player waits for a frame that is not due, or not written yet.
pub(super) const NAP: Duration = Duration::from_millis(5);

/// How long a full update waits for the frame after it, at the end of what is written: a recording
/// that stopped, or a match paused, has none coming.
pub(super) const FRAME_WAIT: Duration = Duration::from_millis(if cfg!(test) { 30 } else { 1000 });

/// The demos played before the one playing whose checkpoints are kept: a match's maps and parts.
pub(super) const PAST_INDEXES: usize = 4;

/// How long the player waits for a client while nobody is in.
pub(super) const IDLE_NAP: Duration = Duration::from_millis(20);

/// With nobody in for this long, the decoded demo is dropped.
pub(super) const IDLE: Duration = Duration::from_secs(60);

/// The last frames kept, for a client that starts from a keyframe: it may need those since the
/// frame the next one builds on.
pub(super) const RECENT: usize = 128;

/// How long the player waits after an error, or a demo it cannot open, before trying it again.
pub(super) const RETRY: Duration = Duration::from_millis(200);

/// How long clients get to finish what they are being sent before they are handed over.
pub(super) const HAND_OVER_WAIT: Duration = Duration::from_secs(1);

/// How often the camera cue is repeated, for clients that looked away.
pub(super) const CUE_EVERY: Duration = Duration::from_secs(3);

/// The most seconds between two frames that a path's camera goes on by: more is a jump.
pub(super) const MOVE_GAP: f64 = 1.0;

/// How long the cameraman stays after the clients were told the spot its path ended at.
pub(super) const SETTLE: Duration = Duration::from_millis(500);

/// The least `cl_interp_ratio` of clients watching through the relay's cameraman.
pub(super) const INTERP_RATIO: &str = "2";

/// How often kept beams are shown again; each shows until then, its life rounded up to the step
/// it is sent in, so they overlap by under a tenth of a second.
pub(super) const KEPT_EVERY: Duration = Duration::from_secs(2);

/// The most kept beams sent a second, and (`KEPT_BURST`) at once: well within what a client takes
/// besides the frames (a burst of hundreds of datagrams loses some, frames among them).
pub(super) const KEPT_RATE: f64 = 800.0;

pub(super) const KEPT_BURST: f64 = 48.0;

/// A clock set back by more than this (seconds) is first put past the beams.
pub(super) const SET_BACK: f64 = 0.5;

/// How long the clients get to draw a frame with their clock ahead.
pub(super) const FLUSH: Duration = Duration::from_millis(150);
