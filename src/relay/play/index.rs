//! Checkpoints through a demo, so a jump decodes at most [`EVERY`] ticks of it instead of all of
//! it from the start (or from where playback is).
//!
//! A checkpoint is what a decoder needs to go on from a frame: the string table changes up to it,
//! a full update of the frame the next one builds on, the frames played since that one, and where
//! the next packet starts in the file. A decoder fed the demo's signon and those has every entity
//! as one that decoded from the start (see `decode::world`), and reads on from there.
//!
//! The checkpoints are made on the side, by a thread decoding the whole demo as soon as playback
//! opens it, and following it as it grows (a live match).

use super::{Cursor, RECENT};
use crate::net::bits::BitBuf;
use crate::relay::Hub;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// Server ticks between checkpoints: 30 s on a 66-tick server.
const EVERY: u32 = 2000;
/// How long the indexing thread waits at the end of what is written so far.
const WAIT: Duration = Duration::from_millis(250);

pub(super) struct Checkpoint {
    /// The server tick of the last frame decoded.
    pub(super) tick: u32,
    /// Where the next packet starts in the file.
    pos: u64,
    tables: Vec<Arc<BitBuf>>,
    key: BitBuf,
    /// The frames played since the one `key` is of, by server tick: as the decoder's own or, where
    /// they build on an older one, from `key`'s.
    since: Vec<(u32, BitBuf)>,
    first: Option<i64>,
    offset: i64,
    paused: bool,
}

impl Checkpoint {
    /// The string table changes up to it, as a client given them would have them.
    pub(super) fn tables(&self) -> Vec<BitBuf> {
        self.tables.iter().map(|t| (**t).clone()).collect()
    }
}

/// A demo's checkpoints, by tick, made by [`Index::start`] on another thread.
pub(super) struct Index {
    pub(super) path: PathBuf,
    marks: Mutex<Vec<Arc<Checkpoint>>>,
    stop: AtomicBool,
}

impl Index {
    /// Starts indexing the demo at `path`; it stops when the index is dropped.
    pub(super) fn start(path: &Path) -> Arc<Index> {
        let index =
            Arc::new(Index { path: path.to_owned(), marks: Mutex::new(Vec::new()), stop: AtomicBool::new(false) });
        let weak = Arc::downgrade(&index);
        let path = path.to_owned();
        std::thread::spawn(move || {
            if let Err(e) = index_demo(&path, &weak, EVERY) {
                trace!("indexing {}: {e:#}", path.display());
            }
        });
        index
    }

    /// The last checkpoint at or before server tick `tick`.
    pub(super) fn before(&self, tick: i64) -> Option<Arc<Checkpoint>> {
        let marks = self.marks.lock().unwrap_or_else(PoisonError::into_inner);
        marks.iter().rev().find(|c| i64::from(c.tick) <= tick).cloned()
    }

    fn add(&self, c: Checkpoint) {
        self.marks.lock().unwrap_or_else(PoisonError::into_inner).push(Arc::new(c));
    }
}

impl Drop for Index {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Decodes the demo, a checkpoint every `every` ticks, until it ends or the index is dropped.
fn index_demo(path: &Path, index: &std::sync::Weak<Index>, every: u32) -> Result<()> {
    let mut c = Cursor::open_with(path, None)?;
    let mut last: Option<u32> = None;
    loop {
        let Some(ix) = index.upgrade() else { return Ok(()) };
        if ix.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        match c.next_tick() {
            None if c.ended => return Ok(()),
            None => std::thread::sleep(WAIT),
            Some(i64::MIN) => c.next = None,
            Some(_) => {
                c.decode();
                let tick = c.dec.server_tick();
                if last.is_none_or(|l| tick >= l.saturating_add(every) || tick < l)
                    && let Some(mark) = c.checkpoint()
                {
                    last = Some(tick);
                    ix.add(mark);
                }
            }
        }
    }
}

impl Cursor {
    /// A checkpoint at the last frame decoded, once the next one is there to say which frame it
    /// builds on.
    fn checkpoint(&mut self) -> Option<Checkpoint> {
        let next = match self.peek()? {
            super::Packet::Message { data, .. } => data.clone(),
            _ => return None,
        };
        let (key_tick, key) = self.dec.keyframe(Some(&next))?;
        let recent: Vec<(u32, BitBuf)> = self.recent.iter().cloned().collect();
        let ticks = recent.iter().map(|(t, _)| *t).filter(|t| *t > key_tick);
        let since = ticks.zip(self.dec.catch_up(key_tick, &recent)).collect();
        Some(Checkpoint {
            tick: self.dec.server_tick(),
            pos: self.next_pos,
            tables: self.tables.clone(),
            key,
            since,
            first: self.first,
            offset: self.offset,
            paused: self.paused,
        })
    }

    /// The demo at `path` decoded as far as `mark`, ready to read on from there.
    pub(super) fn restore(path: &Path, mark: &Checkpoint) -> Result<Cursor> {
        let mut c = Cursor::open_with(path, None)?;
        for t in &mark.tables {
            c.dec.feed(t.bytes(), t.nbits());
        }
        c.dec.feed(mark.key.bytes(), mark.key.nbits());
        for (_, f) in &mark.since {
            c.dec.feed(f.bytes(), f.nbits());
        }
        c.dec.take_output();
        c.reader.seek_to(mark.pos)?;
        c.next = None;
        c.next_pos = mark.pos;
        c.started = true;
        c.first = mark.first;
        c.offset = mark.offset;
        c.paused = mark.paused;
        c.tables.clone_from(&mark.tables);
        c.recent = mark.since.iter().rev().take(RECENT).rev().cloned().collect();
        Ok(c)
    }
}

/// Logs the string table changes up to `mark` (passed over by a jump ahead to it) with `hub`.
pub(super) fn log_tables(hub: &Mutex<Hub>, mark: &Checkpoint) {
    hub.lock().unwrap_or_else(PoisonError::into_inner).log_tables(&mark.tables());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every frame of the demo at `path`, from `c` on.
    fn read_out(mut c: Cursor) -> Cursor {
        while let Some(t) = c.next_tick() {
            if t == i64::MIN {
                c.next = None;
            } else {
                c.decode();
            }
        }
        c
    }

    /// Players leaving or joining rewrite userinfo entries: a jump back past that signs the
    /// clients on again. A live recording of one's own, as no fixture has any.
    #[test]
    #[ignore = "a live recording in STVTF_LIVE_DEMO"]
    fn string_table_entries_rewritten_are_counted() {
        let live = PathBuf::from(std::env::var("STVTF_LIVE_DEMO").unwrap());
        let all = read_out(Cursor::open_with(&live, None).unwrap()).dec.tables_rewritten();
        let mut start = Cursor::open_with(&live, None).unwrap();
        start.next_tick();
        start.decode();
        assert!(start.dec.tables_rewritten() < all, "{} at the start, {all} at the end", start.dec.tables_rewritten());
    }

    #[test]
    fn a_cursor_from_a_checkpoint_reads_on_as_one_from_the_start() {
        let opt = crate::Options { snapshot_hz: 0.0, ..crate::Options::default() };
        let live = crate::demo::record_capture_for_tests("bots_cp_badlands.cap", &opt).1;
        let demo = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pl_upward.dem"));
        for path in [demo, live] {
            let index =
                Arc::new(Index { path: path.clone(), marks: Mutex::new(Vec::new()), stop: AtomicBool::new(false) });
            index_demo(&path, &Arc::downgrade(&index), 300).unwrap();
            let marks = index.marks.lock().unwrap().clone();
            assert!(marks.len() >= 3, "{}: {} checkpoints", path.display(), marks.len());
            let whole = read_out(Cursor::open_with(&path, None).unwrap());
            // A few, spread out: each reads to the end.
            let step = (marks.len() / 4).max(1);
            for mark in marks.iter().skip(1).step_by(step) {
                let from = Cursor::restore(&path, mark).unwrap();
                assert_eq!(from.dec.server_tick(), mark.tick, "{}", path.display());
                let from = read_out(from);
                assert_eq!(from.dec.server_tick(), whole.dec.server_tick());
                assert!(from.dec.world_dump() == whole.dec.world_dump(), "{} from tick {}", path.display(), mark.tick);
            }
        }
    }
}
