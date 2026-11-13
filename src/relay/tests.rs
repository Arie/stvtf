use super::client::{Client, Logged, Phase};
use super::hub::{Hub, Mode};
use super::server_msg::{is_reset_hud, reset_hud, set_pause, show_panel};
use super::*;
use crate::decode::Span;
use crate::net::bits::{BitReader, BitWriter};
use crate::net::handshake;
use crate::net::messages::{self, SVC_SETPAUSE, SVC_UPDATESTRINGTABLE, SVC_USERMESSAGE, Signon, TYPE_BITS};
use crate::net::netchan::{self, Header, OutHeader};
use crate::net::reliable::{Reassembler, SUBCHANNEL_BITS, SUBCHANNELS};
use std::hash::BuildHasher;

#[test]
fn the_first_block_goes_alone_then_all_subchannels_at_once() {
    let mut x = 1u32;
    let noise = (0..20_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect();
    let mut queue = vec![BitBuf::from_bytes(noise)];
    let mut c = Client::new("x".into(), 1, 2, Instant::now());
    c.queue.append(&mut queue);
    let first = c.next_region(1).unwrap();
    assert!(c.next_region(2).is_none(), "nothing before the first block is acked");
    let mut r = Reassembler::default();
    assert_eq!(r.feed(first.bytes(), 0).unwrap().completed.len(), 0);
    let ack = |seq, state| Header {
        sequence: seq,
        ack: seq,
        flags: 0,
        reliable_state: state,
        choked: None,
        challenge: None,
        body_offset: 0,
    };
    c.acked(&ack(1, 0b1));
    let blocks: Vec<BitBuf> = (2..).map_while(|seq| c.next_region(seq)).collect();
    assert_eq!(blocks.len(), usize::from(SUBCHANNELS));
    // The client gets all but the one on subchannel 2, which comes again.
    for (i, b) in blocks.iter().enumerate().filter(|(i, _)| *i != 2) {
        assert_eq!(r.feed(b.bytes(), 0).unwrap().subchannel, i as u8);
    }
    c.acked(&ack(9, 0b1111_1011 ^ 0b1));
    let again = c.next_region(10).unwrap();
    assert_eq!(
        BitReader::new(again.bytes()).read_bits(SUBCHANNEL_BITS).unwrap(),
        0,
        "lost block on the first free subchannel"
    );
    let mut done = r.feed(again.bytes(), 0).unwrap().completed;
    let mut state = 0b1111_1011 ^ 0b1 ^ 0b1;
    let mut seq = 10;
    while c.outgoing.is_some() {
        let sub = (0..SUBCHANNELS).find(|i| c.outgoing.as_ref().unwrap().in_flight(usize::from(*i)));
        if let Some(sub) = sub {
            state ^= 1 << sub;
        }
        c.acked(&ack(seq, state));
        seq += 1;
        while let Some(b) = c.next_region(seq) {
            done.extend(r.feed(b.bytes(), 0).unwrap().completed);
            let sub = BitReader::new(b.bytes()).read_bits(SUBCHANNEL_BITS).unwrap();
            state ^= 1 << sub;
            c.acked(&ack(seq, state));
            seq += 1;
        }
    }
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].len(), 20_000);
}

/// A string table change the server sends in a datagram's unreliable part, and again in
/// the next ones until acked.
fn datagram_with_table_change() -> (BitBuf, Lifecycle) {
    let mut w = BitWriter::new();
    w.write_bits(SVC_UPDATESTRINGTABLE, TYPE_BITS);
    w.write_bits(0xabcd, 16);
    let bits = w.finish();
    let life = Lifecycle {
        spans: vec![Span { id: u8::try_from(SVC_UPDATESTRINGTABLE).unwrap(), bits: 0..bits.nbits() }],
        ..Lifecycle::default()
    };
    (bits, life)
}

/// A hub with a client in the game, and its address.
fn hub_with_one_in() -> (Hub, SocketAddr) {
    let config = RelayConfig::new("127.0.0.1:0");
    let mut hub = Hub::new(UdpSocket::bind("127.0.0.1:0").unwrap(), &config);
    let now = Instant::now();
    let active: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let mut c = Client::new("in".into(), 1, 1, now);
    c.gate = None;
    c.set_phase(Phase::Active, now);
    hub.clients.insert(active, c);
    (hub, active)
}

#[test]
fn notes_go_again_after_every_full_update() {
    let (mut hub, active) = hub_with_one_in();
    let msg = |v: u32| {
        let mut w = BitWriter::new();
        w.write_bits(v, 16);
        w.finish()
    };
    let (key, note) = (msg(1), msg(2));
    // The reliable data sent: the pause, then the note.
    let sent = |hub: &mut Hub| {
        let c = hub.clients.get_mut(&active).unwrap();
        c.outgoing.take().map(|o| o.data().to_vec()).unwrap_or_default()
    };
    let mut w = BitWriter::new();
    w.write_buf(&set_pause(false));
    w.write_buf(&note);
    let expected = w.into_bytes();
    hub.set_notes(vec![note.clone()]);
    assert!(hub.clients[&active].queue.is_empty(), "for full updates to come; shown now by the player");
    hub.all_want_keyframe(Instant::now());
    hub.send_keyframe(&key, &[], false);
    assert!(sent(&mut hub).starts_with(&expected), "after its full update");
    hub.clients.get_mut(&active).unwrap().set_phase(Phase::WaitingFull, Instant::now());
    hub.mode = Mode::Play { paused: false };
    hub.keyframe = Some(vec![key]);
    hub.wants_entities(active, Instant::now());
    assert!(sent(&mut hub).starts_with(&expected), "and after the datagrams since one");
}

#[test]
fn notes_go_again_once_a_client_is_in_and_after_a_hud_reset() {
    let (mut hub, active) = hub_with_one_in();
    let mut w = BitWriter::new();
    w.write_bits(2, 16);
    let note = w.finish();
    hub.set_notes(vec![note.clone()]);
    let now = Instant::now();
    hub.clients.get_mut(&active).unwrap().enter_notes(&hub.notes.clone(), now);
    let queued = |hub: &mut Hub| std::mem::take(&mut hub.clients.get_mut(&active).unwrap().queue);
    assert_eq!(queued(&mut hub), std::slice::from_ref(&note), "with its entities");
    hub.send_due_notes(now);
    assert_eq!(queued(&mut hub), [], "not yet");
    hub.send_due_notes(now + NOTES_AFTER_ENTERING);
    assert_eq!(queued(&mut hub), std::slice::from_ref(&note), "once its HUD is up");
    hub.send_due_notes(now + NOTES_AFTER_ENTERING * 2);
    assert_eq!(queued(&mut hub), [], "once");
    hub.notes_again(now);
    hub.send_due_notes(now + NOTES_AFTER_RESET);
    assert_eq!(queued(&mut hub), std::slice::from_ref(&note), "past a HUD reset");
    // A reset as it goes in (a seek's full update) leaves the wait for its HUD.
    hub.clients.get_mut(&active).unwrap().enter_notes(&[], now);
    hub.notes_again(now);
    hub.send_due_notes(now + NOTES_AFTER_RESET);
    assert_eq!(queued(&mut hub), std::slice::from_ref(&note), "past the reset");
    hub.send_due_notes(now + NOTES_AFTER_ENTERING);
    assert_eq!(queued(&mut hub), [note], "and once its HUD is up");
}

#[test]
fn a_late_client_gets_the_table_changes_once_and_one_in_the_game_skips_them() {
    let (mut hub, active) = hub_with_one_in();
    let now = Instant::now();
    let (bits, life) = datagram_with_table_change();
    for _ in 0..3 {
        hub.upstream_stream(&bits, false, &life);
        hub.upstream_flush();
    }
    assert_eq!(hub.log.len(), 1, "logged once, however often it comes");
    let c = &hub.clients[&active];
    assert_eq!((c.cursor, c.queue.len()), (1, 0), "a client in the game has it from the datagram");
    let mut late = Client::new("late".into(), 2, 2, now);
    late.gate = None;
    late.pump(&hub.log);
    assert_eq!(late.queue, [bits]);
    assert!(!late.has_log(&hub.log), "no entities before the change is acked");
}

#[test]
fn tables_set_back_go_again_on_every_jump_back() {
    let (mut hub, active) = hub_with_one_in();
    let (later, life) = datagram_with_table_change();
    let mut w = BitWriter::new();
    w.write_bits(SVC_UPDATESTRINGTABLE, TYPE_BITS);
    w.write_bits(0x1234, 16);
    let back = w.finish();
    // Back from past the change, on to it again, and back again: the same set-back each time.
    for round in 1..=2 {
        hub.upstream_stream(&later, false, &life);
        hub.upstream_flush();
        hub.clients.get_mut(&active).unwrap().queue.clear();
        hub.set_tables_back(std::slice::from_ref(&later), std::slice::from_ref(&back));
        assert_eq!(hub.clients[&active].queue, std::slice::from_ref(&back), "jump back {round}");
    }
}

/// The relay's protocol from the client's side, without threads or clocks.
pub(super) struct Bench {
    pub(super) hub: Hub,
    pub(super) client: UdpSocket,
    pub(super) addr: SocketAddr,
    pub(super) challenge: u32,
    nonce: u32,
}

impl Bench {
    pub(super) fn new(password: &str, slots: usize) -> Self {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let config = RelayConfig { password: password.into(), slots, ..RelayConfig::new("127.0.0.1:0") };
        let hub = Hub::new(sock, &config);
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let addr = client.local_addr().unwrap();
        let challenge = hub.challenge_key.hash_one(addr) as u32;
        Bench { hub, client, addr, challenge, nonce: 7 }
    }

    fn connect_packet(&self, password: &str, protocol: i32) -> Vec<u8> {
        let mut p = handshake::build_connect(self.nonce, self.challenge, "me", password, "1");
        p[5..9].copy_from_slice(&protocol.to_le_bytes());
        p
    }

    fn connect_with(&mut self, packet: &[u8]) -> Vec<u8> {
        self.hub.datagram(self.addr, packet, Instant::now());
        self.reply()
    }

    pub(super) fn connect(&mut self) {
        let packet = self.connect_packet("", handshake::PROTOCOL_VERSION);
        assert_eq!(self.connect_with(&packet)[4], b'B', "accepted");
    }

    fn reply(&self) -> Vec<u8> {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        let n = self.client.recv(&mut buf).unwrap();
        buf.truncate(n);
        buf
    }

    pub(super) fn client_state(&mut self) -> &mut Client {
        self.hub.clients.get_mut(&self.addr).unwrap()
    }

    /// A channel packet from the client carrying `messages`.
    fn client_sends(&mut self, sequence: i32, messages: &BitBuf) {
        let header = OutHeader { sequence, ack: 0, challenge: self.challenge, reliable_state: 0 };
        let packet = netchan::build_packet(&header, None, Some(messages));
        self.hub.datagram(self.addr, &packet, Instant::now());
    }
}

#[test]
fn the_accept_goes_again_when_a_client_asks_again() {
    let mut b = Bench::new("", 1);
    b.connect();
    b.client_state().cursor = 3;
    b.connect();
    assert_eq!(b.client_state().cursor, 3, "the same connection, not a new one");
}

#[test]
fn a_connect_of_another_protocol_is_refused_saying_ours() {
    let mut b = Bench::new("", 1);
    let packet = b.connect_packet("", handshake::PROTOCOL_VERSION + 1);
    let reply = b.connect_with(&packet);
    assert_eq!(reply[4], b'9');
    let text = String::from_utf8_lossy(&reply);
    assert!(text.contains(&format!("protocol {}", handshake::PROTOCOL_VERSION)), "{text}");
    assert!(b.hub.clients.is_empty());
}

#[test]
fn a_goodbye_frees_the_slot() {
    let mut b = Bench::new("", 1);
    b.connect();
    b.client_sends(1, &messages::goodbye());
    assert!(b.hub.clients.is_empty());
}

#[test]
fn a_client_not_heard_from_times_out() {
    let mut b = Bench::new("", 1);
    b.connect();
    let now = Instant::now();
    b.client_state().set_phase(Phase::Active, now);
    b.hub.service(now);
    assert_eq!(b.hub.clients.len(), 1);
    b.hub.service(now + CLIENT_TIMEOUT);
    assert!(b.hub.clients.is_empty());
}

/// A game client loading a map sends nothing: in its signon, or told to reconnect, it has longer.
#[test]
fn a_client_loading_a_map_is_given_longer() {
    for reconnecting in [false, true] {
        let mut b = Bench::new("", 1);
        b.connect();
        let now = Instant::now();
        if reconnecting {
            b.client_state().set_phase(Phase::Active, now);
            b.hub.reconnect_all();
        }
        b.hub.service(now + CLIENT_TIMEOUT);
        assert_eq!(b.hub.clients.len(), 1, "reconnecting {reconnecting}");
        b.hub.service((now + SIGNON_TIMEOUT).checked_sub(Duration::from_millis(1)).unwrap());
        assert_eq!(b.hub.clients.len(), 1, "reconnecting {reconnecting}");
        b.hub.service(now + SIGNON_TIMEOUT);
        assert!(b.hub.clients.is_empty(), "reconnecting {reconnecting}");
    }
}

#[test]
fn a_hud_reset_is_told_from_other_messages() {
    let span = |m: &BitBuf, id: u32| Span { id: u8::try_from(id).unwrap(), bits: 0..m.nbits() };
    let reset = reset_hud();
    assert!(is_reset_hud(reset.bytes(), &span(&reset, SVC_USERMESSAGE)));
    let pause = set_pause(true);
    assert!(!is_reset_hud(pause.bytes(), &span(&pause, SVC_SETPAUSE)));
}

#[test]
fn the_spectators_panel_is_hidden_and_shown_as_the_game_does() {
    use tf_demo_parser::demo::message::usermessage::UserMessage;
    for show in [false, true] {
        let m = show_panel("specgui", show);
        let mut s = tf_demo_parser::Stream::from(bitbuffer::BitReadBuffer::new_owned(
            m.bytes().to_vec(),
            bitbuffer::LittleEndian,
        ));
        assert_eq!(s.read_int::<u32>(TYPE_BITS).unwrap(), SVC_USERMESSAGE);
        let u: UserMessage<'_> = s.read().unwrap();
        let said = format!("{u:?}");
        assert!(said.contains("VGuiMenu") && said.contains("specgui"), "{said}");
        assert_eq!(s.bits_left(), m.bytes().len() * 8 - m.nbits(), "nothing after it");
        assert_eq!(m.nbits(), TYPE_BITS + 8 + 11 + 10 * 8, "its name, whether to show it, and no keys");
    }
}

#[test]
fn reset_hud_reads_as_the_games_own() {
    use tf_demo_parser::demo::message::usermessage::UserMessage;
    let m = reset_hud();
    let mut s =
        tf_demo_parser::Stream::from(bitbuffer::BitReadBuffer::new_owned(m.bytes().to_vec(), bitbuffer::LittleEndian));
    assert_eq!(s.read_int::<u32>(TYPE_BITS).unwrap(), SVC_USERMESSAGE);
    let u: UserMessage<'_> = s.read().unwrap();
    assert!(matches!(u, UserMessage::ResetHUD(_)), "{u:?}");
    assert_eq!(s.bits_left(), m.bytes().len() * 8 - m.nbits(), "nothing after it");
}

#[test]
fn a_client_in_the_game_hears_from_us_at_least_every_keepalive() {
    let mut b = Bench::new("", 1);
    b.connect();
    let now = Instant::now();
    let c = b.client_state();
    c.set_phase(Phase::Active, now);
    c.last_sent = now;
    let seq = c.out_seq;
    b.hub.service(now);
    assert_eq!(b.client_state().out_seq, seq, "not yet");
    b.hub.service(now + KEEPALIVE * 2);
    assert_eq!(b.client_state().out_seq, seq + 1);
    assert!(netchan::verify_checksum(&b.reply()));
}

/// A client acks -1 until the full update reaches it, so only an ack -1 well after that
/// is a request for another.
#[test]
fn a_client_asks_for_another_full_update_only_after_the_grace() {
    let mut b = Bench::new("", 1);
    b.connect();
    let now = Instant::now();
    b.client_state().set_phase(Phase::Active, now);
    b.client_sends(1, &messages::tick(messages::FULL_UPDATE_TICK));
    assert_eq!(b.client_state().phase, Phase::Active);
    b.client_state().since = now.checked_sub(FULL_UPDATE_GRACE * 2).unwrap();
    b.client_sends(2, &messages::tick(messages::FULL_UPDATE_TICK));
    assert_eq!(b.client_state().phase, Phase::WaitingFull);
}

#[test]
fn the_server_is_asked_for_a_full_update_when_needed_and_not_too_often() {
    let mut b = Bench::new("", 1);
    let relay = |hub: Hub| Relay { sock: UdpSocket::bind("127.0.0.1:0").unwrap(), hub: Mutex::new(hub) };
    b.connect();
    let now = Instant::now();
    b.client_state().set_phase(Phase::Active, now);
    let mut hub_of = relay(b.hub);
    assert!(!hub_of.want_full_update(), "nobody needs one");
    let wants = hub_of.hub.get_mut().unwrap();
    wants.clients.get_mut(&b.addr).unwrap().set_phase(Phase::WaitingFull, now);
    wants.last_full = Some(now);
    assert!(!hub_of.want_full_update(), "the server just sent one");
    hub_of.hub.get_mut().unwrap().last_full = now.checked_sub(FULL_UPDATE_GAP * 2);
    assert!(hub_of.want_full_update());
}

#[test]
fn a_client_joining_soon_after_a_full_update_gets_the_datagrams_since() {
    let mut b = Bench::new("", 2);
    b.connect();
    let tail = {
        let mut w = BitWriter::new();
        w.write_bits(0b101_010, TYPE_BITS);
        w.finish()
    };
    b.hub.keyframe = Some(vec![tail.clone(), tail]);
    let seq = b.client_state().out_seq;
    let now = Instant::now();
    b.hub.wants_entities(b.addr, now);
    assert_eq!(b.client_state().phase, Phase::Active);
    assert_eq!(b.client_state().out_seq, seq + 2);
    assert!(netchan::verify_checksum(&b.reply()));
}

#[test]
fn a_client_joining_long_after_a_full_update_waits_for_the_next() {
    let mut b = Bench::new("", 1);
    b.connect();
    b.hub.wants_entities(b.addr, Instant::now());
    assert_eq!(b.client_state().phase, Phase::WaitingFull);
}

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
    b.hub.log.push(Logged { bits: BitBuf::default(), gate: None });
    b.hub.keyframe = Some(Vec::new());
    b.hub.restart_signon(now);
    assert!(b.hub.log.is_empty() && b.hub.keyframe.is_none() && b.hub.signon_open);
    let c = b.client_state();
    assert_eq!((c.phase, c.cursor, c.gate, c.queue.len()), (Phase::Signon, 0, None, 0));
}

/// Another demo's signon goes only once no client is in one: a game client sent a second while
/// it loads the map crashes.
#[test]
fn a_new_signon_waits_for_the_clients_in_one() {
    let mut b = Bench::new("", 1);
    b.hub.log.push(Logged { bits: BitBuf::from_bytes(vec![1]), gate: None });
    b.connect();
    let now = Instant::now();
    assert!(!b.hub.signon_waits(now), "a client just in, sent nothing yet");
    // In the game, then told to reconnect for a demo.
    let c = b.client_state();
    c.gate = None;
    c.set_phase(Phase::Active, now);
    let new_signon = |hub: &mut Hub| {
        hub.reconnect_all();
        hub.restart_signon(Instant::now());
        hub.log.push(Logged { bits: BitBuf::from_bytes(vec![1]), gate: None });
    };
    new_signon(&mut b.hub);
    assert!(b.hub.signon_waits(now), "told to reconnect");
    b.client_sends(1, &messages::signonstate(Signon::Connected, -1));
    assert_eq!(b.client_state().cursor, 1);
    assert!(b.hub.signon_waits(now), "part of the signon sent");
    b.client_state().set_phase(Phase::Active, now);
    assert!(!b.hub.signon_waits(now), "through it");
    new_signon(&mut b.hub);
    assert!(b.hub.signon_waits(now));
    b.hub.clients.clear();
    assert!(!b.hub.signon_waits(now), "gone");
}

/// What the bench's client is sent, read as a client reads it: each stream's messages.
fn sent_to(b: &Bench) -> Vec<ClientMsg> {
    b.client.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    let mut rx = crate::net::receiver::Receiver::new(Some(b.challenge));
    let mut buf = vec![0u8; MAX_DATAGRAM];
    let mut sent = Vec::new();
    while let Ok(n) = b.client.recv(&mut buf) {
        let Some(pkt) = rx.feed(&buf[..n], Instant::now()) else { continue };
        for s in &pkt.streams {
            sent.extend(client_messages(&s.bits));
        }
    }
    sent
}

/// At the hold's end a client still in its signon is dropped, told why, before the next goes: a
/// second signon crashes a game client. One in the game stays.
#[test]
fn a_client_still_in_its_signon_when_the_hold_ends_is_dropped_saying_why() {
    let mut b = Bench::new("", 2);
    b.hub.log.push(Logged { bits: BitBuf::from_bytes(vec![1]), gate: None });
    b.connect();
    let now = Instant::now();
    b.client_sends(1, &messages::signonstate(Signon::Connected, -1));
    let active: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let mut c = Client::new("in".into(), 1, 1, now);
    c.gate = None;
    c.set_phase(Phase::Active, now);
    b.hub.clients.insert(active, c);
    assert!(b.hub.signon_waits(now));
    sent_to(&b);
    assert!(b.hub.signon_waits((now + SIGNON_HOLD).checked_sub(Duration::from_millis(1)).unwrap()));
    assert!(!b.hub.signon_waits(now + SIGNON_HOLD), "not for good");
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
    b.hub.log.push(Logged { bits: BitBuf::from_bytes(vec![1]), gate: None });
    for c in b.hub.clients.values_mut() {
        c.pump(&b.hub.log);
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
    assert!(hub.signon_waits(now));
    hub.no_signon_waits();
    assert!(hub.signon_waits(now + SIGNON_HOLD), "{:?}", hub.clients[&active].phase);
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
    b.hub.log.push(Logged { bits: BitBuf::from_bytes(vec![1]), gate: None });
    let now = Instant::now();
    let loading: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let mut c = Client::new("loading".into(), 1, 1, now);
    c.gate = None;
    c.cursor = 1;
    b.hub.clients.insert(loading, c);
    assert!(b.hub.signon_waits(now));
    b.connect();
    b.client_sends(1, &messages::signonstate(Signon::Connected, -1));
    assert_eq!(b.client_state().cursor, 0, "not on the old signon");
    assert!(!b.hub.signon_waits(now + SIGNON_HOLD));
    assert!(b.hub.clients.contains_key(&b.addr), "not dropped");
    assert!(!b.hub.clients.contains_key(&loading));
    // The same again, but the hold let go of: it gets this demo's signon.
    b.hub.clients.insert(loading, Client::new("loading".into(), 1, 1, now));
    b.hub.clients.get_mut(&loading).unwrap().reconnecting = true;
    assert!(b.hub.signon_waits(now));
    b.client_sends(2, &messages::signonstate(Signon::Connected, -1));
    assert_eq!(b.client_state().cursor, 0);
    b.hub.no_signon_waits();
    assert_eq!(b.client_state().cursor, 1);
}

/// A playing relay's new signon finds a client part-way through the last one, not told to
/// reconnect (it came in as the switch went): it is, before it gets the new one.
#[test]
fn a_client_part_way_through_the_last_demos_signon_is_told_to_reconnect() {
    let mut b = Bench::new("", 1);
    b.hub.mode = Mode::Play { paused: false };
    b.hub.log.push(Logged { bits: BitBuf::from_bytes(vec![1]), gate: None });
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
