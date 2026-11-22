use super::*;

/// A wait only its own timeout ends.
fn no_end() -> Wait<'static> {
    static NEVER: AtomicBool = AtomicBool::new(false);
    Wait { stop: &NEVER, until: None }
}

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
    assert!(err.rejected().is_some());
    let info = Info { name: "relay", map: "cp_badlands", players: 1, max_players: 8, build: "11138802" };
    assert_eq!(parse_a2s_version(&build_info(&info)).unwrap(), "11138802");
    let mut query = oob(A2S_INFO);
    query.extend_from_slice(A2S_QUERY);
    assert_eq!(parse_from_client(&query), Some(FromClient::Info { challenge: None }));
    let reply = build_info_challenge(77);
    assert!(reply.len() < query.len(), "the challenge is no larger than the query");
    query.extend_from_slice(&reply[A2S_CHALLENGE]);
    assert_eq!(parse_from_client(&query), Some(FromClient::Info { challenge: Some(77) }));
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
    assert_eq!(err.rejected().map(|r| r.0.as_str()), Some("Bad password"));
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
    let done = std::sync::Arc::new(AtomicBool::new(false));
    let spam_done = done.clone();
    let spam = std::thread::spawn(move || {
        let junk = UdpSocket::bind("127.0.0.1:0").unwrap();
        let until = Instant::now() + Duration::from_secs(1);
        while Instant::now() < until && !spam_done.load(Ordering::Relaxed) {
            let _ = junk.send_to(b"junk", to);
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let start = Instant::now();
    let mut buf = [0u8; 64];
    // Port 9 is not the spammer's, so nothing it sends is accepted.
    let got = recv_from_port(&sock, 9, &mut buf, start + Duration::from_millis(200), no_end());
    let elapsed = start.elapsed();
    done.store(true, Ordering::Relaxed);
    spam.join().unwrap();
    let kind = got.unwrap_err().kind();
    assert!(matches!(kind, std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock), "{kind:?}");
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
}

#[test]
fn junk_from_the_servers_port_is_skipped() {
    let server = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let fake = std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        let (_, client) = server.recv_from(&mut buf).unwrap();
        let nonce = &buf[GETCHALLENGE_NONCE..GETCHALLENGE_NONCE + 4];
        server.send_to(b"\xff\xff\xff\xffZjunk", client).unwrap();
        let mut reply = vec![0xff, 0xff, 0xff, 0xff, b'A'];
        reply.extend_from_slice(&MAGIC_VERSION.to_le_bytes());
        reply.extend_from_slice(&77u32.to_le_bytes());
        reply.extend_from_slice(nonce);
        reply.extend_from_slice(&AUTH_STEAM.to_le_bytes());
        server.send_to(&reply, client).unwrap();
    });
    // A game server's challenge ends the attempt as soon as it is parsed.
    let err = connect(addr, "stv.tf", "", "1", Duration::from_secs(2), no_end()).err().unwrap();
    assert!(err.game_port().is_some(), "{err:#}");
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

#[test]
fn replies_that_are_no_challenge_are_errors() {
    let nonce = 5;
    assert!(parse_challenge(b"\x01\x02\x03\x04\x05", nonce).is_err());
    assert!(parse_challenge(b"\xff\xff\xff\xffZ", nonce).is_err());
    let good = build_challenge(9, nonce);
    assert!(parse_challenge(&good[..CHALLENGE_LEN - 1], nonce).unwrap_err().to_string().contains("cut short"));
    let mut bad_magic = good.clone();
    bad_magic[CHALLENGE_MAGIC] ^= 1;
    assert!(parse_challenge(&bad_magic, nonce).unwrap_err().to_string().contains("magic"));
    assert_eq!(parse_challenge(&good, nonce + 1).unwrap(), None, "someone else's");
}

#[test]
fn a_game_port_says_what_to_do() {
    let message = GamePort("1.2.3.4:27015".parse().unwrap()).to_string();
    assert!(message.contains("1.2.3.4:27015") && message.contains("tv_port"), "{message}");
}

/// A server that answers `connect`'s getchallenge as `reply` says, and its connect as `after`.
fn fake_server(
    reply: impl FnOnce(u32) -> Vec<u8> + Send + 'static,
    after: impl FnOnce(&UdpSocket, SocketAddr) + Send + 'static,
) -> (SocketAddr, std::thread::JoinHandle<()>) {
    let server = UdpSocket::bind("127.0.0.1:0").unwrap();
    server.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let addr = server.local_addr().unwrap();
    let thread = std::thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let (_, client) = server.recv_from(&mut buf).unwrap();
        let nonce = le_u32(&buf, GETCHALLENGE_NONCE).unwrap();
        server.send_to(&reply(nonce), client).unwrap();
        after(&server, client);
    });
    (addr, thread)
}

#[test]
fn a_server_wanting_another_auth_protocol_is_refused() {
    let reply = |nonce| {
        let mut c = build_challenge(9, nonce);
        c[CHALLENGE_AUTH..CHALLENGE_AUTH + 4].copy_from_slice(&99u32.to_le_bytes());
        c
    };
    let (addr, server) = fake_server(reply, |_, _| {});
    let err = connect(addr, "me", "", "1", Duration::from_secs(5), no_end()).err().unwrap();
    assert!(err.to_string().contains("auth protocol 99"), "{err}");
    server.join().unwrap();
}

#[test]
fn no_reply_to_a_getchallenge_times_out() {
    let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = silent.local_addr().unwrap();
    let err = connect(addr, "me", "", "1", Duration::from_millis(50), no_end()).err().unwrap();
    assert!(err.to_string().contains("no challenge reply"), "{err}");
}

/// We may hold a slot on the server without knowing: it is given back before giving up.
#[test]
fn no_accept_times_out_and_says_goodbye() {
    let (addr, server) = fake_server(
        |nonce| build_challenge(9, nonce),
        |sock, _| {
            let mut buf = [0u8; 2048];
            sock.recv_from(&mut buf).unwrap(); // the connect
            let (n, _) = sock.recv_from(&mut buf).unwrap();
            assert!(netchan::verify_checksum(&buf[..n]), "a channel packet");
            let h = netchan::decode_header(&buf[..n]).unwrap();
            assert_eq!(h.challenge, Some(9));
            let body =
                crate::net::bits::BitReader::at(&buf[..n], h.body_offset * 8).read_bits(messages::TYPE_BITS).unwrap();
            assert_eq!(body, messages::NET_DISCONNECT);
        },
    );
    let err = connect(addr, "me", "", "1", Duration::from_millis(50), no_end()).err().unwrap();
    assert!(err.to_string().contains("no accept"), "{err}");
    server.join().unwrap();
}

/// The accept was lost and the server's first channel packet came instead: it is kept.
#[test]
fn channel_data_before_the_accept_counts_as_one() {
    let (addr, server) = fake_server(
        |nonce| build_challenge(9, nonce),
        |sock, client| {
            let mut buf = [0u8; 2048];
            sock.recv_from(&mut buf).unwrap();
            sock.send_to(&[1], client).unwrap(); // too short to be anything: skipped
            sock.send_to(b"\xff\xff\xff\xffZ", client).unwrap(); // out of band, of no use
            sock.send_to(&[1, 0, 0, 0, 0, 0], client).unwrap();
        },
    );
    let conn = connect(addr, "me", "", "1", Duration::from_secs(5), no_end()).unwrap();
    assert_eq!(conn.pending.as_deref(), Some([1, 0, 0, 0, 0, 0].as_slice()));
    assert_eq!(conn.challenge, 9);
    server.join().unwrap();
}

#[test]
fn a_stop_ends_a_wait_at_once_and_says_so() {
    let (addr, server) = fake_server(
        |nonce| build_challenge(9, nonce),
        |sock, _| {
            let mut buf = [0u8; 2048];
            sock.recv_from(&mut buf).unwrap(); // the connect
            let (n, _) = sock.recv_from(&mut buf).unwrap();
            assert!(netchan::verify_checksum(&buf[..n]), "a goodbye: the server may have given us a slot");
        },
    );
    let stop = AtomicBool::new(false);
    let started = Instant::now();
    let err = std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(100));
            stop.store(true, Ordering::Relaxed);
        });
        connect(addr, "me", "", "1", Duration::from_secs(20), Wait { stop: &stop, until: None }).err().unwrap()
    });
    assert!(err.is_stopped(), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    server.join().unwrap();
}

#[test]
fn the_time_left_ends_a_wait_for_the_challenge_and_says_so() {
    let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = silent.local_addr().unwrap();
    let stop = AtomicBool::new(false);
    let until = Some(Instant::now() + Duration::from_millis(50));
    let wait = Wait { stop: &stop, until };
    let err = connect(addr, "me", "", "1", Duration::from_secs(20), wait).err().unwrap();
    assert!(err.is_stopped(), "{err}");
    assert!(a2s_version(addr, Duration::from_secs(20), wait).is_err(), "no wait for an answer either");
}
