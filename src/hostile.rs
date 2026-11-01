//! Whatever comes off the network or out of a file may be garbage or forged: it must never
//! panic. The inputs are random but seeded, so a failure repeats. For a longer hunt,
//! `STVTF_FUZZ_ROUNDS` multiplies how many are tried and `STVTF_FUZZ_SEED` picks other inputs;
//! the same two values give the same inputs again.

use crate::capture;
use crate::decode::{self, Decoder, Tables};
use crate::net::compress;
use crate::net::handshake::{parse_a2s_version, parse_challenge, parse_from_client};
use crate::net::netchan::{self, Kind};
use crate::net::receiver::Receiver;
use crate::output::{Line, Status};
use crate::session::{self, Options};
use std::time::Instant;
use typed_arena::Arena;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn rounds(default: u64) -> u64 {
    default * std::env::var("STVTF_FUZZ_ROUNDS").ok().and_then(|r| r.parse().ok()).unwrap_or(1)
}

/// xorshift64*: small and seeded, for repeatable inputs.
struct Rng(u64);

impl Rng {
    /// The inputs numbered `n`, of those `STVTF_FUZZ_SEED` picks.
    fn seeded(n: u64) -> Self {
        let run = std::env::var("STVTF_FUZZ_SEED").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        // xorshift never leaves 0.
        Rng((n ^ run.wrapping_mul(0x9E37_79B9_7F4A_7C15)).max(1))
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let len = self.below(max + 1);
        (0..len).map(|_| self.next() as u8).collect()
    }

    /// Corrupts `data`: flipped bits, a random byte, or a cut-off end.
    fn mutate(&mut self, data: &mut Vec<u8>) {
        if data.is_empty() {
            return;
        }
        match self.below(4) {
            0 => {
                let at = self.below(data.len());
                data[at] ^= 1 << self.below(8);
            }
            1 => {
                let at = self.below(data.len());
                data[at] = self.next() as u8;
            }
            2 => {
                let at = self.below(data.len());
                data[at] = 0xff;
            }
            _ => data.truncate(self.below(data.len())),
        }
    }
}

fn tables() -> Tables {
    Tables::from_demo_bytes(crate::builtin(crate::CURRENT_BUILD).unwrap().tables).unwrap()
}

/// The prefix a packet needs to get past the first check of each parser, to reach the next ones.
const PREFIXES: [&[u8]; 8] = [
    b"",
    b"\xff\xff\xff\xff",
    b"\xff\xff\xff\xffq",
    b"\xff\xff\xff\xffk",
    b"\xff\xff\xff\xffA",
    b"\xff\xff\xff\xff9",
    b"\xff\xff\xff\xffI",
    b"\xfe\xff\xff\xff",
];

#[test]
fn packets_out_of_band_never_panic() {
    let mut rng = Rng::seeded(1);
    for _ in 0..rounds(20_000) {
        let mut data = PREFIXES[rng.below(PREFIXES.len())].to_vec();
        data.extend(rng.bytes(80));
        let _ = parse_from_client(&data);
        let _ = parse_challenge(&data, rng.next() as u32);
        let _ = parse_a2s_version(&data);
    }
}

#[test]
fn packets_of_a_connection_never_panic() {
    let mut rng = Rng::seeded(2);
    let mut rx = Receiver::new(Some(7));
    let now = Instant::now();
    let markers: [&[u8]; 4] = [
        b"",
        b"\xfe\xff\xff\xff",
        b"\xfd\xff\xff\xff",
        &[b"\xfd\xff\xff\xff".as_slice(), compress::SNAPPY_MAGIC].concat(),
    ];
    for _ in 0..rounds(20_000) {
        let mut data = markers[rng.below(markers.len())].to_vec();
        data.extend(rng.bytes(300));
        let _ = rx.feed(&data, now);
    }
}

/// Past the checksum and the challenge, into the reliable and unreliable parts.
#[test]
fn sealed_garbage_never_panics() {
    let mut rng = Rng::seeded(3);
    let mut rx = Receiver::new(Some(7));
    let now = Instant::now();
    for n in 1..=rounds(20_000) {
        let mut data = vec![0; 8];
        data[..4].copy_from_slice(&(n as i32).to_le_bytes());
        data.push(netchan::FLAG_CHALLENGE | (rng.next() as u8 & netchan::FLAG_RELIABLE));
        data.extend_from_slice(&[0, 0, 0]);
        data.extend_from_slice(&7u32.to_le_bytes());
        data.extend(rng.bytes(400));
        netchan::reseal(&mut data);
        let _ = rx.feed(&data, now);
    }
}

#[test]
fn captures_never_panic() {
    let mut rng = Rng::seeded(4);
    let real = std::fs::read(format!("{FIXTURES}/bots_cp_badlands.cap")).unwrap();
    let real = &real[..real.len().min(4096)];
    for _ in 0..rounds(5_000) {
        let mut data = real.to_vec();
        rng.mutate(&mut data);
        let _ = capture::read(&data);
        let mut junk = b"STVMAPC1".to_vec();
        junk.extend(rng.bytes(200));
        let _ = capture::read(&junk);
    }
}

#[test]
fn garbage_for_the_decoder_never_panics_the_process() {
    let tables = tables();
    let mut rng = Rng::seeded(5);
    for _ in 0..rounds(40) {
        let arena = Arena::new();
        let mut dec = Decoder::new(&arena, &tables, 20.0).unwrap();
        for _ in 0..50 {
            let data = rng.bytes(300);
            let _ = dec.feed(&data, data.len() * 8);
        }
    }
}

/// A corrupted stream through the whole session and decoder. The checksum is put right after
/// each change, or the receiver would drop the packet before anything interesting.
#[test]
fn corrupted_sessions_never_panic_the_process() {
    let tables = tables();
    let bytes = std::fs::read(format!("{FIXTURES}/bots_cp_badlands.cap")).unwrap();
    let records = capture::read(&bytes).unwrap();
    let mut sessions = records.iter().filter(|r| r.kind == capture::Kind::Session as u8);
    let challenge = u32::from_le_bytes(sessions.next().unwrap().data.try_into().unwrap());
    let datagrams: Vec<&[u8]> =
        records.iter().filter(|r| r.kind == capture::Kind::Datagram as u8).map(|r| r.data).take(500).collect();
    let opt = Options { snapshot_hz: 5.0, ..Options::default() };
    let (mut errors, mut snapshots) = (0, 0);
    for seed in 1..=rounds(8) {
        let mut rng = Rng::seeded(seed.wrapping_mul(0xD6E8_FEB8_6659_FD93));
        let every = 1 + rng.below(40);
        let mutated: Vec<Vec<u8>> = datagrams
            .iter()
            .map(|d| {
                let mut d = d.to_vec();
                if netchan::classify(&d) == Kind::InBand && rng.below(every) == 0 {
                    rng.mutate(&mut d);
                    netchan::reseal(&mut d);
                }
                d
            })
            .collect();
        let refs: Vec<&[u8]> = mutated.iter().map(Vec::as_slice).collect();
        session::replay(challenge, &refs, &tables, &opt, &mut |line| match line {
            Line::Status(Status::Disconnected { decode_errors, .. }) => errors += decode_errors,
            Line::Snapshot(_) => snapshots += 1,
            _ => {}
        })
        .unwrap();
    }
    assert!(errors > 0, "the corruption must reach the decoder");
    assert!(snapshots > 0, "and the stream must get far enough to decode entities");
}

#[test]
fn corrupted_demos_never_panic() {
    let real = std::fs::read(format!("{FIXTURES}/tc_hydro.dem")).unwrap();
    // Well past the header and data tables, into the packets.
    let real = &real[..real.len().min(1 << 20)];
    let mut rng = Rng::seeded(6);
    for _ in 0..rounds(4) {
        let mut data = real.to_vec();
        for _ in 0..=rng.below(30) {
            let at = rng.below(data.len());
            data[at] = rng.next() as u8;
        }
        let _ = decode::demo_file(&data, 1.0, &mut |_| {});
    }
}
