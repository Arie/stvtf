//! Connectionless handshake and the `A2S_INFO` query used to learn the server build.
//!
//!   C->S 'q' getchallenge -> S->C 'A' challenge -> C->S 'k' connect -> S->C 'B' accepted
//!
//! Auth is the anonymous HASHEDCDKEY path, which `SourceTV` accepts without Steam.

use super::{MAX_DATAGRAM, messages, netchan};
use anyhow::{Context, Result, anyhow, bail};
use std::fmt;

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

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
    cstr(&mut v, "0000000000");
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

fn reject_reason(data: &[u8]) -> String {
    let tail = data.get(REJECT_REASON..).unwrap_or_default();
    let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    String::from_utf8_lossy(&tail[..end]).into_owned()
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
        bail!("not a connectionless reply");
    }
    let word = |o: usize| data.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    match data[OOB_KIND] {
        S2C_CONNREJECT if word(REJECT_NONCE) == Some(nonce) => return Err(Rejected(reject_reason(data)).into()),
        S2C_CONNREJECT => return Ok(None),
        S2C_CHALLENGE => {}
        c => bail!("unexpected reply {:?}", char::from(c)),
    }
    if data.len() < CHALLENGE_LEN {
        bail!("truncated challenge");
    }
    let word = |o: usize| word(o).unwrap_or_default();
    if word(CHALLENGE_MAGIC) != MAGIC_VERSION {
        bail!("bad challenge magic {:#x}", word(CHALLENGE_MAGIC));
    }
    if word(CHALLENGE_NONCE) != nonce {
        return Ok(None);
    }
    Ok(Some(Challenge { challenge: word(CHALLENGE_SERVER), auth_proto: word(CHALLENGE_AUTH) }))
}

pub fn connect(server: SocketAddr, name: &str, password: &str, build: &str, timeout: Duration) -> Result<Connection> {
    let sock = UdpSocket::bind(ANY_ADDR)?;
    let nonce = random_u32();
    sock.send_to(&build_getchallenge(nonce), server)?;
    let mut buf = vec![0u8; MAX_DATAGRAM];
    let deadline = Instant::now() + timeout;
    // Replies are taken from any address (see `recv_from_port`), so only the nonce tells
    // the server's from a forgery: skip the ones without it, and those that do not parse, rather
    // than fail on them. The last that did not parse is named if no reply comes.
    let mut junk: Option<anyhow::Error> = None;
    let (ch, from) = loop {
        let (n, from) = recv_from_port(&sock, server.port(), &mut buf, deadline).with_context(|| match &junk {
            Some(e) => format!("no challenge reply (ignored one: {e})"),
            None => "no challenge reply".to_string(),
        })?;
        match parse_challenge(&buf[..n], nonce) {
            Ok(Some(ch)) => break (ch, from),
            Ok(None) => {}
            Err(e) if e.is::<Rejected>() => return Err(e),
            Err(e) => junk = Some(e),
        }
    };
    sock.connect(from)?;
    if ch.auth_proto == AUTH_STEAM {
        return Err(GamePort(server).into());
    }
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
    bail!("no accept within {timeout:?}")
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
) -> std::io::Result<(usize, SocketAddr)> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        sock.set_read_timeout(Some(left))?;
        let (n, from) = sock.recv_from(buf)?;
        if from.port() == port {
            return Ok((n, from));
        }
    }
}

/// An unpredictable nonce, so a spoofer cannot forge the challenge reply.
fn random_u32() -> u32 {
    super::random_u64() as u32
}

/// The server build (`A2S_INFO` `version`), following the challenge some servers require.
pub fn a2s_version(server: SocketAddr, timeout: Duration) -> Result<String> {
    let sock = UdpSocket::bind(ANY_ADDR)?;
    let deadline = Instant::now() + timeout;
    let mut server = server;
    let mut req = OOB_PREFIX.to_vec();
    req.push(A2S_INFO);
    req.extend_from_slice(b"Source Engine Query\0");
    let mut buf = vec![0u8; A2S_REPLY];
    let mut extra: Vec<u8> = Vec::new();
    for _ in 0..A2S_TRIES {
        let mut msg = req.clone();
        msg.extend_from_slice(&extra);
        sock.send_to(&msg, server)?;
        let n;
        (n, server) = recv_from_port(&sock, server.port(), &mut buf, deadline).context("no A2S_INFO reply")?;
        let data = &buf[..n];
        if n >= A2S_CHALLENGE.end && data[..OOB_KIND] == OOB_PREFIX && data[OOB_KIND] == S2C_CHALLENGE {
            extra = data[A2S_CHALLENGE].to_vec();
            continue;
        }
        return parse_a2s_version(data);
    }
    Err(anyhow!("A2S_INFO challenge loop"))
}

fn parse_a2s_version(data: &[u8]) -> Result<String> {
    if data.len() < A2S_STRINGS || data[..OOB_KIND] != OOB_PREFIX || data[OOB_KIND] != S2A_INFO {
        bail!("not an A2S_INFO reply");
    }
    let mut pos = A2S_STRINGS;
    // Skip name, map, folder and game.
    for _ in 0..4 {
        let rest = data.get(pos..).ok_or_else(|| anyhow!("truncated"))?;
        let end = rest.iter().position(|&b| b == 0).ok_or_else(|| anyhow!("unterminated string"))?;
        pos += end + 1;
    }
    let Some(tail) = data.get(pos + A2S_FIXED..) else { bail!("truncated A2S_INFO") };
    let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    Ok(String::from_utf8_lossy(&tail[..end]).into_owned())
}

/// A connectionless packet a client sends a server.
#[derive(Debug, PartialEq)]
pub enum FromClient {
    GetChallenge { nonce: u32 },
    Connect { protocol: i32, challenge: u32, nonce: u32, name: String, password: String },
    Info,
}

/// Reads one NUL-terminated string at `*pos`, moving past it.
fn read_cstr(data: &[u8], pos: &mut usize) -> Option<String> {
    let rest = data.get(*pos..)?;
    let end = rest.iter().position(|&b| b == 0)?;
    *pos += end + 1;
    Some(String::from_utf8_lossy(&rest[..end]).into_owned())
}

/// Parses what a client sends a server out of band; None for anything else.
pub fn parse_from_client(data: &[u8]) -> Option<FromClient> {
    if data.len() <= OOB_KIND || data[..OOB_KIND] != OOB_PREFIX {
        return None;
    }
    let word = |o: usize| data.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    match data[OOB_KIND] {
        C2S_GETCHALLENGE => Some(FromClient::GetChallenge { nonce: word(OOB_KIND + 1)? }),
        C2S_CONNECT => {
            // Protocol, auth protocol, our challenge, the client's nonce, then name and password.
            let mut pos = OOB_KIND + 17;
            Some(FromClient::Connect {
                protocol: word(OOB_KIND + 1)? as i32,
                challenge: word(OOB_KIND + 9)?,
                nonce: word(OOB_KIND + 13)?,
                name: read_cstr(data, &mut pos)?,
                password: read_cstr(data, &mut pos)?,
            })
        }
        A2S_INFO => Some(FromClient::Info),
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
    cstr(&mut v, "000000");
    v
}

/// The 'B' accept of a connect; the client checks it echoes its nonce.
pub fn build_accept(nonce: u32) -> Vec<u8> {
    let mut v = oob(S2C_CONNECTION);
    v.extend_from_slice(&nonce.to_le_bytes());
    cstr(&mut v, "0000000000");
    v
}

/// A '9' refusal.
pub fn build_reject(nonce: u32, reason: &str) -> Vec<u8> {
    let mut v = oob(S2C_CONNREJECT);
    v.extend_from_slice(&nonce.to_le_bytes());
    cstr(&mut v, reason);
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
    v.extend_from_slice(&[info.players, info.max_players, 0, b'p', b'l', 0, 0]);
    cstr(&mut v, info.build);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_side_handshake_meets_the_client() {
        let nonce = 0x1234_5678;
        assert_eq!(parse_from_client(&build_getchallenge(nonce)), Some(FromClient::GetChallenge { nonce }));
        let ch = parse_challenge(&build_challenge(99, nonce), nonce).unwrap().unwrap();
        assert_eq!(ch, Challenge { challenge: 99, auth_proto: AUTH_HASHEDCDKEY });
        let connect = parse_from_client(&build_connect(nonce, 99, "me", "pw", "1")).unwrap();
        assert_eq!(
            connect,
            FromClient::Connect {
                protocol: PROTOCOL_VERSION,
                challenge: 99,
                nonce,
                name: "me".into(),
                password: "pw".into()
            }
        );
        let err = parse_challenge(&build_reject(nonce, "Bad password"), nonce).unwrap_err();
        assert!(err.is::<Rejected>());
        let info = Info { name: "relay", map: "cp_badlands", players: 1, max_players: 8, build: "11138802" };
        assert_eq!(parse_a2s_version(&build_info(&info)).unwrap(), "11138802");
    }

    #[test]
    fn connect_packet_layout() {
        let p = build_connect(1, 2, "stv.tf", "tv", "11138802");
        assert_eq!(&p[..5], &[0xff, 0xff, 0xff, 0xff, b'k']);
        assert_eq!(i32::from_le_bytes(p[5..9].try_into().unwrap()), 24);
        assert_eq!(u32::from_le_bytes(p[9..13].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(p[13..17].try_into().unwrap()), 2);
        let strings: Vec<&[u8]> = p[21..].split(|&b| b == 0).collect();
        assert_eq!(strings[0], b"stv.tf");
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
        assert_eq!(parse_challenge(&ok, 5).unwrap(), Some(Challenge { challenge: 77, auth_proto: 2 }));
        assert_eq!(parse_challenge(&ok, 6).unwrap(), None, "reply echoing another nonce");
        let mut rej = vec![0xff, 0xff, 0xff, 0xff, b'9'];
        rej.extend_from_slice(&5u32.to_le_bytes());
        rej.extend_from_slice(b"Bad password\0");
        let err = parse_challenge(&rej, 5).unwrap_err();
        assert_eq!(err.downcast_ref::<Rejected>().map(|r| r.0.as_str()), Some("Bad password"));
    }

    #[test]
    fn refusal_without_our_nonce_is_ignored() {
        // Anyone can send from the server's port: a forged "Bad password" would end the watch.
        let mut forged = vec![0xff, 0xff, 0xff, 0xff, b'9'];
        forged.extend_from_slice(&6u32.to_le_bytes());
        forged.extend_from_slice(b"Bad password\0");
        assert_eq!(parse_challenge(&forged, 5).unwrap(), None);
        assert_eq!(parse_challenge(&[0xff, 0xff, 0xff, 0xff, b'9', 5, 0], 5).unwrap(), None, "truncated");
    }

    #[test]
    fn junk_from_other_ports_does_not_hold_off_the_timeout() {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let to = sock.local_addr().unwrap();
        let spam = std::thread::spawn(move || {
            let junk = UdpSocket::bind("127.0.0.1:0").unwrap();
            let until = Instant::now() + Duration::from_secs(1);
            while Instant::now() < until {
                let _ = junk.send_to(b"junk", to);
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let start = Instant::now();
        let mut buf = [0u8; 64];
        // Port 9 is not the spammer's, so nothing it sends is accepted.
        let got = recv_from_port(&sock, 9, &mut buf, start + Duration::from_millis(200));
        let kind = got.unwrap_err().kind();
        assert!(matches!(kind, std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock), "{kind:?}");
        assert!(start.elapsed() < Duration::from_secs(1), "{:?}", start.elapsed());
        spam.join().unwrap();
    }

    #[test]
    fn junk_from_the_servers_port_is_skipped() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let fake = std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let (_, client) = server.recv_from(&mut buf).unwrap();
            let nonce = &buf[OOB_KIND + 1..OOB_KIND + 5];
            server.send_to(b"\xff\xff\xff\xffZjunk", client).unwrap();
            let mut reply = vec![0xff, 0xff, 0xff, 0xff, b'A'];
            reply.extend_from_slice(&MAGIC_VERSION.to_le_bytes());
            reply.extend_from_slice(&77u32.to_le_bytes());
            reply.extend_from_slice(nonce);
            reply.extend_from_slice(&AUTH_STEAM.to_le_bytes());
            server.send_to(&reply, client).unwrap();
        });
        // A game server's challenge ends the attempt as soon as it is parsed.
        let err = connect(addr, "stv.tf", "", "1", Duration::from_secs(2)).err().unwrap();
        assert!(err.is::<GamePort>(), "{err:#}");
        fake.join().unwrap();
    }

    #[test]
    fn nonces_differ() {
        assert_ne!(random_u32(), random_u32());
    }

    #[test]
    fn a2s_info_requires_connectionless_prefix() {
        assert!(parse_a2s_version(b"\xfe\xff\xff\xffI\x11rest").is_err());
    }

    #[test]
    fn a2s_version_parse() {
        let mut d = vec![0xff, 0xff, 0xff, 0xff, b'I', 17];
        for s in ["SourceTV", "cp_badlands", "tf", "Team Fortress"] {
            d.extend_from_slice(s.as_bytes());
            d.push(0);
        }
        d.extend_from_slice(&440u16.to_le_bytes());
        d.extend_from_slice(&[3, 32, 0, b'p', b'l', 1, 0]);
        d.extend_from_slice(b"11138802\0");
        assert_eq!(parse_a2s_version(&d).unwrap(), "11138802");
        assert!(parse_a2s_version(&d[..d.len() - 15]).is_err(), "truncated");
    }
}
