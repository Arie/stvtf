use super::*;

/// A game client that loses datagrams, as one over the internet does: it keeps the frames it got
/// and acks the last, and a delta from a frame it lacks would crash it.
struct LossyClient {
    rx: Receiver,
    /// What it makes of what it gets, when the relay sends entities of its own: the relay's
    /// decoder, which never read those, cannot read their changes.
    own: Option<Cursor>,
    frames: std::collections::BTreeSet<u32>,
    got: usize,
    fulls: usize,
    deltas: usize,
}

impl LossyClient {
    /// Takes what it was sent since last time, all but every `lose`th packet, and acks the last
    /// frame it has to `relay`.
    fn take(&mut self, b: &Bench, relay: &Relay, reader: &Cursor, lose: usize) {
        b.client.set_read_timeout(Some(Duration::from_millis(2))).unwrap();
        let mut buf = vec![0u8; crate::net::MAX_DATAGRAM];
        while let Ok(len) = b.client.recv(&mut buf) {
            // A packet in parts is lost whole.
            let Some(pkt) = self.rx.feed(&buf[..len], Instant::now()) else { continue };
            self.got += 1;
            let dropped = self.got.is_multiple_of(lose);
            for s in pkt.streams.iter().filter(|s| !s.reliable && !dropped) {
                let reader = self.own.as_ref().unwrap_or(reader);
                let Some(tick) = reader.dec.frame_tick(s.bits.bytes()) else { continue };
                let entities = reader.dec.entities(s.bits.bytes());
                if let Some(own) = &mut self.own {
                    assert!(!own.dec.feed(s.bits.bytes(), s.bits.nbits()).lost_frame, "a frame the client cannot take");
                }
                match entities {
                    None => continue,
                    Some(Entities::Full) => {
                        self.frames.clear();
                        self.fulls += 1;
                    }
                    Some(Entities::Delta(base)) => {
                        assert!(
                            self.frames.contains(&base),
                            "a delta to {tick} from {base}, the client has {:?}",
                            self.frames
                        );
                        self.frames.retain(|t| *t >= base);
                        self.deltas += 1;
                    }
                }
                self.frames.insert(tick);
            }
        }
        // Its ack, and all it was sent reliably.
        for c in relay.hub().clients.values_mut() {
            if let Some(last) = self.frames.last().filter(|t| c.frames.contains(t)) {
                c.frames.retain(|t| t >= last);
                (c.acked, c.keyed) = (Some(*last), None);
            }
            c.outgoing = None;
            c.queue.clear();
        }
    }
}

/// Any datagram may be lost: a client's entities are a delta from the frame it acked, never from
/// one it may lack, and it is given a full update when there is none to write them from.
#[test]
fn a_client_that_loses_datagrams_is_never_sent_a_delta_from_a_frame_it_lacks() {
    loses_datagrams(None);
}

/// Nor with the relay's camera on its way along a path, each frame written for it.
#[test]
fn a_client_that_loses_datagrams_follows_a_camera_on_its_path() {
    loses_datagrams(Some(&super::super::shot_tests::across(60.0)));
}

fn loses_datagrams(camera: Option<&super::super::Camera>) {
    for lose in [3, 7, 20] {
        let mut b = bench_with_one_in();
        b.client_state().set_phase(Phase::WaitingFull, Instant::now());
        let c = upward_until(four_frames_kept);
        let (relay, c, want) = relay_playing(&mut b, c, 0);
        let want = Want { camera: camera.cloned(), ..want };
        relay.hub().start_playing();
        let mut p = Player::new(&relay);
        p.cur = Some(c);
        let mut client = LossyClient {
            rx: Receiver::new(Some(b.challenge)),
            own: camera.is_some().then(|| Cursor::open_detached(&want.demo).unwrap()),
            frames: std::collections::BTreeSet::default(),
            got: 0,
            fulls: 0,
            deltas: 0,
        };
        for _ in 0..400 {
            // A full update for one that waits for it, as the player's loop gives it.
            if relay.hub().wants_keyframe() {
                let _ = p.send_keyframe(0.015);
            } else {
                // The next frame, past what is none.
                while p.cur.as_mut().unwrap().next_tick() == Some(NOT_A_FRAME) {
                    p.cur.as_mut().unwrap().skip_next();
                }
                p.play_frame(&want);
            }
            // Read as the player's own decoder reads them: it has the baselines of now.
            client.take(&b, &relay, p.cur.as_ref().unwrap(), lose);
        }
        assert!(client.deltas > 150, "losing every {lose}th: {} full updates, {} deltas", client.fulls, client.deltas);
        assert!(client.fulls < 40, "not a full update for every loss: {}", client.fulls);
        let man = p.cur.as_ref().unwrap().dec.cameraman();
        assert_eq!(man.is_some(), camera.is_some());
        if let Some(own) = &client.own {
            assert!(own.dec.world_dump().contains_key(&man.unwrap()), "the client has the cameraman");
        }
    }
}
