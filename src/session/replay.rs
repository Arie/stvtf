//! A capture replayed through a session, as fast as it goes or at its own pace.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{Next, Options, Session, Tap, drive, finish};
use crate::Result;
use crate::decode::Tables;
use crate::output::Line;

/// How often a paced replay checks its clock.
const PACE_POLL: Duration = Duration::from_millis(2);

/// Replays one recorded session through the same driver, sending nothing.
pub fn replay(
    challenge: u32,
    datagrams: &[&[u8]],
    tables: &Tables,
    opt: &Options,
    emit: &mut dyn FnMut(Line),
) -> Result<()> {
    let mut s = Session::new(None, challenge, opt);
    let mut it = datagrams.iter();
    let mut next = |buf: &mut [u8]| -> Result<Next> { Ok(it.next().map_or(Next::Stop, |d| Next::recorded(d, buf))) };
    let outcome = drive(&mut s, tables, None, &mut next, emit)?;
    finish(&s, &outcome, emit);
    Ok(())
}

/// Replays one recorded session at the pace it was recorded, `speed` times as fast, into a
/// tap. `wait` pauses the clock while it says so (a relay client still signing on).
#[allow(clippy::too_many_arguments, reason = "internal plumbing")]
pub(crate) fn replay_paced(
    challenge: u32,
    records: &[(u64, &[u8])],
    tables: &Tables,
    opt: &Options,
    speed: f64,
    tap: &dyn Tap,
    wait: &dyn Fn() -> bool,
    stop: &AtomicBool,
    emit: &mut dyn FnMut(Line),
) -> Result<()> {
    let mut s = Session::new(None, challenge, opt);
    s.tap = Some(tap);
    let mut it = records.iter();
    let first = records.first().map_or(0, |r| r.0);
    let mut start = Instant::now();
    let mut next = |buf: &mut [u8]| -> Result<Next> {
        let Some((t_us, d)) = it.next() else { return Ok(Next::Stop) };
        let due = Duration::from_micros(t_us - first).div_f64(speed);
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(Next::Stop);
            }
            if wait() {
                let paused = Instant::now();
                std::thread::sleep(PACE_POLL);
                start += paused.elapsed();
                continue;
            }
            let now = Instant::now();
            let at = start + due;
            if now >= at {
                break;
            }
            std::thread::sleep((at - now).min(PACE_POLL));
        }
        Ok(Next::recorded(d, buf))
    };
    let outcome = drive(&mut s, tables, None, &mut next, emit)?;
    finish(&s, &outcome, emit);
    Ok(())
}
