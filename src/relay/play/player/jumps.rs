//! Jumps: back or far ahead in the demo, decoding without sending.

use std::sync::Mutex;
use std::time::Instant;

use super::Player;
use crate::Result;
use crate::relay::hub::Hub;
use crate::relay::play::Want;
use crate::relay::play::cursor::{Cursor, owned};
use crate::relay::play::tuning::JUMP;
use crate::sync::lock;

impl Player<'_> {
    /// Back to `target`: the demo again from the last checkpoint before it (or its start), decoded
    /// up to there, and a full update of it: the clients stay in, and go back. If an entry of the
    /// string tables they have changed value since (a player left or joined), those entries are set
    /// back first.
    pub(super) fn jump_back(&mut self, want: &Want, target: i64) -> Result<()> {
        let Some(c) = &self.cur else { return Ok(()) };
        tracing::info!(
            "relay: :{} jump back from {} to {target}, {} clients",
            self.relay.port(),
            c.tick(),
            self.relay.clients()
        );
        // A spectator's panel: not for one watching as the player.
        if !c.dec.players_own() {
            self.relay.hub().reshow_panel_with_next_keyframe();
        }
        // The clients keep the models loaded since.
        self.models = self.models.with(&c.dec);
        let rewritten = c.dec.tables_rewritten();
        let took = Instant::now();
        // From the last checkpoint before it, or else the start.
        let mark = self.index.as_ref().and_then(|i| i.before(target));
        let mut again = match &mark {
            Some(m) => Cursor::restore(&want.demo, m)?,
            None => Cursor::open_detached(&want.demo)?,
        };
        self.shoot_in(&mut again);
        // The clients had the string table changes up to there.
        self.dry = skim(&mut again, None, target).1;
        tracing::debug!("relay: decoded back to {target} in {} ms", took.elapsed().as_millis());
        if again.dec.tables_rewritten() < rewritten {
            // The clients' tables are set back to these, or failing that, they sign on again.
            // The stream's changes after here are sent again when playback comes to them.
            let later = owned(c.tables.get(again.tables.len()..).unwrap_or_default());
            if let Some(msgs) = again.dec.set_tables_back(&c.dec) {
                tracing::debug!("relay: string tables set back with {} messages", msgs.len());
                self.relay.hub().set_tables_back(&later, &msgs);
            } else {
                tracing::info!(
                    "relay: :{} the string tables changed since: the clients sign on again",
                    self.relay.port()
                );
                self.cur = None;
                self.dry = false;
                return Ok(());
            }
        }
        self.cur = Some(again);
        self.rekey();
        Ok(())
    }

    /// Decodes on to `target` without sending when playback is more than `jump` behind it and the
    /// demo has it written, from a checkpoint if one is well past: every client gets a keyframe.
    pub(super) fn jump_ahead(&mut self, want: &Want, target: i64, jump: i64) -> Result<()> {
        let Some(c) = &mut self.cur else { return Ok(()) };
        if !(c.started && c.tick() + jump < target && !self.dry && c.next_tick().is_some_and(|n| n <= target)) {
            return Ok(());
        }
        let (from, took) = (c.tick(), Instant::now());
        // From a checkpoint well past where it is, if there is one: the clients get the string
        // table changes passed over with it, those after the ones they had.
        let mark = self.index.as_ref().and_then(|i| i.before(target)).filter(|m| i64::from(m.tick) > from + jump);
        if let Some(m) = &mark {
            let passed = m.tables();
            self.relay.hub().add_table_changes(passed.get(c.tables.len()..).unwrap_or_default());
            *c = Cursor::restore(&want.demo, m)?;
            c.dec.set_shot(self.shot.pose);
        }
        let (decoded, dry) = skim(c, Some(&self.relay.hub), target);
        let decoded = decoded + usize::from(mark.is_some());
        tracing::info!(
            "relay: :{} jump ahead from {from} to {} in {} ms, {} clients",
            self.relay.port(),
            c.tick(),
            took.elapsed().as_millis(),
            self.relay.clients()
        );
        self.dry = dry;
        if decoded > 0 {
            self.rekey();
        }
        Ok(())
    }
}

/// The ticks in [`JUMP`]: off by more, playback jumps.
pub(super) fn jump_ticks(interval: f32) -> i64 {
    (JUMP.as_secs_f32() / interval.max(1e-3)) as i64
}

/// Decodes on to `target` without sending: only string table changes reach the clients, through
/// `hub` if given. How many frames it decoded, and whether it stopped short for want of more
/// written.
pub(super) fn skim(c: &mut Cursor, hub: Option<&Mutex<Hub>>, target: i64) -> (usize, bool) {
    let mut decoded = 0;
    loop {
        match c.next_tick() {
            None => return (decoded, !c.ended),
            Some(next) if next > target => return (decoded, false),
            Some(_) => match c.decode() {
                Some((bits, life)) => {
                    if let Some(hub) = hub {
                        lock(hub).skim(&bits, &life);
                    }
                    decoded += 1;
                }
                None => c.skip_next(),
            },
        }
    }
}
