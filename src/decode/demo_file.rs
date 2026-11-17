//! A `.dem` run through the decoder.

use super::decoder::{SnapshotClock, snapshot_every};
use super::extract::Extractor;
use crate::output::Line;

/// Runs a demo file through the same extractor a live session uses, passing events and
/// snapshots (at most `snapshot_hz` per second; 0 disables them) to `emit`.
pub fn demo_file(bytes: &[u8], snapshot_hz: f32, emit: &mut dyn FnMut(Line)) -> crate::Result<()> {
    let demo = tf_demo_parser::Demo::new(bytes);
    let parser = tf_demo_parser::DemoParser::new_with_analyser(demo.get_stream(), Extractor::default());
    let (_header, mut ticker) = parser.ticker()?;
    let mut snapshots = SnapshotClock::default();
    while ticker.tick()? {
        let x = ticker.state();
        x.announce();
        for line in x.drain() {
            emit(line);
        }
        let now = x.server_tick;
        if now > 0 && snapshots.due(now, snapshot_every(snapshot_hz, x.interval())) {
            emit(Line::Snapshot(x.snapshot()));
        }
    }
    Ok(())
}
