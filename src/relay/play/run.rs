//! A relay playing a demo to its clients, as a room wants it.

use std::sync::atomic::AtomicBool;

pub use super::playback::Playback;
use super::player::Player;
use crate::output::Line;
use crate::relay::Relay;

impl Relay {
    /// Plays demos to the clients, where `playback` wants them, until `stop`.
    pub fn play(&self, playback: &Playback, stop: &AtomicBool, emit: &mut dyn FnMut(Line)) -> crate::Result<()> {
        self.announce(emit)?;
        self.serving(stop, || {
            self.hub().start_playing();
            Player::new(self).run(playback, stop);
        });
        Ok(())
    }
}
