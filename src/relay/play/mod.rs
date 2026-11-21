//! A relay that plays a demo instead of passing a server on, at a moment set from outside: a
//! room's playback. The demo may still be being written (a live match recorded as it goes).
//!
//! - Frames go out at their own pace, as far as the wanted moment. Off from it by more than a
//!   second, playback jumps: it decodes on (back, from the last checkpoint before) without sending,
//!   and every client gets a full update of where it went. Only another demo file signs them on
//!   again, and not one of the same map load.
//! - A full update is written from the decoded entities (see `decode::world`), for a client that
//!   joins or follows a jump. The frames after it are deltas from the frame that client acked: the
//!   demo's own where it builds on that one, else written anew.
//! - Paused, no frames go out. The clients are not told, so their clock runs on and what is drawn
//!   for them (beams) shows and fades; the round timers are held instead, and going on they get a
//!   full update of where it is. The match's own pauses in the demo they are told (`svc_SetPause`).
//! - Before a full update sets the clients' clock back, a `net_Tick` ahead runs their beams out.
//! - With nobody in, nothing is decoded; after a while what was decoded is dropped.
//! - Why it cannot play (not written yet, another build's demo, a read error) is told through
//!   [`Playback::problem`], and it tries again.

mod beams;
mod cursor;
mod follow;
mod index;
mod light;
mod names;
mod notes;
mod playback;
mod player;
mod run;
mod shot;
mod tuning;
mod voice;

pub use self::light::Light;
pub use self::names::Names;
pub use self::notes::Note;
pub use self::playback::{Camera, Path, Playback, Step, Want};
pub use self::voice::VoiceIn;
pub use crate::decode::beams::Beam;

#[cfg(test)]
mod shot_tests;
#[cfg(test)]
mod tests;
