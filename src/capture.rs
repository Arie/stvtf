//! Raw inbound datagram captures, for offline replay and tests.
//!
//! Format: b"STVMAPC1" then records `u64 t_us | u8 kind | u32 len | data`.
//! kind 1 = inbound datagram, kind 2 = session start (data = u32 challenge).

use crate::decode::Tables;
use crate::session::{self, Options};
use anyhow::{Result, bail};
use serde_json::Value;
use std::io::{BufWriter, Read, Write};
use std::time::Instant;

const MAGIC: &[u8; 8] = b"STVMAPC1";

pub struct Recorder {
    out: BufWriter<std::fs::File>,
    start: Instant,
}

impl Recorder {
    pub fn create(path: &std::path::Path) -> Result<Self> {
        let mut out = BufWriter::new(std::fs::File::create(path)?);
        out.write_all(MAGIC)?;
        Ok(Recorder { out, start: Instant::now() })
    }

    fn record(&mut self, kind: u8, data: &[u8]) -> Result<()> {
        let t = self.start.elapsed().as_micros() as u64;
        self.out.write_all(&t.to_le_bytes())?;
        self.out.write_all(&[kind])?;
        self.out.write_all(&(data.len() as u32).to_le_bytes())?;
        self.out.write_all(data)?;
        Ok(())
    }

    pub fn session(&mut self, challenge: u32) -> Result<()> {
        self.record(2, &challenge.to_le_bytes())?;
        self.out.flush()?;
        Ok(())
    }

    pub fn datagram(&mut self, data: &[u8]) -> Result<()> {
        self.record(1, data)
    }
}

pub struct Record {
    pub t_us: u64,
    pub kind: u8,
    pub data: Vec<u8>,
}

pub fn read(bytes: &[u8]) -> Result<Vec<Record>> {
    let mut r = bytes;
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        bail!("not a stvmap capture");
    }
    let mut out = Vec::new();
    while r.len() >= 13 {
        let t_us = u64::from_le_bytes(r[..8].try_into()?);
        let kind = r[8];
        let len = u32::from_le_bytes(r[9..13].try_into()?) as usize;
        if r.len() < 13 + len {
            break; // truncated tail of a killed run
        }
        out.push(Record { t_us, kind, data: r[13..13 + len].to_vec() });
        r = &r[13 + len..];
    }
    Ok(out)
}

/// Decodes a capture without a network, one session per recorded connection.
pub fn replay(records: &[Record], tables: &Tables, opt: &Options, emit: &mut dyn FnMut(Value)) -> Result<()> {
    let mut sessions: Vec<(u32, Vec<&[u8]>)> = Vec::new();
    for rec in records {
        match rec.kind {
            2 => sessions.push((u32::from_le_bytes(rec.data[..4].try_into()?), Vec::new())),
            1 => {
                if let Some(last) = sessions.last_mut() {
                    last.1.push(&rec.data);
                }
            }
            _ => {}
        }
    }
    for (challenge, datagrams) in sessions {
        session::replay(challenge, &datagrams, tables, opt, emit)?;
    }
    Ok(())
}
