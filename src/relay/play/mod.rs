//! A relay that plays a demo instead of passing a server on, at a moment set from outside: a
//! room's playback. The demo may still be being written (a live match recorded as it goes).
//!
//! - Frames go out at their own pace, as far as the wanted moment. Off from it by more than a
//!   second, playback jumps: it decodes on (back, from the demo's start) without sending, and
//!   every client gets a full update of where it went. Only another demo file signs them on again.
//! - A full update is written from the decoded entities (see `decode::world`), for a client that
//!   joins, loses a frame or follows a jump; the frames after it are the demo's own.
//! - Paused, no frames go out. The clients are not told, so their clock runs on and what is drawn
//!   for them (beams) shows and fades; the round timers are held instead, and going on they get a
//!   full update of where it is. The match's own pauses in the demo they are told (`svc_SetPause`).
//! - Before a full update sets the clients' clock back, a `net_Tick` ahead runs their beams out.
//! - With nobody in, nothing is decoded; after a while what was decoded is dropped.
//! - Why it cannot play (not written yet, another build's demo, a read error) is told through
//!   [`Playback::problem`], and it tries again.

mod beams;
mod cursor;
mod index;
mod light;
mod notes;
mod playback;
mod player;
mod voice;

pub use crate::decode::beams::Beam;
pub use light::Light;
pub use notes::Note;
pub use playback::{Camera, Playback, Want};
use std::time::Duration;
pub use voice::VoiceIn;

/// Off from the wanted moment by more than this, playback jumps instead of catching up.
const JUMP: Duration = Duration::from_secs(1);
/// What [`Cursor::next_tick`](cursor::Cursor::next_tick) gives for a packet that is not a frame: due at once.
const NOT_A_FRAME: i64 = i64::MIN;
/// How long the player waits for a frame that is not due, or not written yet.
const NAP: Duration = Duration::from_millis(5);
/// How long the player waits for a client while nobody is in.
const IDLE_NAP: Duration = Duration::from_millis(20);
/// With nobody in for this long, the decoded demo is dropped.
const IDLE: Duration = Duration::from_secs(60);
/// Frames kept that a keyframe's client may need: those since the frame the next one builds on.
const RECENT: usize = 128;
/// How long the player waits after an error, or a demo it cannot open, before trying it again.
const RETRY: Duration = Duration::from_millis(200);
/// How often the camera cue is repeated, for clients that looked away.
const CUE_EVERY: Duration = Duration::from_secs(3);
/// How often kept beams are shown again; each shows until then, its life rounded up to the step
/// it is sent in, so they overlap by under a tenth of a second.
const KEPT_EVERY: Duration = Duration::from_secs(2);
/// Kept beams sent a second at most, and at once: well within what a client takes besides the
/// frames (a burst of hundreds of datagrams loses some, frames among them).
const KEPT_RATE: f64 = 800.0;
const KEPT_BURST: f64 = 48.0;
/// A clock set back by more than this (seconds) is first put past the beams.
const SET_BACK: f64 = 0.5;
/// How long the clients get to draw a frame with their clock ahead.
const FLUSH: Duration = Duration::from_millis(150);

#[cfg(test)]
mod tests;
