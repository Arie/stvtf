//! Connectionless handshake and the A2S_INFO query used to learn the server build.
//!
//!   C->S 'q' getchallenge -> S->C 'A' challenge -> C->S 'k' connect -> S->C 'B' accepted
//!
//! Auth is the anonymous HASHEDCDKEY path, which SourceTV accepts without Steam.

use super::{messages, netchan};
use anyhow::{Context, Result, anyhow, bail};
use std::fmt;
use std::hash::{BuildHasher, RandomState};
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

const PROTOCOL_VERSION: i32 = 24;
const AUTH_HASHEDCDKEY: u32 = 2;
const MAGIC_VERSION: u32 = 0x5A4F_4933;
const OOB_PREFIX: [u8; 4] = [0xff; 4];

/// Offsets in the 'A' challenge reply: magic, server challenge, our nonce echoed, auth protocol.
const CHALLENGE_MAGIC: usize = 5;
const CHALLENGE_SERVER: usize = 9;
const CHALLENGE_NONCE: usize = 13;
const CHALLENGE_AUTH: usize = 17;
const CHALLENGE_LEN: usize = 21;

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
    let mut v = oob(b'q');
    v.extend_from_slice(&nonce.to_le_bytes());
    cstr(&mut v, "0000000000");
    v
}

pub fn build_connect(nonce: u32, challenge: u32, name: &str, password: &str, build: &str) -> Vec<u8> {
    let mut v = oob(b'k');
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

fn reject_reason(data: &[u8]) -> String {
    let tail = data.get(9..).unwrap_or_default();
    let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    String::from_utf8_lossy(&tail[..end]).into_owned()
}

#[derive(Debug, PartialEq)]
pub struct Challenge {
    pub challenge: u32,
    pub auth_proto: u32,
}

/// Parses the 'A' reply to our getchallenge, which must echo our `nonce`.
pub fn parse_challenge(data: &[u8], nonce: u32) -> Result<Challenge> {
    if data.len() < 5 || data[..4] != OOB_PREFIX {
        bail!("not a connectionless reply");
    }
    match data[4] {
        b'9' => return Err(Rejected(reject_reason(data)).into()),
        b'A' => {}
        c => bail!("unexpected reply {:?}", char::from(c)),
    }
    if data.len() < CHALLENGE_LEN {
        bail!("truncated challenge");
    }
    let word = |o: usize| u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
    if word(CHALLENGE_MAGIC) != MAGIC_VERSION {
        bail!("bad challenge magic {:#x}", word(CHALLENGE_MAGIC));
    }
    if word(CHALLENGE_NONCE) != nonce {
        bail!("challenge reply for another client");
    }
    Ok(Challenge { challenge: word(CHALLENGE_SERVER), auth_proto: word(CHALLENGE_AUTH) })
}

pub fn connect(server: SocketAddr, name: &str, password: &str, build: &str, timeout: Duration) -> Result<Connection> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.connect(server)?;
    sock.set_read_timeout(Some(timeout))?;
    let nonce = random_u32();
    sock.send(&build_getchallenge(nonce))?;
    let mut buf = vec![0u8; 65536];
    let n = sock.recv(&mut buf).context("no challenge reply")?;
    let ch = parse_challenge(&buf[..n], nonce)?;
    if ch.auth_proto != AUTH_HASHEDCDKEY {
        bail!("server wants auth protocol {} (SourceTV accepts {})", ch.auth_proto, AUTH_HASHEDCDKEY);
    }
    sock.send(&build_connect(nonce, ch.challenge, name, password, build))?;
    let deadline = Instant::now() + timeout;
    loop {
        // Wait only for what is left of the timeout, however many datagrams arrive.
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        sock.set_read_timeout(Some(left))?;
        let Ok(n) = sock.recv(&mut buf) else { break };
        let data = &buf[..n];
        if netchan::classify(data) == netchan::Kind::Oob && n > 4 {
            match data[4] {
                b'B' => return Ok(Connection { sock, challenge: ch.challenge, pending: None }),
                b'9' => return Err(Rejected(reject_reason(data)).into()),
                _ => continue,
            }
        }
        if n >= 4 {
            return Ok(Connection { sock, challenge: ch.challenge, pending: Some(data.to_vec()) });
        }
    }
    // The server may have opened a channel whose accept we missed; close it.
    let bye = messages::disconnect("Disconnect by user.");
    let pkt = netchan::build_packet(1, 0, ch.challenge, 0, None, Some((&bye.0, bye.1)));
    let _ = sock.send(&pkt);
    bail!("no accept within {timeout:?}")
}

/// An unpredictable nonce, so a spoofer cannot forge the challenge reply. `RandomState`
/// is keyed from the OS random source; no extra dependency needed.
fn random_u32() -> u32 {
    RandomState::new().hash_one(Instant::now()) as u32
}

#[derive(Debug, Default, Clone)]
pub struct ServerInfo {
    pub name: String,
    pub map: String,
    pub players: u8,
    pub max_players: u8,
    pub version: String,
    pub password: bool,
}

/// A2S_INFO, following the challenge some servers require.
pub fn a2s_info(server: SocketAddr, timeout: Duration) -> Result<ServerInfo> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.connect(server)?;
    sock.set_read_timeout(Some(timeout))?;
    let mut req = OOB_PREFIX.to_vec();
    req.push(b'T');
    req.extend_from_slice(b"Source Engine Query\0");
    let mut buf = vec![0u8; 4096];
    let mut extra: Vec<u8> = Vec::new();
    for _ in 0..3 {
        let mut msg = req.clone();
        msg.extend_from_slice(&extra);
        sock.send(&msg)?;
        let n = sock.recv(&mut buf).context("no A2S_INFO reply")?;
        let data = &buf[..n];
        if n >= 9 && data[..4] == OOB_PREFIX && data[4] == b'A' {
            extra = data[5..9].to_vec();
            continue;
        }
        return parse_a2s_info(data);
    }
    Err(anyhow!("A2S_INFO challenge loop"))
}

pub fn parse_a2s_info(data: &[u8]) -> Result<ServerInfo> {
    if data.len() < 6 || data[..4] != OOB_PREFIX || data[4] != b'I' {
        bail!("not an A2S_INFO reply");
    }
    let mut pos = 6;
    let mut s = || -> Result<String> {
        let rest = data.get(pos..).ok_or_else(|| anyhow!("truncated"))?;
        let end = rest.iter().position(|&b| b == 0).ok_or_else(|| anyhow!("unterminated string"))?;
        pos += end + 1;
        Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
    };
    let name = s()?;
    let map = s()?;
    let _folder = s()?;
    let _game = s()?;
    let rest = data.get(pos..).ok_or_else(|| anyhow!("truncated"))?;
    if rest.len() < 9 {
        bail!("truncated A2S_INFO");
    }
    let players = rest[2];
    let max_players = rest[3];
    let password = rest[7] != 0;
    let tail = &rest[9..];
    let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    let version = String::from_utf8_lossy(&tail[..end]).into_owned();
    Ok(ServerInfo { name, map, players, max_players, version, password })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_packet_layout() {
        let p = build_connect(1, 2, "stvmap", "tv", "11138802");
        assert_eq!(&p[..5], &[0xff, 0xff, 0xff, 0xff, b'k']);
        assert_eq!(i32::from_le_bytes(p[5..9].try_into().unwrap()), 24);
        assert_eq!(u32::from_le_bytes(p[9..13].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(p[13..17].try_into().unwrap()), 2);
        let strings: Vec<&[u8]> = p[21..].split(|&b| b == 0).collect();
        assert_eq!(strings[0], b"stvmap");
        assert_eq!(strings[1], b"tv");
        assert_eq!(strings[2], b"11138802");
        assert_eq!(strings[3].len(), 32);
    }

    #[test]
    fn challenge_parse_and_reject() {
        let mut ok = vec![0xff, 0xff, 0xff, 0xff, b'A'];
        ok.extend_from_slice(&MAGIC_VERSION.to_le_bytes());
        ok.extend_from_slice(&77u32.to_le_bytes());
        ok.extend_from_slice(&5u32.to_le_bytes());
        ok.extend_from_slice(&2u32.to_le_bytes());
        assert_eq!(parse_challenge(&ok, 5).unwrap(), Challenge { challenge: 77, auth_proto: 2 });
        assert!(parse_challenge(&ok, 6).is_err(), "reply echoing another nonce");
        let mut rej = vec![0xff, 0xff, 0xff, 0xff, b'9', 0, 0, 0, 0];
        rej.extend_from_slice(b"Bad password\0");
        let err = parse_challenge(&rej, 5).unwrap_err();
        assert_eq!(err.downcast_ref::<Rejected>().map(|r| r.0.as_str()), Some("Bad password"));
    }

    #[test]
    fn nonces_differ() {
        assert_ne!(random_u32(), random_u32());
    }

    #[test]
    fn a2s_info_requires_connectionless_prefix() {
        assert!(parse_a2s_info(b"\xfe\xff\xff\xffI\x11rest").is_err());
    }

    #[test]
    fn a2s_info_parse() {
        let mut d = vec![0xff, 0xff, 0xff, 0xff, b'I', 17];
        for s in ["SourceTV", "cp_badlands", "tf", "Team Fortress"] {
            d.extend_from_slice(s.as_bytes());
            d.push(0);
        }
        d.extend_from_slice(&440u16.to_le_bytes());
        d.extend_from_slice(&[3, 32, 0, b'p', b'l', 1, 0]);
        d.extend_from_slice(b"11138802\0");
        let i = parse_a2s_info(&d).unwrap();
        assert_eq!(i.map, "cp_badlands");
        assert_eq!(i.players, 3);
        assert_eq!(i.max_players, 32);
        assert!(i.password);
        assert_eq!(i.version, "11138802");
    }
}
