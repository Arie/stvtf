//! Raw inbound datagram captures, for offline replay and tests.
//!
//! Format: b"STVMAPC1" then records `u64 t_us | u8 kind | u32 len | data`.
//! kind 1 = inbound datagram, kind 2 = session start (data = u32 challenge).
//! Only inbound traffic is recorded, so the password (sent outbound) never is.

use crate::decode::Tables;
use crate::session::{self, Options};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

const MAGIC: &[u8; 8] = b"STVMAPC1";
const RECORD_HEADER: usize = 13;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Datagram = 1,
    Session = 2,
}

#[derive(Debug)]
pub struct Recorder {
    out: BufWriter<File>,
    start: Instant,
}

impl Recorder {
    /// Creates a new capture file; an existing file is never overwritten.
    pub fn create(path: &Path) -> Result<Self> {
        let file = File::create_new(path).with_context(|| format!("creating capture {}", path.display()))?;
        let mut out = BufWriter::new(file);
        out.write_all(MAGIC)?;
        Ok(Recorder { out, start: Instant::now() })
    }

    fn record(&mut self, kind: Kind, data: &[u8]) -> Result<()> {
        let t = u64::try_from(self.start.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.out.write_all(&t.to_le_bytes())?;
        self.out.write_all(&[kind as u8])?;
        self.out.write_all(&u32::try_from(data.len())?.to_le_bytes())?;
        self.out.write_all(data)?;
        Ok(())
    }

    pub fn session(&mut self, challenge: u32) -> Result<()> {
        self.record(Kind::Session, &challenge.to_le_bytes())?;
        self.out.flush()?;
        Ok(())
    }

    pub fn datagram(&mut self, data: &[u8]) -> Result<()> {
        self.record(Kind::Datagram, data)
    }
}

/// One record, borrowing its data from the capture bytes.
#[derive(Debug)]
pub struct Record<'a> {
    pub t_us: u64,
    /// The raw kind byte; unknown kinds are skipped on replay.
    pub kind: u8,
    pub data: &'a [u8],
}

pub fn read(bytes: &[u8]) -> Result<Vec<Record<'_>>> {
    let Some(mut r) = bytes.strip_prefix(MAGIC) else { bail!("not a stvmap capture") };
    let mut out = Vec::new();
    while let Some(header) = r.get(..RECORD_HEADER) {
        let t_us = u64::from_le_bytes(header[..8].try_into()?);
        let kind = header[8];
        let len = u32::from_le_bytes(header[9..13].try_into()?) as usize;
        let rest = &r[RECORD_HEADER..];
        // A truncated tail is what a killed run leaves behind.
        let Some(data) = rest.get(..len) else { break };
        out.push(Record { t_us, kind, data });
        r = &rest[len..];
    }
    Ok(out)
}

/// Decodes a capture without a network, one session per recorded connection.
pub fn replay(records: &[Record<'_>], tables: &Tables, opt: &Options, emit: &mut dyn FnMut(Value)) -> Result<()> {
    let mut sessions: Vec<(u32, Vec<&[u8]>)> = Vec::new();
    for rec in records {
        if rec.kind == Kind::Session as u8 {
            let challenge: [u8; 4] = rec.data.try_into().context("session record is not a 4-byte challenge")?;
            sessions.push((u32::from_le_bytes(challenge), Vec::new()));
        } else if rec.kind == Kind::Datagram as u8
            && let Some(last) = sessions.last_mut()
        {
            last.1.push(rec.data);
        }
    }
    for (challenge, datagrams) in sessions {
        session::replay(challenge, &datagrams, tables, opt, emit)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: u8, data: &[u8]) -> Vec<u8> {
        let mut v = 7u64.to_le_bytes().to_vec();
        v.push(kind);
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn reads_records_and_tolerates_a_truncated_tail() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(record(2, &5u32.to_le_bytes()));
        bytes.extend(record(1, b"datagram"));
        let mut cut = record(1, b"cut short");
        cut.truncate(cut.len() - 3);
        bytes.extend(cut);
        let recs = read(&bytes).unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[1].data, b"datagram");
        assert_eq!(recs[1].t_us, 7);
    }

    #[test]
    fn rejects_other_files() {
        assert!(read(b"NOTSTVMAP").is_err());
        assert!(read(b"STV").is_err());
    }

    #[test]
    fn short_session_record_is_an_error_not_a_panic() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(record(2, &[1]));
        let recs = read(&bytes).unwrap();
        let tables = Tables::from_demo_bytes(crate::builtin(crate::CURRENT_BUILD).unwrap().tables).unwrap();
        assert!(replay(&recs, &tables, &Options::default(), &mut |_| {}).is_err());
    }

    #[test]
    fn recorder_never_overwrites() {
        let path = std::env::temp_dir().join(format!("stvmap-test-{}.cap", std::process::id()));
        std::fs::write(&path, b"precious").unwrap();
        assert!(Recorder::create(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"precious");
        std::fs::remove_file(&path).unwrap();
    }
}
