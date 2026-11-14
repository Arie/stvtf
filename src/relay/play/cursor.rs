//! A demo being read and decoded, frame by frame.

use super::{NOT_A_FRAME, RECENT};
use crate::decode::{Decoder, Lifecycle, Span, Tables};
use crate::demo::{self, Next, Packet};
use crate::lock;
use crate::net::bits::{BitBuf, BitReader, BitWriter};
use crate::net::messages::{self, NET_SIGNONSTATE, TYPE_BITS};
use crate::relay::Hub;
use crate::relay::hub::Part;
use anyhow::{Context, Result, bail};
use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

/// The `SendTable` CRC of this build's class tables, which a game client builds its classes from.
static OUR_CRC: LazyLock<Option<u32>> = LazyLock::new(|| {
    let b = crate::builtin(crate::CURRENT_BUILD)?;
    Tables::from_demo_bytes(b.tables).ok().map(|t| t.sendtable_crc())
});

/// A demo being played: where in the file, and its decoded state.
pub(super) struct Cursor {
    pub(super) path: PathBuf,
    pub(super) reader: demo::Reader<File>,
    pub(super) dec: Decoder,
    /// The next packet, read but not played.
    pub(super) next: Option<Packet>,
    /// A frame's server tick less its demo tick, as of the last frame.
    pub(super) offset: i64,
    /// A frame came: the entities are there.
    pub(super) started: bool,
    /// The server tick of the first frame.
    pub(super) first: Option<i64>,
    /// The demo's stop packet was read: no more frames come.
    pub(super) ended: bool,
    /// The last frames decoded, by server tick, cut after their last message.
    pub(super) recent: VecDeque<(u32, BitBuf)>,
    /// The match itself was paused (`svc_SetPause` in the demo) as of the last frame decoded: a
    /// client that jumps here, past the frame that said so, is told.
    pub(super) paused: bool,
    /// Where `next` starts in the file.
    pub(super) next_pos: u64,
    /// Every string table change since the signon, in order: what a checkpoint starts from.
    pub(super) tables: Vec<Arc<BitBuf>>,
}

/// The demo's signon is not all written yet.
#[derive(Debug)]
struct NotYet(PathBuf);

impl std::fmt::Display for NotYet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} has no signon yet", self.0.display())
    }
}

impl std::error::Error for NotYet {}

/// Whether a demo that did not open for `e` may yet: it is not there, or not written far enough.
pub(super) fn may_open_yet(e: &anyhow::Error) -> bool {
    use std::io::ErrorKind::{NotFound, UnexpectedEof};
    e.chain().any(|c| {
        c.is::<NotYet>()
            || c.downcast_ref::<std::io::Error>().is_some_and(|e| matches!(e.kind(), NotFound | UnexpectedEof))
    })
}

impl Cursor {
    /// Opens a demo and reads it up to its first frame, passing its signon to `hub`.
    pub(super) fn open(path: &Path, hub: &Mutex<Hub>) -> Result<Cursor> {
        Self::open_with(path, Some(hub))
    }

    /// [`Cursor::open`], giving the signon to nobody: the clients have the demo's already (a jump
    /// in it), or it is decoded on the side.
    pub(super) fn open_detached(path: &Path) -> Result<Cursor> {
        Self::open_with(path, None)
    }

    fn open_with(path: &Path, hub: Option<&Mutex<Hub>>) -> Result<Cursor> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut reader = demo::Reader::new(file)?;
        let (signon, tables, next) = read_signon(path, &mut reader)?;
        if Some(tables.sendtable_crc()) != *OUR_CRC {
            bail!("the demo is of a TF2 build with other class tables, which a game client cannot play");
        }
        let mut dec = Decoder::new(&tables, 0.0)?;
        dec.keep_world();
        feed_signon(&mut dec, signon, hub);
        Ok(Cursor {
            path: path.to_owned(),
            reader,
            dec,
            next,
            offset: 0,
            started: false,
            first: None,
            ended: false,
            recent: VecDeque::default(),
            paused: false,
            next_pos: 0,
            tables: Vec::new(),
        })
    }

    /// The next packet, read ahead; None when the demo has no more yet.
    pub(super) fn peek(&mut self) -> Option<&Packet> {
        if self.next.is_none() && !self.ended {
            self.next_pos = self.reader.position();
            self.next = match self.reader.next_packet() {
                Ok(Next::Packet(p)) => Some(p),
                Ok(Next::More) => None,
                Ok(Next::Stop) => {
                    self.ended = true;
                    None
                }
                // A demo cut off in a broken packet (a recording stopped short, say) ends there.
                Err(e) => {
                    log!("{} ends in an unreadable packet: {e:#}", self.path.display());
                    self.ended = true;
                    None
                }
            };
        }
        self.next.as_ref()
    }

    /// The data of the packet read ahead, if it is a frame.
    pub(super) fn next_frame(&self) -> Option<&[u8]> {
        match &self.next {
            Some(Packet::Message { data, .. }) => Some(data),
            _ => None,
        }
    }

    /// The server tick the next frame has: its own `net_Tick`, or as far as the frames so far tell
    /// (a demo's tick less the server's, which a pause changes: an STV demo has no frames while the
    /// server is paused); None when there is none yet, [`NOT_A_FRAME`] for another packet.
    pub(super) fn next_tick(&mut self) -> Option<i64> {
        let offset = self.offset;
        self.peek()?;
        Some(match &self.next {
            Some(Packet::Message { tick, data }) => {
                let guess = i64::from(*tick) + offset;
                self.dec.frame_tick(data).map_or(guess, i64::from)
            }
            _ => NOT_A_FRAME,
        })
    }

    /// Decodes the next packet; a frame's stream and what decoding found, or None for a packet
    /// that is not one.
    pub(super) fn decode(&mut self) -> Option<(BitBuf, Lifecycle)> {
        let Some(Packet::Message { tick, data }) = self.next.take() else { return None };
        let bits = BitBuf::from_bytes(data);
        let life = self.dec.feed(bits.bytes(), bits.nbits());
        let (bits, life) = without_signon(&bits, &life);
        self.dec.take_output();
        self.offset = i64::from(self.dec.server_tick()) - i64::from(tick);
        self.started = true;
        self.first.get_or_insert(i64::from(self.dec.server_tick()));
        for span in life.spans.iter().filter(|s| u32::from(s.id) == messages::SVC_SETPAUSE) {
            if let Ok(p) = BitReader::at(bits.bytes(), span.bits.start + TYPE_BITS).read_bit() {
                self.paused = p;
            }
        }
        self.tables.extend(crate::relay::server_msg::table_changes(&bits, &life).map(Arc::new));
        self.recent.push_back((self.dec.server_tick(), life.cut(&bits)));
        if self.recent.len() > RECENT {
            self.recent.pop_front();
        }
        Some((bits, life))
    }

    /// Passes over the packet read ahead.
    pub(super) fn skip_next(&mut self) {
        self.next = None;
    }

    /// Where playback is: the server tick of the last frame.
    pub(super) fn tick(&self) -> i64 {
        i64::from(self.dec.server_tick())
    }
}

/// A demo's signon packets, read up to its first frame, its data tables, and that frame if it is
/// written.
fn read_signon(path: &Path, reader: &mut demo::Reader<File>) -> Result<(Vec<Vec<u8>>, Tables, Option<Packet>)> {
    let mut signon = Vec::new();
    let mut tables = None;
    let mut synced = false;
    let next = loop {
        match reader.next_packet()? {
            Next::Packet(Packet::Signon { data, .. }) => signon.push(data),
            Next::Packet(Packet::DataTables { .. }) => tables = Some(tables_of(path, reader)?),
            Next::Packet(p @ Packet::Message { .. }) => break Some(p),
            Next::Packet(Packet::SyncTick { .. }) => synced = true,
            Next::Packet(_) => {}
            // A live demo can be read before its first frame is written; its signon is all
            // there once the sync tick that ends it is.
            Next::More if tables.is_some() && synced => break None,
            Next::More => return Err(NotYet(path.to_owned()).into()),
            Next::Stop => bail!("{} has no frames", path.display()),
        }
    };
    let tables = tables.context("the demo has no data tables")?;
    Ok((signon, tables, next))
}

/// Decodes the `signon` packets, and passes them to `hub` if any, with the spawn count a game
/// client takes.
fn feed_signon(dec: &mut Decoder, signon: Vec<Vec<u8>>, hub: Option<&Mutex<Hub>>) {
    let mut spawncount = None;
    for data in signon {
        let bits = BitBuf::from_bytes(data);
        let life = dec.feed(bits.bytes(), bits.nbits());
        let Some(hub) = hub else { continue };
        if let Some((count, _)) = &life.server_info {
            spawncount = Some(*count);
        }
        let (bits, life) = match spawncount {
            Some(count) => with_spawncount(&bits, &life, count),
            None => (bits, life),
        };
        let mut h = lock(hub);
        h.upstream_stream(&bits, Part::Reliable, &life);
        h.upstream_flush();
    }
}

/// A signon stream with its `net_SignonState`s at `count`, the `ServerInfo`'s: a game client
/// refuses any other ("Received wrong spawn count"), and an STV demo's differ from its
/// `ServerInfo`'s.
fn with_spawncount(bits: &BitBuf, life: &Lifecycle, count: i32) -> (BitBuf, Lifecycle) {
    let mut w = BitWriter::new();
    let mut spans = Vec::with_capacity(life.spans.len());
    for span in &life.spans {
        let msg = BitReader::slice(bits.bytes(), span.bits.clone());
        let msg = match crate::relay::server_msg::signon_state(span.id, &msg) {
            Some(s) => messages::signonstate(s, count),
            None => msg,
        };
        let start = w.nbits();
        w.write_buf(&msg);
        spans.push(Span { id: span.id, bits: start..w.nbits() });
    }
    let fixed = Lifecycle {
        signon: life.signon.clone(),
        server_info: life.server_info.clone(),
        tick: life.tick,
        spans,
        ..Lifecycle::default()
    };
    (w.finish(), fixed)
}

/// A frame's stream without the server's `net_SignonState`s (a changelevel, say): a client's
/// signon is the relay's. A `net_Disconnect` ends a stream, and is cut with what follows it. One in
/// what the decoder could not parse (an [`UNPARSED`](crate::decode::UNPARSED) span) goes through.
fn without_signon(bits: &BitBuf, life: &Lifecycle) -> (BitBuf, Lifecycle) {
    let (bits, life) = without(bits, life, |s| u32::from(s.id) == NET_SIGNONSTATE);
    (bits, Lifecycle { signon: Vec::new(), ..life })
}

/// A frame's stream without the director's camera cues.
pub(super) fn without_cues(dec: &Decoder, bits: &BitBuf, life: &Lifecycle) -> (BitBuf, Lifecycle) {
    without(bits, life, |s| dec.is_director_cue(bits.bytes(), s))
}

/// A stream without the messages `drop` picks.
fn without(bits: &BitBuf, life: &Lifecycle, drop: impl Fn(&Span) -> bool) -> (BitBuf, Lifecycle) {
    if !life.spans.iter().any(&drop) {
        return (bits.clone(), life.clone());
    }
    let mut w = BitWriter::new();
    let mut spans = Vec::with_capacity(life.spans.len());
    for span in life.spans.iter().filter(|s| !drop(s)) {
        let start = w.nbits();
        w.write_range(bits.bytes(), span.bits.clone());
        spans.push(Span { id: span.id, bits: start..w.nbits() });
    }
    (w.finish(), Lifecycle { spans, ..life.clone() })
}

/// String table changes as messages of their own.
pub(super) fn owned(tables: &[Arc<BitBuf>]) -> Vec<BitBuf> {
    tables.iter().map(|t| (**t).clone()).collect()
}

/// The tables of the demo at `path`, read up to where `reader` is: just past its data tables.
fn tables_of(path: &Path, reader: &demo::Reader<File>) -> Result<Tables> {
    let mut prefix = vec![0u8; reader.position() as usize];
    File::open(path)?.read_exact(&mut prefix)?;
    Tables::from_demo_bytes(&prefix)
}
