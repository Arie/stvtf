use super::*;

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
        let m = vgui_menu("specgui", show);
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
    let relay = |hub: Hub| Relay {
        sock: UdpSocket::bind("127.0.0.1:0").unwrap(),
        port: 0,
        hub: Mutex::new(hub),
        taken_over: Mutex::new(None),
    };
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
    b.hub.since_full = Some(vec![tail.clone(), tail]);
    let seq = b.client_state().out_seq;
    let now = Instant::now();
    b.hub.start_entities(b.addr, now);
    assert_eq!(b.client_state().phase, Phase::Active);
    assert_eq!(b.client_state().out_seq, seq + 2);
    assert!(netchan::verify_checksum(&b.reply()));
}

#[test]
fn a_client_joining_long_after_a_full_update_waits_for_the_next() {
    let mut b = Bench::new("", 1);
    b.connect();
    b.hub.start_entities(b.addr, Instant::now());
    assert_eq!(b.client_state().phase, Phase::WaitingFull);
}
