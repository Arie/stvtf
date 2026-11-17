//! Voice added to a demo, as it was heard.

use std::io::{Read, Seek, Write};

use super::command::{CONSOLE_CMD, DATA_TABLES, MESSAGE, SIGNON, STRING_TABLES, SYNC_TICK, USER_CMD};
use super::writer::{end_demo, put};
use super::{HEADER_LEN, Header, HeldTables, Next, Packet, Reader};
use crate::Result;

/// Voice to write into a demo: Opus frames (20 ms each) as the voice of the player in `slot`
/// (their entity less one), whose Steam account is `account`, heard at demo tick `tick`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Voiced {
    /// The demo's tick it is heard at: the server's, less that of the demo's first frame.
    pub tick: i32,
    /// The speaker's slot.
    pub slot: u8,
    /// The speaker's Steam account (`[U:1:<account>]`).
    pub account: u32,
    /// Samples a second the frames are labelled with.
    pub rate: u16,
    /// The Opus packets.
    pub frames: Vec<Vec<u8>>,
}

/// The tick voice due at `due` is heard at, before the frame at `tick`: no earlier than what
/// went `before` it, nor later than the frame (whatever a demo's ticks say).
pub(super) fn heard_at(due: i32, before: i32, tick: i32) -> i32 {
    due.max(before).min(tick)
}

/// Copies demo `src` to `out` with `voices` in it, each a frame of its own before the first of
/// the demo's at or after its tick, as the game's own voice messages (so the demo plays them as
/// the players talking). The header's counts are made anew, and a demo still being written (no
/// stop packet yet) gets one.
pub fn with_voice<R: Read + Seek, W: Write + Seek>(src: R, voices: &[Voiced], mut out: W) -> Result<W> {
    let mut r = Reader::new(src)?;
    let mut header = r.header.clone();
    let interval = if header.ticks > 0 && header.seconds > 0.0 { header.seconds / header.ticks as f32 } else { 0.015 };
    (header.frames, header.ticks) = (0, 0);
    out.write_all(&header.bytes())?;
    let mut order: Vec<&Voiced> = voices.iter().collect();
    order.sort_by_key(|v| v.tick);
    let mut order = order.into_iter().peekable();
    let mut voice = VoiceWriter::default();
    let mut last = 0;
    // Data tables that come before the signon (as older recordings have them) wait for their
    // place in it.
    let (mut tables, mut signed_on) = (HeldTables::<Vec<u8>>::new(None), false);
    loop {
        match r.next_packet()? {
            Next::Packet(Packet::Message { tick, data }) => {
                while let Some(v) = order.next_if(|v| v.tick <= tick) {
                    last = heard_at(v.tick, last, tick);
                    voice.say(&mut out, &mut header, v, last)?;
                }
                put(&mut out, MESSAGE, tick, &data)?;
                last = tick;
                header.frames += 1;
                header.ticks = header.ticks.max(tick.max(0) as u32);
            }
            Next::Packet(Packet::Signon { tick, data }) => {
                if let Some(t) = tables.before(&data) {
                    put(&mut out, DATA_TABLES, tick, &t)?;
                }
                signed_on = true;
                put(&mut out, SIGNON, tick, &data)?;
            }
            Next::Packet(Packet::SyncTick { tick }) => {
                if let Some(t) = tables.at_end() {
                    put(&mut out, DATA_TABLES, tick, &t)?;
                }
                // Everything before the frames is the signon, as the header counts it.
                header.signon_bytes = (out.stream_position()? - HEADER_LEN as u64) as u32;
                put(&mut out, SYNC_TICK, tick, &[])?;
            }
            Next::Packet(Packet::ConsoleCmd { tick, data }) => put(&mut out, CONSOLE_CMD, tick, &data)?,
            Next::Packet(Packet::UserCmd { tick, data }) => put(&mut out, USER_CMD, tick, &data)?,
            Next::Packet(Packet::DataTables { data, .. }) if !signed_on => tables = HeldTables::new(Some(data)),
            Next::Packet(Packet::DataTables { tick, data }) => put(&mut out, DATA_TABLES, tick, &data)?,
            Next::Packet(Packet::StringTables { tick, data }) => put(&mut out, STRING_TABLES, tick, &data)?,
            Next::Packet(Packet::Stop) | Next::Stop | Next::More => break,
        }
    }
    // Said after the demo's last frame: with it.
    for v in order {
        voice.say(&mut out, &mut header, v, last)?;
    }
    end_demo(out, &mut header, interval)
}

/// Voice written into a demo: a speaker's frames are numbered on from one message to the next.
#[derive(Default)]
pub(super) struct VoiceWriter {
    pub(super) numbers: std::collections::HashMap<u8, u16>,
}

impl VoiceWriter {
    /// Writes `v` as frames of its own at `tick`, counted in `header`.
    pub(super) fn say<W: Write>(&mut self, out: &mut W, header: &mut Header, v: &Voiced, tick: i32) -> Result<()> {
        let seq = self.numbers.entry(v.slot).or_insert(0);
        for part in crate::decode::voice_runs(&v.frames) {
            if let Some(m) = crate::decode::steam_voice(v.slot, u64::from(v.account), v.rate, part, seq) {
                put(out, MESSAGE, tick, m.bytes())?;
                header.frames += 1;
            }
        }
        Ok(())
    }
}
