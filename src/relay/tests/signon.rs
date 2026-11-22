use super::*;

/// A map change: every client signs on again, except that what it was waiting to echo still
/// stands if that is the first step of signon.
#[test]
fn a_new_map_signs_every_client_on_again() {
    let mut b = Bench::new("", 1);
    b.connect();
    let now = Instant::now();
    let c = b.client_state();
    c.set_phase(Phase::Active, now);
    c.cursor = 5;
    c.gate = Some(Signon::Spawn);
    c.queue.push(BitBuf::default());
    b.hub.backlog.push(BitBuf::default(), None);
    b.hub.since_full = Some(Vec::new());
    b.hub.restart_signon(now);
    assert!(b.hub.backlog.len() == 0 && b.hub.since_full.is_none() && b.hub.backlog.open);
    let c = b.client_state();
    assert_eq!((c.phase, c.cursor, c.gate, c.queue.len()), (Phase::Signon, 0, None, 0));
}

/// Another demo's signon goes only once no client is in one: a game client sent a second while
/// it loads the map crashes.
#[test]
fn a_new_signon_waits_for_the_clients_in_one() {
    let mut b = Bench::new("", 1);
    b.hub.backlog.push(BitBuf::from_bytes(vec![1]), None);
    b.connect();
    let now = Instant::now();
    assert!(!b.hub.hold_for_signon(now), "a client just in, sent nothing yet");
    // In the game, then told to reconnect for a demo.
    let c = b.client_state();
    c.gate = None;
    c.set_phase(Phase::Active, now);
    let new_signon = |hub: &mut Hub| {
        hub.reconnect_all();
        hub.restart_signon(Instant::now());
        hub.backlog.push(BitBuf::from_bytes(vec![1]), None);
    };
    new_signon(&mut b.hub);
    assert!(b.hub.hold_for_signon(now), "told to reconnect");
    b.client_sends(1, &messages::signonstate(Signon::Connected, -1));
    assert_eq!(b.client_state().cursor, 1);
    assert!(b.hub.hold_for_signon(now), "part of the signon sent");
    b.client_state().set_phase(Phase::Active, now);
    assert!(!b.hub.hold_for_signon(now), "through it");
    new_signon(&mut b.hub);
    assert!(b.hub.hold_for_signon(now));
    b.hub.clients.clear();
    assert!(!b.hub.hold_for_signon(now), "gone");
}

/// A `Full` sent after a reconnect is no answer to it.
#[test]
fn a_state_of_the_signon_left_is_no_answer_to_a_reconnect() {
    let mut b = Bench::new("", 2);
    b.hub.backlog.push(BitBuf::from_bytes(vec![1]), None);
    b.connect();
    let now = Instant::now();
    b.client_sends(1, &messages::signonstate(Signon::Connected, -1));
    b.hub.clients.get_mut(&b.addr).unwrap().set_phase(Phase::Active, now);
    b.hub.reconnect_all();
    b.hub.restart_signon(now);
    b.client_sends(2, &messages::signonstate(Signon::Full, -1));
    let c = &b.hub.clients[&b.addr];
    assert!(c.reconnecting && c.gate.is_some() && c.phase == Phase::Signon, "still to answer");
    assert!(b.hub.hold_for_signon(now), "another signon waits for it");
    b.client_sends(3, &messages::signonstate(Signon::Connected, -1));
    let c = &b.hub.clients[&b.addr];
    assert!(!c.reconnecting && c.gate.is_none(), "answered");
}

/// At the hold's end a client still in its signon is dropped, told why, before the next goes: a
/// second signon crashes a game client. One in the game stays.
#[test]
fn a_client_still_in_its_signon_when_the_hold_ends_is_dropped_saying_why() {
    let mut b = Bench::new("", 2);
    b.hub.backlog.push(BitBuf::from_bytes(vec![1]), None);
    b.connect();
    let now = Instant::now();
    b.client_sends(1, &messages::signonstate(Signon::Connected, -1));
    let active: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let mut c = Client::new("in".into(), 1, 1, now);
    c.gate = None;
    c.set_phase(Phase::Active, now);
    b.hub.clients.insert(active, c);
    assert!(b.hub.hold_for_signon(now));
    sent_to(&b);
    assert!(b.hub.hold_for_signon((now + SIGNON_HOLD).checked_sub(Duration::from_millis(1)).unwrap()));
    assert!(!b.hub.hold_for_signon(now + SIGNON_HOLD), "not for good");
    assert!(!b.hub.clients.contains_key(&b.addr), "dropped");
    assert!(b.hub.clients.contains_key(&active), "the one in the game stays");
    let sent = sent_to(&b);
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::Disconnect(r) if r.contains("retry"))),
        "told why, and what to do: {sent:?}"
    );
    // The next signon: nothing of it reaches the client dropped.
    b.hub.reconnect_all();
    b.hub.restart_signon(now);
    b.hub.backlog.push(BitBuf::from_bytes(vec![1]), None);
    for c in b.hub.clients.values_mut() {
        c.pump(&b.hub.backlog);
    }
    b.hub.service(now);
    assert_eq!(sent_to(&b), [], "nothing more");
}

/// A hold given up (playback back on the demo playing, or nobody in) starts over the next time.
#[test]
fn a_hold_let_go_of_starts_over() {
    let (mut hub, active) = hub_with_one_in();
    hub.reconnect_all();
    let now = Instant::now();
    assert!(hub.hold_for_signon(now));
    hub.release_signon_hold();
    assert!(hub.hold_for_signon(now + SIGNON_HOLD), "{:?}", hub.clients[&active].phase);
}

/// What anyone on the internet can send a relay: random, forged, or past the checksum.
#[test]
fn garbage_from_clients_never_panics_the_hub() {
    let mut b = Bench::new("", 2);
    b.connect();
    let mut x = 0x1234_5678_9abc_def0_u64;
    let mut noise = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let now = Instant::now();
    for n in 0..5000 {
        let len = (noise() % 200) as usize;
        let body: Vec<u8> = (0..len).map(|_| noise() as u8).collect();
        let mut prefixed = match n % 3 {
            0 => Vec::new(),
            1 => b"\xff\xff\xff\xff".to_vec(),
            _ => {
                let mut w = BitWriter::new();
                w.write_bytes(&body);
                let header = OutHeader { sequence: n + 1, ack: 0, challenge: b.challenge, reliable_state: 0 };
                b.hub.datagram(b.addr, &netchan::build_packet(&header, None, Some(&w.finish())), now);
                continue;
            }
        };
        prefixed.extend_from_slice(&body);
        b.hub.datagram(b.addr, &prefixed, now);
        let stranger: SocketAddr = "127.0.0.1:9".parse().unwrap();
        b.hub.datagram(stranger, &prefixed, now);
    }
}

/// A client that connects while the next demo waits is not started on the old one's signon: it
/// would be dropped at the hold's end. It signs on to the next, or to this one if it stays.
#[test]
fn a_client_coming_in_during_a_hold_waits_for_the_next_signon() {
    let mut b = Bench::new("", 2);
    b.hub.mode = Mode::Play { paused: false };
    b.hub.backlog.push(BitBuf::from_bytes(vec![1]), None);
    let now = Instant::now();
    let loading: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let mut c = Client::new("loading".into(), 1, 1, now);
    c.gate = None;
    c.cursor = 1;
    b.hub.clients.insert(loading, c);
    assert!(b.hub.hold_for_signon(now));
    b.connect();
    b.client_sends(1, &messages::signonstate(Signon::Connected, -1));
    assert_eq!(b.client_state().cursor, 0, "not on the old signon");
    assert!(!b.hub.hold_for_signon(now + SIGNON_HOLD));
    assert!(b.hub.clients.contains_key(&b.addr), "not dropped");
    assert!(!b.hub.clients.contains_key(&loading));
    // The same again, but the hold let go of: it gets this demo's signon.
    b.hub.clients.insert(loading, Client::new("loading".into(), 1, 1, now));
    b.hub.clients.get_mut(&loading).unwrap().reconnecting = true;
    assert!(b.hub.hold_for_signon(now));
    b.client_sends(2, &messages::signonstate(Signon::Connected, -1));
    assert_eq!(b.client_state().cursor, 0);
    b.hub.release_signon_hold();
    assert_eq!(b.client_state().cursor, 1);
}

/// A playing relay's new signon finds a client part-way through the last one, not told to
/// reconnect (it came in as the switch went): it is, before it gets the new one.
#[test]
fn a_client_part_way_through_the_last_demos_signon_is_told_to_reconnect() {
    let mut b = Bench::new("", 1);
    b.hub.mode = Mode::Play { paused: false };
    b.hub.backlog.push(BitBuf::from_bytes(vec![1]), None);
    b.connect();
    b.client_sends(1, &messages::signonstate(Signon::Connected, -1));
    assert_eq!(b.client_state().cursor, 1);
    b.hub.restart_signon(Instant::now());
    let reconnect = messages::signonstate(Signon::Connected, -1);
    let c = b.client_state();
    assert!(c.reconnecting);
    assert_eq!((c.cursor, c.gate), (0, Some(Signon::Connected)));
    assert_eq!(c.queue, [reconnect]);
}

#[test]
fn a_clients_address_is_logged_without_its_end() {
    assert_eq!(masked_addr("203.0.113.57:27005".parse().unwrap()), "203.0.113.x:27005");
    assert_eq!(masked_addr("[2001:db8:1:2:3:4:5:6]:27005".parse().unwrap()), "2001:db8:1::x:27005");
}

/// The next relay goes on with a client's connection: its packets are read, ours are numbered
/// on, and one in a signon or a transfer is told to retry.
#[test]
fn a_client_handed_over_is_heard_and_sent_on_from_where_its_connection_was() {
    let mut b = Bench::new("", 2);
    b.connect();
    let now = Instant::now();
    {
        let c = b.client_state();
        c.gate = None;
        c.set_phase(Phase::Active, now);
        (c.out_seq, c.rx.in_seq, c.in_reliable_state) = (40, 12, 0b101);
    }
    let late: SocketAddr = "127.0.0.1:2".parse().unwrap();
    b.hub.clients.insert(late, Client::new("late".into(), 9, 9, now));
    let h = b.hub.hand_over(None);
    assert_eq!(h.clients(), 1, "the one in the game");
    assert!(b.hub.silent && b.hub.clients.is_empty());
    let config = RelayConfig { password: String::new(), ..RelayConfig::new("127.0.0.1:0") };
    let mut next = Hub::new(UdpSocket::bind("127.0.0.1:0").unwrap(), &config);
    next.take_over(&serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap(), now).unwrap();
    let c = &next.clients[&b.addr];
    assert_eq!((c.out_seq, c.rx.in_seq, c.in_reliable_state, c.phase), (40, 12, 0b101, Phase::WaitingFull));
    assert_eq!(c.challenge, b.challenge);
    b.hub = next;
    b.client_sends(13, &messages::tick(5));
    assert_eq!(b.client_state().rx.in_seq, 13);
}
