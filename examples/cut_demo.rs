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
//! Packet layout (demo protocol 3): u8 command, i32 tick, then by command:
//!
//! ```text
//! 1 signon / 2 packet: 76 bytes cmd info, i32 in seq, i32 out seq, i32 length, data
//! 3 sync tick:         nothing
//! 4 console command:   i32 length, data
//! 5 user command:      i32 sequence, i32 length, data
//! 6 data tables:       i32 length, data
//! 7 stop:              end of the demo
//! 8 string tables:     i32 length, data
//! ```

use anyhow::{Context, Result, bail};

const MAGIC: &[u8; 8] = b"HL2DEMO\0";
const HEADER: usize = 1072;
// Header fields after the four 260-byte strings.
const DURATION: usize = 1056;
const TICKS: usize = 1060;
const FRAMES: usize = 1064;
const CMD_INFO: usize = 76;
const SIGNON: u8 = 1;
const PACKET: u8 = 2;
const STOP: u8 = 7;

fn i32_at(data: &[u8], pos: usize) -> Result<i32> {
    let bytes = data.get(pos..pos + 4).context("demo truncated")?;
    Ok(i32::from_le_bytes(bytes.try_into()?))
}

/// A length field followed by that many bytes, starting at `pos`; returns the end.
fn skip_sized(data: &[u8], pos: usize) -> Result<usize> {
    let len = usize::try_from(i32_at(data, pos)?).context("negative length")?;
    Ok(pos + 4 + len)
}

struct Packet {
    start: usize,
    end: usize,
    cmd: u8,
    tick: i32,
}

/// The packets after the header, up to a stop packet or the end of the file.
fn packets(data: &[u8]) -> Result<Vec<Packet>> {
    let mut out = Vec::new();
    let mut pos = HEADER;
    // Some demos end without a stop packet, in a few bytes of trailer.
    while pos + 5 <= data.len() {
        let start = pos;
        let cmd = data[pos];
        let tick = i32_at(data, pos + 1)?;
        pos += 5;
        pos = match cmd {
            SIGNON | PACKET => skip_sized(data, pos + CMD_INFO + 8)?,
            3 => pos,
            4 | 6 | 8 => skip_sized(data, pos)?,
            5 => skip_sized(data, pos + 4)?,
            STOP => break,
            _ => bail!("unknown demo command {cmd} at byte {start}"),
        };
        if pos > data.len() {
            bail!("demo truncated in the packet at byte {start}");
        }
        out.push(Packet { start, end: pos, cmd, tick });
    }
    Ok(out)
}

fn cut(data: &[u8], last_tick: i32) -> Result<Vec<u8>> {
    if data.len() < HEADER || &data[..8] != MAGIC {
        bail!("not a Source demo");
    }
    let mut out = data[..HEADER].to_vec();
    let mut range: Option<(i32, i32)> = None;
    let mut frames: i32 = 0;
    for p in packets(data)? {
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
    out.push(STOP);
    out.extend_from_slice(&last.to_le_bytes());
    out.push(0); // the engine writes one padding byte after the stop command

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

    fn packet(out: &mut Vec<u8>, cmd: u8, tick: i32) {
        out.push(cmd);
        out.extend_from_slice(&tick.to_le_bytes());
        match cmd {
            SIGNON | PACKET => {
                out.extend_from_slice(&[0; CMD_INFO + 8]);
                out.extend_from_slice(&1i32.to_le_bytes());
                out.push(cmd);
            }
            6 | 8 => {
                out.extend_from_slice(&1i32.to_le_bytes());
                out.push(cmd);
            }
            _ => {}
        }
    }

    fn demo(setup_tick: i32) -> Vec<u8> {
        let mut d = MAGIC.to_vec();
        d.resize(HEADER, 0);
        for cmd in [SIGNON, 6, 8] {
            packet(&mut d, cmd, setup_tick);
        }
        packet(&mut d, 3, 0);
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
                [(SIGNON, setup_tick), (6, setup_tick), (8, setup_tick), (3, 0), (PACKET, 2), (PACKET, 4)]
            );
        }
    }
}
