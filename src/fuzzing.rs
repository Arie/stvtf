//! Entry points for the fuzz targets in `fuzz/`, which `cargo fuzz` builds with `cfg(fuzzing)`.
//! The parsers are private, and in any other build this module does not exist.

use crate::capture;
use crate::decode::{Decoder, Tables};
use crate::net::bits::BitBuf;
use crate::net::handshake;
use crate::net::netchan::{self, FLAG_CHALLENGE, FLAG_RELIABLE, Kind};
use crate::net::receiver::Receiver;
use crate::relay;
use std::sync::LazyLock;
use std::time::Instant;
use typed_arena::Arena;

const CHALLENGE: u32 = 7;

/// Splits an input into pieces, each led by its length, so that one input can be many packets.
fn pieces(mut data: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let (len, rest) = data.split_first_chunk::<2>()?;
        let len = usize::from(u16::from_le_bytes(*len)).min(rest.len());
        let (piece, rest) = rest.split_at(len);
        data = rest;
        Some(piece)
    })
}

/// A channel packet around `body`, with a checksum and challenge that pass, numbered `sequence`.
fn sealed(challenge: u32, sequence: i32, reliable: bool, body: &[u8]) -> Vec<u8> {
    let mut p = sequence.to_le_bytes().to_vec();
    p.extend_from_slice(&[0; 4]);
    p.push(FLAG_CHALLENGE | if reliable { FLAG_RELIABLE } else { 0 });
    p.extend_from_slice(&[0; 3]);
    p.extend_from_slice(&challenge.to_le_bytes());
    p.extend_from_slice(body);
    netchan::reseal(&mut p);
    p
}

/// The packets a server's reply to a client can be: what a watch reads while connecting.
pub fn out_of_band(data: &[u8]) {
    let _ = handshake::parse_from_client(data);
    let _ = handshake::parse_challenge(data, 0);
    let _ = handshake::parse_a2s_version(data);
}

/// Any packets, raw and past the checksum: joining, inflating and reliable transfers.
pub fn channel(data: &[u8]) {
    let mut rx = Receiver::new(Some(CHALLENGE));
    let now = Instant::now();
    for (n, piece) in pieces(data).enumerate() {
        let n = n as i32 + 1;
        match piece.split_first() {
            Some((&kind, body)) if kind & 1 == 0 => drop(rx.feed(&sealed(CHALLENGE, n, kind & 2 != 0, body), now)),
            _ => drop(rx.feed(piece, now)),
        }
    }
}

/// What a client sends a relay in its channel.
pub fn client_messages(data: &[u8]) {
    let _ = relay::client_messages(&BitBuf::from_bytes(data.to_vec()));
}

/// Any packets at a relay with one client in, which is what anyone on the internet can do.
pub fn relay_hub(data: &[u8]) {
    let mut bench = relay::FuzzBench::new();
    for (n, piece) in pieces(data).enumerate() {
        match piece.split_first() {
            Some((&kind, body)) if kind & 1 == 0 => {
                bench.datagram(&sealed(bench.challenge(), n as i32 + 1, kind & 2 != 0, body));
            }
            _ => bench.datagram(piece),
        }
    }
    bench.service();
}

static TABLES: LazyLock<Tables> =
    LazyLock::new(|| Tables::from_demo_bytes(crate::builtin(crate::CURRENT_BUILD).unwrap().tables).unwrap());

/// Message streams into a new decoder, as they come out of a channel.
pub fn decoder(data: &[u8]) {
    let arena = Arena::new();
    let mut decoder = Decoder::new(&arena, &TABLES, 20.0).unwrap();
    for piece in pieces(data) {
        let _ = decoder.feed(piece, piece.len() * 8);
    }
}

/// Inputs for [`channel`] and [`decoder`] from a real capture: the fuzzer starts from traffic that
/// gets far, not from nothing. Each is `(channel, decoder)`.
pub fn seeds(capture_bytes: &[u8]) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let records = capture::read(capture_bytes).unwrap();
    let challenge = u32::from_le_bytes(records[0].data.try_into().unwrap());
    let mut rx = Receiver::new(Some(challenge));
    let now = Instant::now();
    let prefixed = |p: &[u8]| {
        let len = u16::try_from(p.len()).unwrap_or(u16::MAX);
        [&len.to_le_bytes(), &p[..usize::from(len)]].concat()
    };
    let (mut channel, mut streams) = (Vec::new(), Vec::new());
    for r in records.iter().skip(1).filter(|r| r.kind == capture::Kind::Datagram as u8).take(400) {
        if netchan::classify(r.data) != Kind::InBand {
            continue;
        }
        // Without the checksum and challenge, which `channel` makes up.
        let body = netchan::decode_header(r.data).map_or(16, |h| h.body_offset).min(r.data.len());
        let reliable = netchan::decode_header(r.data).is_ok_and(|h| h.reliable());
        channel.push(prefixed(&[&[if reliable { 2 } else { 0 }], &r.data[body..]].concat()));
        if let Some(got) = rx.feed(r.data, now) {
            for s in got.streams.iter().filter(|s| s.bits.nbits() >= 16) {
                streams.push(s.bits.bytes().to_vec());
            }
        }
    }
    // Packets in a row, for transfers of several.
    channel.push(channel.iter().take(30).flatten().copied().collect());
    // Every stream up to each point: entities need what came before them.
    let stream_seeds = (1..=streams.len())
        .step_by(20)
        .map(|n| streams[..n].iter().flat_map(|s| prefixed(s)).collect())
        .chain(streams.iter().map(|s| prefixed(s)))
        .collect();
    (channel, stream_seeds)
}
