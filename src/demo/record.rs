//! A live session recorded as `.dem` files as it goes: the signon a connection got, then one
//! frame per datagram, every message as the server sent it. A new connection (a reconnect, a new
//! map) starts a new file, with the signon that connection got.

use super::{Header, Writer};
use crate::decode::{Lifecycle, Tables};
use crate::net::bits::{BitBuf, BitReader, BitWriter};
use crate::net::messages::{NET_TICK, SVC_PACKETENTITIES};
use crate::session::Tap;
use std::fs::File;
use std::io::BufWriter;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The header's counts are written again this often; every frame reaches the file at once, so a
/// file being written can be played while it grows.
const FLUSH_EVERY: Duration = Duration::from_millis(100);

/// Where a recorded session's demos go.
pub trait Files: Send + Sync {
    /// A demo of `map` whose first frame is at server tick `first_tick`: the file to write it to.
    fn create(&self, map: &str, first_tick: u32) -> std::io::Result<File>;
}

#[derive(Default)]
struct State {
    map: String,
    /// The data tables of the build, from its tables.
    datatables: Vec<u8>,
    /// The signon so far: each stream byte padded.
    signon: Vec<Vec<u8>>,
    in_signon: bool,
    /// This datagram's streams, each cut off after its last message.
    datagram: Option<BitWriter>,
    /// The tick of the frame in `datagram`, once one has a `net_Tick`.
    frame_tick: Option<u32>,
    tick: Option<u32>,
    writer: Option<Writer<BufWriter<File>>>,
    flushed: Option<Instant>,
    /// A file could not be made or written: nothing more until the next connection.
    failed: bool,
}

/// The tap that writes the demos.
pub(crate) struct Recorder {
    files: Arc<dyn Files>,
    state: Mutex<State>,
}

impl Recorder {
    pub(crate) fn new(files: Arc<dyn Files>) -> Self {
        Recorder { files, state: Mutex::new(State { in_signon: true, ..State::default() }) }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A stream up to the end of its last message: what follows is padding, and followed by more it
/// would be read as messages.
fn cut(bits: &BitBuf, life: &Lifecycle) -> BitBuf {
    let end = life.spans.last().map_or(0, |s| s.bits.end);
    BitReader::slice(bits.bytes(), 0..end)
}

impl Tap for Recorder {
    fn stream(&self, bits: &BitBuf, _reliable: bool, life: &Lifecycle) {
        let mut st = self.state();
        if let Some((_, map)) = &life.server_info {
            // A new signon: on a new connection, or a new map on this one.
            if let Some(w) = st.writer.take() {
                let _ = w.finish();
            }
            st.map.clone_from(map);
            st.signon.clear();
            st.in_signon = true;
            st.failed = false;
        }
        let tick = life.tick.map(|t| t.max(0) as u32);
        let entities = life.spans.iter().any(|s| u32::from(s.id) == SVC_PACKETENTITIES);
        let msgs = cut(bits, life);
        if st.in_signon && !entities {
            st.tick = tick.or(st.tick);
            if msgs.nbits() > 0 {
                st.signon.push(msgs.bytes().to_vec());
            }
            return;
        }
        st.in_signon = false;
        // One frame per packet: a datagram can hold two (the full update after signon comes
        // reliably, beside the next frame's delta).
        if life.spans.iter().any(|s| u32::from(s.id) == NET_TICK) && st.frame_tick.is_some() {
            self.write(&mut st);
        }
        if let Some(t) = tick {
            st.tick = Some(t);
            st.frame_tick.get_or_insert(t);
        }
        st.datagram.get_or_insert_with(BitWriter::new).write_buf(&msgs);
    }

    fn flush(&self) {
        let mut st = self.state();
        self.write(&mut st);
    }

    fn want_full_update(&self) -> bool {
        false
    }

    fn server_build(&self, _build: &str) {}

    fn tables(&self, tables: &Tables) {
        tables.datatables_bytes().clone_into(&mut self.state().datatables);
    }
}

impl Recorder {
    /// Writes the frame gathered so far as a packet, starting the file with the first.
    fn write(&self, st: &mut State) {
        let frame_tick = st.frame_tick.take();
        let Some(datagram) = st.datagram.take() else { return };
        if st.failed {
            return;
        }
        let tick = frame_tick.or(st.tick).unwrap_or(0);
        if st.writer.is_none() {
            let started = self.files.create(&st.map, tick).map_err(anyhow::Error::from).and_then(|file| {
                let header = Header {
                    protocol: crate::net::handshake::PROTOCOL_VERSION as u32,
                    server: "stvtf".into(),
                    client: "SourceTV Demo".into(),
                    map: st.map.clone(),
                    game: "tf".into(),
                    seconds: 0.0,
                    ticks: 0,
                    frames: 0,
                    signon_bytes: 0,
                };
                let mut w = Writer::new(BufWriter::new(file), header, crate::DEFAULT_TICK_INTERVAL)?;
                w.data_tables(&st.datatables)?;
                for s in &st.signon {
                    w.signon(s)?;
                }
                Ok(w)
            });
            match started {
                Ok(w) => st.writer = Some(w),
                Err(e) => {
                    eprintln!("stvtf: recording a demo: {e:#}");
                    st.failed = true;
                    return;
                }
            }
        }
        let due = st.flushed.is_none_or(|t| t.elapsed() >= FLUSH_EVERY);
        let Some(w) = st.writer.as_mut() else { return };
        let mut written = w.message(tick as i32, datagram.finish().bytes());
        // Every frame reaches the file at once (one that pauses the match is the last for as long
        // as the pause lasts); the header's counts less often.
        if written.is_ok() {
            written = if due { w.flush() } else { w.flush_packets() };
        }
        if due {
            st.flushed = Some(Instant::now());
        }
        if let Err(e) = written {
            eprintln!("stvtf: writing a demo: {e:#}");
            st.writer = None;
            st.failed = true;
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(w) = self.state().writer.take() {
            let _ = w.finish();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::capture;
    use crate::output::Line;
    use crate::session::{self, Options};
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    /// Files in a folder of their own, named after their first tick.
    struct Folder(PathBuf);

    impl Files for Folder {
        fn create(&self, map: &str, first_tick: u32) -> std::io::Result<File> {
            File::create_new(self.0.join(format!("{map}-{first_tick}.dem")))
        }
    }

    fn data(lines: Vec<Line>) -> Vec<Line> {
        lines.into_iter().filter(|l| !matches!(l, Line::Status(_))).collect()
    }

    /// The first session of capture fixture `name`, recorded as a demo: what it decoded to, and the
    /// demo's file.
    pub(crate) fn record_capture(name: &str, opt: &Options) -> (Vec<Line>, PathBuf) {
        static MADE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let bytes = std::fs::read(crate::fixture(name)).unwrap();
        let records = capture::read(&bytes).unwrap();
        let tables = crate::watch::tables(crate::CURRENT_BUILD, None).unwrap();
        let mut want = Vec::new();
        capture::replay(&records, &tables, opt, &mut |l| want.push(l)).unwrap();
        let n = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("stvtf-record-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let recorder = Recorder::new(Arc::new(Folder(dir.clone())));
        recorder.tables(&tables);
        let mut sessions = records.iter().skip_while(|r| r.kind != capture::Kind::Session as u8);
        let challenge = sessions.next().unwrap().challenge().unwrap();
        let datagrams: Vec<(u64, &[u8])> = sessions
            .take_while(|r| r.kind != capture::Kind::Session as u8)
            .filter(|r| r.kind == capture::Kind::Datagram as u8)
            .map(|r| (r.t_us, r.data))
            .collect();
        let stop = AtomicBool::new(false);
        session::replay_paced(challenge, &datagrams, &tables, opt, 1e9, &recorder, &|| false, &stop, &mut |_| {})
            .unwrap();
        drop(recorder);
        let files: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(files.len(), 1, "{files:?}");
        (want, files[0].clone())
    }

    #[test]
    fn a_recorded_session_plays_as_the_session_did() {
        // A snapshot every frame, so both have the same.
        let opt = Options { snapshot_hz: 1000.0, ..Options::default() };
        let (want, file) = record_capture("bots_cp_badlands.cap", &opt);
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("cp_badlands-"), "{name}");
        let demo = std::fs::read(&file).unwrap();
        std::fs::remove_dir_all(file.parent().unwrap()).unwrap();
        let header = Header::parse(&demo).unwrap();
        assert_eq!(header.map, "cp_badlands");
        assert!(header.frames > 100 && header.ticks > 100 && header.signon_bytes > 1000, "{header:?}");
        let mut got = Vec::new();
        crate::decode::demo_file(&demo, 1000.0, &mut |l| got.push(l)).unwrap();
        let (got, want) = (data(got), data(want));
        // Events are the same, and so are the snapshots of every frame (a demo read as one also has
        // one at the signon's tick).
        let events = |l: &Vec<Line>| l.iter().filter(|l| !matches!(l, Line::Snapshot(_))).cloned().collect::<Vec<_>>();
        assert_eq!(events(&got), events(&want));
        let snaps = |l: &Vec<Line>| -> std::collections::BTreeMap<u32, Line> {
            l.iter().filter_map(|l| if let Line::Snapshot(s) = l { Some((s.tick, l.clone())) } else { None }).collect()
        };
        let (g, w) = (snaps(&got), snaps(&want));
        let both: Vec<_> = w.keys().filter(|t| g.contains_key(t)).collect();
        assert!(both.len() * 10 > w.len() * 9, "{} of {} snapshots at the same ticks", both.len(), w.len());
        for t in both {
            assert_eq!(g[t], w[t], "at tick {t}");
        }
    }
}
