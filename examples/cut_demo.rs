//! Cuts a TF2 .dem after a given tick, for small test fixtures.
//!
//! ```text
//! cargo run --example cut_demo -- IN.dem OUT.dem TICK
//! ```
//!
//! TICK is a demo tick, counted from the start of the recording (not the server tick).
//! Copies the header and every packet up to that tick, then ends the demo with a stop
//! packet and fixes up the header's tick, frame and duration counts. A demo is a
//! stream of deltas, so only its end can be cut, never its start or middle.
//!
//! The demo's packet layout is in `common/demo.rs`.

#[path = "common/demo.rs"]
mod demo;

use anyhow::{Context, Result, bail};
use demo::{DURATION, FRAMES, HEADER, PACKET, TICKS, i32_at, packets};

fn cut(data: &[u8], last_tick: i32) -> Result<Vec<u8>> {
    let packets = packets(data)?;
    let mut out = data[..HEADER].to_vec();
    let mut range: Option<(i32, i32)> = None;
    let mut frames: i32 = 0;
    for p in packets {
        // The setup before the first game packet (signon, data and string tables) can carry any
        // tick; only game ticks count towards the cut.
        if (p.cmd == PACKET || range.is_some()) && p.tick > last_tick {
            break;
        }
        out.extend_from_slice(&data[p.start..p.end]);
        if p.cmd == PACKET {
            frames += 1;
            range = Some((range.map_or(p.tick, |r| r.0), p.tick));
        }
    }
    let (first, last) = range.context("no game packets before that tick")?;
    demo::stop(&mut out, last);

    let ticks = i32_at(data, TICKS)?;
    let secs = f32::from_le_bytes(data[DURATION..DURATION + 4].try_into()?);
    let per_tick = if ticks > 0 { secs / ticks as f32 } else { 0.015 };
    out[DURATION..DURATION + 4].copy_from_slice(&((last - first) as f32 * per_tick).to_le_bytes());
    out[TICKS..TICKS + 4].copy_from_slice(&(last - first).to_le_bytes());
    out[FRAMES..FRAMES + 4].copy_from_slice(&frames.to_le_bytes());
    eprintln!("kept ticks {first}..{last}, {frames} frames, {} MiB", out.len() >> 20);
    Ok(out)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output, tick] = args.as_slice() else {
        bail!("usage: cargo run --example cut_demo -- IN.dem OUT.dem TICK");
    };
    let tick: i32 = tick.parse().with_context(|| format!("TICK {tick}: not a number"))?;
    let data = std::fs::read(input).with_context(|| format!("reading {input}"))?;
    let out = cut(&data, tick)?;
    std::fs::File::create_new(output)
        .and_then(|mut f| std::io::Write::write_all(&mut f, &out))
        .with_context(|| format!("writing {output}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use demo::{CMD_INFO, DATA_TABLES, MAGIC, SIGNON, STRING_TABLES, SYNC_TICK};

    fn packet(out: &mut Vec<u8>, cmd: u8, tick: i32) {
        out.push(cmd);
        out.extend_from_slice(&tick.to_le_bytes());
        match cmd {
            SIGNON | PACKET => {
                out.extend_from_slice(&[0; CMD_INFO + 8]);
                out.extend_from_slice(&1i32.to_le_bytes());
                out.push(cmd);
            }
            DATA_TABLES | STRING_TABLES => {
                out.extend_from_slice(&1i32.to_le_bytes());
                out.push(cmd);
            }
            _ => {}
        }
    }

    fn demo(setup_tick: i32) -> Vec<u8> {
        let mut d = MAGIC.to_vec();
        d.resize(HEADER, 0);
        for cmd in [SIGNON, DATA_TABLES, STRING_TABLES] {
            packet(&mut d, cmd, setup_tick);
        }
        packet(&mut d, SYNC_TICK, 0);
        for tick in [2, 4, 10] {
            packet(&mut d, PACKET, tick);
        }
        d
    }

    #[test]
    fn setup_before_the_game_is_kept_whatever_its_tick() {
        // Some recorders stamp the data and string tables with a tick past the game's first ones.
        for setup_tick in [0, i32::MIN, 5160] {
            let cut = cut(&demo(setup_tick), 5).unwrap();
            let kept: Vec<(u8, i32)> = packets(&cut).unwrap().iter().map(|p| (p.cmd, p.tick)).collect();
            assert_eq!(
                kept,
                [
                    (SIGNON, setup_tick),
                    (DATA_TABLES, setup_tick),
                    (STRING_TABLES, setup_tick),
                    (SYNC_TICK, 0),
                    (PACKET, 2),
                    (PACKET, 4)
                ]
            );
        }
    }
}
