//! Connectionless handshake and the `A2S_INFO` query used to learn the server build.
//!
//!   C->S 'q' getchallenge -> S->C 'A' challenge -> C->S 'k' connect -> S->C 'B' accepted
//!
//! Auth is the anonymous HASHEDCDKEY path, which `SourceTV` accepts without Steam.

use std::fmt;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{MAX_DATAGRAM, messages, netchan};
use crate::Result;
use crate::error::{Context, bail, invalid};

pub const PROTOCOL_VERSION: i32 = 24;
const AUTH_HASHEDCDKEY: u32 = 2;
const AUTH_STEAM: u32 = 3;
const MAGIC_VERSION: u32 = 0x5A4F_4933;
const OOB_PREFIX: [u8; 4] = [0xff; 4];
/// A connectionless packet's kind, right after the prefix.
const OOB_KIND: usize = OOB_PREFIX.len();
/// Connectionless packet kinds.
const C2S_GETCHALLENGE: u8 = b'q';
const C2S_CONNECT: u8 = b'k';
const S2C_CHALLENGE: u8 = b'A';
const S2C_CONNECTION: u8 = b'B';
const S2C_CONNREJECT: u8 = b'9';
const A2S_INFO: u8 = b'T';
const S2A_INFO: u8 = b'I';
/// The text of an `A2S_INFO` query.
const A2S_QUERY: &[u8; 20] = b"Source Engine Query\0";
/// Room for an `A2S_INFO` reply.
const A2S_REPLY: usize = 4096;
/// An `A2S_INFO` reply: the protocol version follows the kind, then name, map, folder and game.
const A2S_STRINGS: usize = OOB_KIND + 2;
/// After those strings: app id (u16), players, max players, bots, server type, environment,
/// visibility, VAC (a byte each), then the version string.
const A2S_FIXED: usize = 9;
/// An `A2S_INFO` that needs a challenge gets an 'A' with it (4 bytes) first; resend with it.
const A2S_CHALLENGE: std::ops::Range<usize> = OOB_KIND + 1..OOB_KIND + 5;
const A2S_TRIES: usize = 3;
/// The engine's padding after a getchallenge or an accept, and after a challenge.
const PADDING: &str = "0000000000";
const CHALLENGE_PADDING: &str = "000000";
/// Bind to any local address and port.
const ANY_ADDR: &str = "0.0.0.0:0";

/// Offsets in the 'A' challenge reply: magic, server challenge, our nonce echoed, auth protocol.
const CHALLENGE_MAGIC: usize = 5;
const CHALLENGE_SERVER: usize = 9;
const CHALLENGE_NONCE: usize = 13;
const CHALLENGE_AUTH: usize = 17;
const CHALLENGE_LEN: usize = 21;
/// Offset of our nonce echoed in a '9' refusal; the reason string follows it.
const REJECT_NONCE: usize = 5;
const REJECT_REASON: usize = 9;
/// Offsets in a 'q' getchallenge (the nonce) and a 'k' connect: protocol, auth protocol, the
/// server's challenge, the nonce, then name and password.
const GETCHALLENGE_NONCE: usize = 5;
const CONNECT_PROTOCOL: usize = 5;
const CONNECT_CHALLENGE: usize = 13;
const CONNECT_NONCE: usize = 17;
const CONNECT_NAME: usize = 21;

/// The server refused the connection ('9' reply); the reason is the server's text,
/// often a `#GameUI_ServerReject…` localization token.
#[derive(Debug)]
pub struct Rejected(pub String);

impl fmt::Display for Rejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "connect refused: {}", self.0)
    }
}

impl std::error::Error for Rejected {}

/// The address answered as a game server, not `SourceTV`.
#[derive(Debug)]
pub struct GamePort(pub SocketAddr);

impl fmt::Display for GamePort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} is a game port, not SourceTV: use the server's tv_port", self.0)
    }
}

impl std::error::Error for GamePort {}

#[derive(Debug)]
pub struct Connection {
    pub sock: UdpSocket,
    pub challenge: u32,
    /// In-band datagram that arrived instead of or before the 'B' accept.
    pub pending: Option<Vec<u8>>,
}

fn oob(cmd: u8) -> Vec<u8> {
    let mut v = OOB_PREFIX.to_vec();
    v.push(cmd);
    v.reserve(64);
    v
}

fn cstr(v: &mut Vec<u8>, s: &str) {
    v.extend_from_slice(s.as_bytes());
    v.push(0);
}

pub fn build_getchallenge(nonce: u32) -> Vec<u8> {
    let mut v = oob(C2S_GETCHALLENGE);
    v.extend_from_slice(&nonce.to_le_bytes());
    cstr(&mut v, PADDING);
    v
}

pub fn build_connect(nonce: u32, challenge: u32, name: &str, password: &str, build: &str) -> Vec<u8> {
    let mut v = oob(C2S_CONNECT);
    v.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    v.extend_from_slice(&AUTH_HASHEDCDKEY.to_le_bytes());
    v.extend_from_slice(&challenge.to_le_bytes());
    v.extend_from_slice(&nonce.to_le_bytes());
    cstr(&mut v, name);
    cstr(&mut v, password);
    cstr(&mut v, build);
    cstr(&mut v, &format!("{:x}", md5::compute(name.as_bytes())));
    v
}

fn le_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// The text up to the first NUL, or to the end if there is none.
fn until_nul(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn reject_reason(data: &[u8]) -> String {
    until_nul(data.get(REJECT_REASON..).unwrap_or_default())
}

#[derive(Debug, PartialEq)]
pub struct Challenge {
    pub challenge: u32,
    pub auth_proto: u32,
}

/// Parses the 'A' reply (or '9' refusal) to our getchallenge. Both echo our `nonce`; a
/// reply that does not is someone else's or forged, and gives `None`.
pub fn parse_challenge(data: &[u8], nonce: u32) -> Result<Option<Challenge>> {
    if data.len() <= OOB_KIND || data[..OOB_KIND] != OOB_PREFIX {
        bail!("cannot read a reply that is not connectionless");
    }
    match data[OOB_KIND] {
        S2C_CONNREJECT if le_u32(data, REJECT_NONCE) == Some(nonce) => return Err(Rejected(reject_reason(data)).into()),
        S2C_CONNREJECT => return Ok(None),
        S2C_CHALLENGE => {}
        c => bail!("cannot take reply {:?}", char::from(c)),
    }
    if data.len() < CHALLENGE_LEN {
        bail!("cannot read the challenge: reply cut short");
    }
    let word = |o: usize| le_u32(data, o).unwrap_or_default();
    if word(CHALLENGE_MAGIC) != MAGIC_VERSION {
        bail!("cannot take challenge magic {:#x}", word(CHALLENGE_MAGIC));
    }
    if word(CHALLENGE_NONCE) != nonce {
        return Ok(None);
    }
    Ok(Some(Challenge { challenge: word(CHALLENGE_SERVER), auth_proto: word(CHALLENGE_AUTH) }))
}

/// How often a wait for a server looks up from the socket.
const WAIT_POLL: Duration = Duration::from_millis(50);

/// What ends a wait for a server early: asked to stop, or out of time as a whole.
#[derive(Copy, Clone)]
pub struct Wait<'a> {
    pub stop: &'a AtomicBool,
    pub until: Option<Instant>,
}

impl Wait<'_> {
    pub fn over(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || self.until.is_some_and(|u| Instant::now() >= u)
    }
}

/// [`connect`] gave up because of its [`Wait`], which says nothing about the server.
#[derive(Debug)]
pub struct Stopped;

impl fmt::Display for Stopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("stopped")
    }
}

impl std::error::Error for Stopped {}

/// A read that found nothing: its timeout, or a signal (even with a handler that asks to restart,
/// a read with a timeout is interrupted). It says nothing about the server.
fn nothing_yet(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted)
}

pub fn connect(
    server: SocketAddr,
    name: &str,
    password: &str,
    build: &str,
    timeout: Duration,
    wait: Wait<'_>,
) -> Result<Connection> {
    let sock = UdpSocket::bind(ANY_ADDR)?;
    let nonce = random_u32();
    sock.send_to(&build_getchallenge(nonce), server)?;
    let mut buf = vec![0u8; MAX_DATAGRAM];
    let deadline = Instant::now() + timeout;
    // Replies are taken from any address (see `recv_from_port`), so only the nonce tells
    // the server's from a forgery: skip the ones without it, and those that do not parse, rather
    // than fail on them. The last that did not parse is named if no reply comes.
    let mut junk: Option<crate::Error> = None;
    let (ch, from) = loop {
        let got = recv_from_port(&sock, server.port(), &mut buf, deadline, wait);
        if wait.over() {
            return Err(Stopped.into());
        }
        let (n, from) = got.with_context(|| match &junk {
            Some(e) => format!("no challenge reply (ignored one: {e})"),
            None => "no challenge reply".to_string(),
        })?;
        match parse_challenge(&buf[..n], nonce) {
            Ok(Some(ch)) => break (ch, from),
            Ok(None) => {}
            Err(e) if e.rejected().is_some() => return Err(e),
            Err(e) => junk = Some(e),
        }
    };
    sock.connect(from)?;
    if ch.auth_proto == AUTH_STEAM {
        return Err(GamePort(server).into());
    }
    if ch.auth_proto != AUTH_HASHEDCDKEY {
        bail!("cannot use auth protocol {} (SourceTV takes {})", ch.auth_proto, AUTH_HASHEDCDKEY);
    }
    sock.send(&build_connect(nonce, ch.challenge, name, password, build))?;
    let deadline = Instant::now() + timeout;
    loop {
        // Wait only for what is left of the timeout, however many datagrams arrive.
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || wait.over() {
            break;
        }
        sock.set_read_timeout(Some(left.min(WAIT_POLL)))?;
        let n = match sock.recv(&mut buf) {
            Ok(n) => n,
            Err(e) if nothing_yet(&e) => continue,
            Err(_) => break,
        };
        let data = &buf[..n];
        if netchan::classify(data) == netchan::Kind::Oob && n > OOB_KIND {
            match data[OOB_KIND] {
                S2C_CONNECTION => return Ok(Connection { sock, challenge: ch.challenge, pending: None }),
                S2C_CONNREJECT => return Err(Rejected(reject_reason(data)).into()),
                _ => continue,
            }
        }
        // Too short for a netchan header otherwise.
        if n >= netchan::SEQUENCE_LEN {
            return Ok(Connection { sock, challenge: ch.challenge, pending: Some(data.to_vec()) });
        }
    }
    // The server may have opened a channel whose accept we missed; close it.
    let bye = messages::goodbye();
    let header = netchan::OutHeader { sequence: 1, ack: 0, challenge: ch.challenge, reliable_state: 0 };
    let pkt = netchan::build_packet(&header, None, Some(&bye));
    let _ = sock.send(&pkt);
    if wait.over() {
        return Err(Stopped.into());
    }
    bail!("cannot connect: no accept within {timeout:?}")
}

/// The next datagram from the server's port, on whichever address it answers from: a game
/// server on our own host, bound to 0.0.0.0 behind Docker's NAT, answers a container from the
/// bridge's gateway rather than the public address it was sent to.
///
/// Times out at `deadline`, however many datagrams from other ports arrive before it.
fn recv_from_port(
    sock: &UdpSocket,
    port: u16,
    buf: &mut [u8],
    deadline: Instant,
    wait: Wait<'_>,
) -> std::io::Result<(usize, SocketAddr)> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || wait.over() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        sock.set_read_timeout(Some(left.min(WAIT_POLL)))?;
        match sock.recv_from(buf) {
            Ok((n, from)) if from.port() == port => return Ok((n, from)),
            Ok(_) => {}
            Err(e) if nothing_yet(&e) => {}
            Err(e) => return Err(e),
        }
    }
}

/// An unpredictable nonce, so a spoofer cannot forge the challenge reply.
fn random_u32() -> u32 {
    super::random_u64() as u32
}

/// The server build (`A2S_INFO` `version`), following the challenge some servers require.
pub fn a2s_version(server: SocketAddr, timeout: Duration, wait: Wait<'_>) -> Result<String> {
    let sock = UdpSocket::bind(ANY_ADDR)?;
    let deadline = Instant::now() + timeout;
    let mut server = server;
    let mut req = OOB_PREFIX.to_vec();
    req.push(A2S_INFO);
    req.extend_from_slice(A2S_QUERY);
    let mut buf = vec![0u8; A2S_REPLY];
    let mut extra: Vec<u8> = Vec::new();
    for _ in 0..A2S_TRIES {
        let mut msg = req.clone();
        msg.extend_from_slice(&extra);
        sock.send_to(&msg, server)?;
        let n;
        (n, server) =
            recv_from_port(&sock, server.port(), &mut buf, deadline, wait).context("get an A2S_INFO reply")?;
        let data = &buf[..n];
        if n >= A2S_CHALLENGE.end && data[..OOB_KIND] == OOB_PREFIX && data[OOB_KIND] == S2C_CHALLENGE {
            extra = data[A2S_CHALLENGE].to_vec();
            continue;
        }
        return parse_a2s_version(data);
    }
    Err(invalid!("cannot get past A2S_INFO's challenges"))
}

pub(crate) fn parse_a2s_version(data: &[u8]) -> Result<String> {
    if data.len() < A2S_STRINGS || data[..OOB_KIND] != OOB_PREFIX || data[OOB_KIND] != S2A_INFO {
        bail!("cannot read a reply that is not A2S_INFO");
    }
    let mut pos = A2S_STRINGS;
    // Skip name, map, folder and game.
    for _ in 0..4 {
        read_cstr(data, &mut pos).ok_or_else(|| invalid!("cannot read A2S_INFO: reply cut short"))?;
    }
    let Some(tail) = data.get(pos + A2S_FIXED..) else { bail!("cannot read A2S_INFO: reply cut short") };
    Ok(until_nul(tail))
}

/// A connectionless packet a client sends a server.
#[derive(Debug, PartialEq)]
pub enum FromClient {
    GetChallenge {
        nonce: u32,
    },
    Connect {
        protocol: i32,
        challenge: u32,
        nonce: u32,
        name: String,
        password: String,
    },
    /// `A2S_INFO`, with the challenge of an earlier reply if it has one.
    Info {
        challenge: Option<u32>,
    },
}

/// Reads one NUL-terminated string at `*pos`, moving past it.
fn read_cstr(data: &[u8], pos: &mut usize) -> Option<String> {
    let rest = data.get(*pos..)?;
    *pos += rest.iter().position(|&b| b == 0)? + 1;
    Some(until_nul(rest))
}

/// Parses what a client sends a server out of band; None for anything else.
pub fn parse_from_client(data: &[u8]) -> Option<FromClient> {
    if data.len() <= OOB_KIND || data[..OOB_KIND] != OOB_PREFIX {
        return None;
    }
    match data[OOB_KIND] {
        C2S_GETCHALLENGE => Some(FromClient::GetChallenge { nonce: le_u32(data, GETCHALLENGE_NONCE)? }),
        C2S_CONNECT => {
            let mut pos = CONNECT_NAME;
            Some(FromClient::Connect {
                protocol: le_u32(data, CONNECT_PROTOCOL)? as i32,
                challenge: le_u32(data, CONNECT_CHALLENGE)?,
                nonce: le_u32(data, CONNECT_NONCE)?,
                name: read_cstr(data, &mut pos)?,
                password: read_cstr(data, &mut pos)?,
            })
        }
        A2S_INFO => {
            let query = data.get(OOB_KIND + 1..)?;
            let rest = query.strip_prefix(A2S_QUERY.as_slice())?;
            Some(FromClient::Info { challenge: le_u32(rest, 0) })
        }
        _ => None,
    }
}

/// The 'A' reply to a getchallenge, offering the auth protocol `SourceTV` takes.
pub fn build_challenge(challenge: u32, nonce: u32) -> Vec<u8> {
    let mut v = oob(S2C_CHALLENGE);
    v.extend_from_slice(&MAGIC_VERSION.to_le_bytes());
    v.extend_from_slice(&challenge.to_le_bytes());
    v.extend_from_slice(&nonce.to_le_bytes());
    v.extend_from_slice(&AUTH_HASHEDCDKEY.to_le_bytes());
    cstr(&mut v, CHALLENGE_PADDING);
    v
}

/// The 'B' accept of a connect; the client checks it echoes its nonce.
pub fn build_accept(nonce: u32) -> Vec<u8> {
    let mut v = oob(S2C_CONNECTION);
    v.extend_from_slice(&nonce.to_le_bytes());
    cstr(&mut v, PADDING);
    v
}

/// A '9' refusal.
pub fn build_reject(nonce: u32, reason: &str) -> Vec<u8> {
    let mut v = oob(S2C_CONNREJECT);
    v.extend_from_slice(&nonce.to_le_bytes());
    cstr(&mut v, reason);
    v
}

/// The challenge an `A2S_INFO` must come back with: shorter than the query, so a forged source
/// address gets nothing larger than it sent.
pub fn build_info_challenge(challenge: u32) -> Vec<u8> {
    let mut v = oob(S2C_CHALLENGE);
    v.extend_from_slice(&challenge.to_le_bytes());
    v
}

/// What an `A2S_INFO` reply says about a server.
#[derive(Debug)]
pub struct Info<'a> {
    pub name: &'a str,
    pub map: &'a str,
    pub players: u8,
    pub max_players: u8,
    pub build: &'a str,
}

/// The `A2S_INFO` reply of a `SourceTV` server ('p' for proxy).
pub fn build_info(info: &Info<'_>) -> Vec<u8> {
    const A2S_PROTOCOL: u8 = 17;
    const TF2_APP_ID: u16 = 440;
    let mut v = oob(S2A_INFO);
    v.push(A2S_PROTOCOL);
    for s in [info.name, info.map, "tf", "Team Fortress"] {
        cstr(&mut v, s);
    }
    v.extend_from_slice(&TF2_APP_ID.to_le_bytes());
    // No bots, a proxy on Linux, public, no VAC.
    v.extend_from_slice(&[info.players, info.max_players, 0, b'p', b'l', 0, 0]);
    cstr(&mut v, info.build);
    v
}

#[cfg(test)]
mod tests;
