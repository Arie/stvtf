use std::time::Duration;

use super::{End, Fatal, Next, Options, Session, drive, finish, replay};
use crate::decode::Lifecycle;
use crate::net::messages::{self, Signon};
use crate::net::netchan::{self, OutHeader};
use crate::output::{Line, Status};

#[test]
fn a_lost_frame_asks_for_a_full_update_until_one_comes() {
    let opt = Options::default();
    let mut s = Session::new(None, 0, &opt);
    let mut emit = |_| {};
    s.lifecycle(Lifecycle { tick: Some(500), ..Lifecycle::default() }, &mut emit);
    assert_eq!(s.ack_tick(), 500);
    s.lifecycle(Lifecycle { tick: Some(501), lost_frame: true, ..Lifecycle::default() }, &mut emit);
    assert_eq!(s.ack_tick(), messages::FULL_UPDATE_TICK);
    s.lifecycle(Lifecycle { tick: Some(502), ..Lifecycle::default() }, &mut emit);
    assert_eq!(s.ack_tick(), messages::FULL_UPDATE_TICK, "a delta does not put the entities right");
    s.lifecycle(Lifecycle { tick: Some(503), full_update: true, ..Lifecycle::default() }, &mut emit);
    assert_eq!(s.ack_tick(), 503);
}

#[test]
fn only_unfixable_rejections_are_fatal() {
    assert_eq!(Fatal::from_reason("Bad spectator password"), Some(Fatal::BadPassword));
    assert_eq!(Fatal::from_reason("#GameUI_ServerRejectBadPassword"), Some(Fatal::BadPassword));
    assert_eq!(Fatal::from_reason("Server uses different class tables\n"), Some(Fatal::ClassTables));
    assert_eq!(Fatal::from_reason("#GameUI_ServerRejectServerFull"), None);
    assert_eq!(Fatal::from_reason("Kicked: password changed, reconnect"), None);
}

#[test]
fn silence_ends_the_session() {
    let opt = Options { silence: Duration::ZERO, ..Options::default() };
    let mut s = Session::new(None, 0, &opt);
    std::thread::sleep(Duration::from_millis(1));
    s.poll(&mut |_| {});
    assert!(s.broke.is_some_and(|(why, fatal)| why.starts_with("no data") && fatal.is_none()));
}

/// While a map loads the server answers seconds apart. It takes every copy of a reliable
/// message it gets, and flips the subchannel's bit for each: a copy sent on a timer is one
/// more `net_SignonState`, which forces a reconnect, or past `sv_signon_dos_disconnect` (20)
/// drops us ("Connection error"). As in the engine, a copy goes only once it is lost.
#[test]
fn a_reliable_message_goes_again_only_once_the_server_shows_it_lost() {
    let opt = Options::default();
    let challenge = 7;
    let mut s = Session::new(None, challenge, &opt);
    let tables = crate::builds::tables(crate::builds::CURRENT, None).unwrap();
    let mut dec = crate::decode::Decoder::new(&tables, opt.snapshot_hz).unwrap();
    let mut server_seq = 0;
    let mut from_server = |s: &mut Session<'_>, ack: i32, reliable_state: u8| {
        server_seq += 1;
        let header = OutHeader { sequence: server_seq, ack, challenge, reliable_state };
        s.datagram(&netchan::build_packet(&header, None, None), &mut dec, &mut |_| {});
    };
    s.send_reliable(&messages::signonstate(Signon::Spawn, 8), Some(0));
    let carried = s.out_seq - 1;

    std::thread::sleep(Duration::from_millis(50));
    let sent = s.out_seq;
    s.poll(&mut |_| {});
    assert_eq!(s.out_seq, sent, "a slow server is no reason to send it again");
    from_server(&mut s, carried - 1, 0);
    assert_eq!(s.out_seq, sent + 1, "only the reply: the server has not had the packet yet");

    let sent = s.out_seq;
    from_server(&mut s, carried, 0);
    assert_eq!(s.out_seq, sent + 2, "lost: sent again, then the reply");
    let resent = sent;
    assert!(s.pending.is_some());

    let sent = s.out_seq;
    from_server(&mut s, resent - 1, 0);
    assert_eq!(s.out_seq, sent + 1, "past the first copy, not the second: only the reply");

    let last = s.out_seq - 1;
    from_server(&mut s, last, 1);
    assert!(s.pending.is_none(), "acked");
}

#[test]
fn a_change_of_level_is_announced_and_leaves_the_signon_state() {
    let opt = Options::default();
    let mut s = Session::new(None, 0, &opt);
    let mut lines = Vec::new();
    s.advance(Signon::ChangeLevel, &mut |l| lines.push(l));
    assert_eq!(lines, [Line::Status(Status::Changelevel)]);
    assert_eq!(s.state, Signon::Connected);
}

/// A second changelevel keeps the spawncount; its reconnect still needs an answer.
#[test]
fn a_reconnect_after_another_changelevel_is_answered_again() {
    let opt = Options::default();
    let mut s = Session::new(None, 0, &opt);
    let mut emit = |_| {};
    s.lifecycle(Lifecycle { server_info: Some((79, "cp_x".into())), ..Lifecycle::default() }, &mut emit);
    s.state = Signon::Full;
    for _ in 0..2 {
        s.advance(Signon::ChangeLevel, &mut emit);
        s.advance(Signon::Connected, &mut emit);
        assert!(s.pending.take().is_some(), "answered");
        s.advance(Signon::Connected, &mut emit);
        assert!(s.pending.is_none(), "a copy of it is not");
    }
}

#[test]
fn a_new_map_restarts_the_tick_count() {
    let opt = Options::default();
    let mut s = Session::new(None, 0, &opt);
    let mut emit = |_| {};
    let info = |count| Lifecycle { server_info: Some((count, "cp_x".into())), ..Lifecycle::default() };
    s.lifecycle(info(1), &mut emit);
    s.lifecycle(Lifecycle { tick: Some(500), ..Lifecycle::default() }, &mut emit);
    s.lifecycle(info(1), &mut emit);
    assert_eq!(s.ack_tick(), 500, "the same map again");
    s.lifecycle(info(2), &mut emit);
    assert_eq!(s.ack_tick(), 0);
}

#[test]
fn a_failing_source_ends_the_session_saying_why() {
    let opt = Options::default();
    let mut s = Session::new(None, 0, &opt);
    let mut next = |_: &mut [u8]| Ok(Next::Failed("socket error: gone".to_string()));
    let outcome =
        drive(&mut s, &crate::builds::tables(crate::builds::CURRENT, None).unwrap(), None, &mut next, &mut |_| {})
            .unwrap();
    assert!(matches!(outcome.end, End::Broken(b) if b.reason == "socket error: gone" && !b.streamed));
}

/// `connect` can return with the first packet of the channel already read, when the accept
/// was lost: it counts as if it came next.
#[test]
fn a_packet_read_while_connecting_is_decoded_first() {
    let bytes = std::fs::read(crate::testing::fixture("bots_cp_badlands.cap")).unwrap();
    let records = crate::capture::read(&bytes).unwrap();
    let challenge = records[0].challenge().unwrap();
    let datagrams: Vec<&[u8]> = records[1..].iter().map(|r| r.data).take(200).collect();
    let tables = crate::builds::tables(crate::builds::CURRENT, None).unwrap();
    let opt = Options::default();
    let mut whole = Vec::new();
    replay(challenge, &datagrams, &tables, &opt, &mut |l| whole.push(l)).unwrap();

    let mut s = Session::new(None, challenge, &opt);
    let mut rest = datagrams[1..].iter();
    let mut next = |buf: &mut [u8]| Ok(rest.next().map_or(Next::Stop, |d| Next::recorded(d, buf)));
    let mut lines = Vec::new();
    let outcome = drive(&mut s, &tables, Some(datagrams[0]), &mut next, &mut |l| lines.push(l)).unwrap();
    finish(&s, &outcome, &mut |l| lines.push(l));
    assert!(lines.len() > 1, "{lines:?}");
    assert_eq!(lines, whole);
}

/// A relay whose signon starts over as we come in asks for a reconnect before it has sent a
/// `ServerInfo`: answered, as a game client answers it, or the relay sends nothing more.
#[test]
fn a_reconnect_asked_before_any_server_info_is_answered() {
    let opt = Options::default();
    let mut s = Session::new(None, 0, &opt);
    let sent = s.out_seq;
    s.advance(Signon::Connected, &mut |_| {});
    assert!(s.pending.is_some() && s.out_seq == sent + 1, "said once");
    assert_eq!(s.state, Signon::Connected);
    // Not again for the ack of it, nor while it is still on its way.
    s.advance(Signon::Connected, &mut |_| {});
    assert_eq!(s.out_seq, sent + 1);
}
