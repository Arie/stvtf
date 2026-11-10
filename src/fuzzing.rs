//! Entry points for the fuzz targets in `fuzz/`, which `cargo fuzz` builds with `cfg(fuzzing)`.
//! The parsers are private, and in any other build this module does not exist.

use crate::capture;
use crate::decode::{Decoder, Tables};
use crate::net::bits::BitBuf;
use crate::net::handshake;
use crate::net::netchan::{self, FLAG_CHALLENGE, FLAG_RELIABLE, Kind};
use crate::net::receiver::Receiver;
use crate::relay;
use std::borrow::Cow;
use std::sync::LazyLock;
use std::time::Instant;

const CHALLENGE: u32 = 7;
/// In a piece's first byte: the piece goes as it is, not sealed around the rest.
const RAW: u8 = 1;
/// In a piece's first byte: the sealed packet is reliable.
const RELIABLE: u8 = 2;
/// The header [`sealed`] puts before the body: up to `reliable_state`, then the challenge.
const SEALED_HEADER: usize = netchan::HEADER_MIN + size_of::<u32>();

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

/// The packet a piece stands for: itself, or the rest of it sealed as packet `sequence`.
fn packet(challenge: u32, sequence: i32, piece: &[u8]) -> Cow<'_, [u8]> {
    match piece.split_first() {
        Some((&kind, body)) if kind & RAW == 0 => Cow::Owned(sealed(challenge, sequence, kind & RELIABLE != 0, body)),
        _ => Cow::Borrowed(piece),
    }
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
        let _ = rx.feed(&packet(CHALLENGE, n as i32 + 1, piece), now);
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
        bench.datagram(&packet(bench.challenge(), n as i32 + 1, piece));
    }
    bench.service();
}

static TABLES: LazyLock<Tables> = LazyLock::new(|| crate::watch::tables(crate::CURRENT_BUILD, None).unwrap());

/// Message streams into a new decoder, as they come out of a channel.
pub fn decoder(data: &[u8]) {
    let mut decoder = Decoder::new(&TABLES, crate::DEFAULT_SNAPSHOT_HZ).unwrap();
    for piece in pieces(data) {
        let _ = decoder.feed(piece, piece.len() * 8);
    }
}

/// Inputs for [`channel`] and [`decoder`] from a real capture: the fuzzer starts from traffic that
/// gets far, not from nothing. Each is `(channel, decoder)`.
pub fn seeds(capture_bytes: &[u8]) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let records = capture::read(capture_bytes).unwrap();
    let challenge = records[0].challenge().unwrap();
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
        let body = netchan::decode_header(r.data).map_or(SEALED_HEADER, |h| h.body_offset).min(r.data.len());
        let reliable = netchan::decode_header(r.data).is_ok_and(|h| h.reliable());
        channel.push(prefixed(&[&[if reliable { RELIABLE } else { 0 }], &r.data[body..]].concat()));
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
