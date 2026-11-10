//! A room's notes in TF2: the game's text bubble (`show_annotation`), at a spot or following a
//! player, shown until hidden. A full update drops them, so the hub sends them again after every
//! one; a note gone from the room is hidden.

use super::Player;
use crate::decode::Decoder;
use crate::net::bits::BitBuf;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// A note the clients in the game keep seeing, until replaced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    /// Its own, while it is kept: what hides it.
    pub id: u32,
    /// What it says.
    pub text: String,
    /// Where it is, when it follows no one.
    pub at: [f32; 3],
    /// The user id of the player it follows.
    pub player: Option<u16>,
}

/// How often notes on players are looked at again: whether their player is in the match and
/// alive (TF2 leaves a dead player's bubble where they died, then moves it about).
const CHECK: Duration = Duration::from_millis(250);

/// The notes as the clients were last given them.
#[derive(Default)]
pub(super) struct Shown {
    /// The version of the room's notes; none before the first.
    version: Option<u64>,
    ids: Vec<u32>,
    /// A note follows a player: looked at again in a while.
    follows: bool,
    tried: Option<Instant>,
}

/// `note`'s bubble, or none while its player is not in the match or dead.
fn bubble(dec: &Decoder, note: &Note) -> Option<BitBuf> {
    let follow = match note.player {
        Some(user) => Some(dec.living_player_entity(user)?),
        None => None,
    };
    dec.annotation(note.id, &note.text, note.at, follow)
}

impl Player<'_> {
    /// Gives the clients the room's notes when they changed, and those on players as their player
    /// comes and goes, dies and respawns: new ones shown, gone ones hidden, all of them again after
    /// full updates and HUD resets.
    pub(super) fn show_notes(&mut self, version: u64, notes: &[Note]) {
        self.relay.hub().send_due_notes(Instant::now());
        let s = &mut self.shown_notes;
        let check = s.follows && s.tried.is_none_or(|t| t.elapsed() >= CHECK);
        if s.version == Some(version) && !check {
            return;
        }
        let Some(c) = self.cur.as_ref().filter(|c| c.started) else { return };
        let (mut ids, mut msgs) = (Vec::new(), Vec::new());
        for n in notes {
            if let Some(m) = bubble(&c.dec, n) {
                ids.push(n.id);
                msgs.push(m);
            }
        }
        let follows = notes.iter().any(|n| n.player.is_some());
        let mut hub = self.relay.hub();
        for id in s.ids.iter().filter(|id| !ids.contains(id)) {
            if let Some(m) = c.dec.hide_annotation(*id) {
                hub.tell_active(&m);
            }
        }
        for (id, m) in ids.iter().zip(&msgs) {
            if !s.ids.contains(id) {
                hub.tell_active(m);
            }
        }
        hub.set_notes(msgs);
        *s = Shown { version: Some(version), ids, follows, tried: Some(Instant::now()) };
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::upward_until;
    use super::*;
    use tf_demo_parser::demo::gameevent_gen::GameEvent;
    use tf_demo_parser::demo::message::Message;

    fn event(dec: &Decoder, msg: &BitBuf) -> GameEvent {
        let state = dec.parser_state_for_tests();
        let mut s = bitbuffer::BitReadStream::new(bitbuffer::BitReadBuffer::new(msg.bytes(), bitbuffer::LittleEndian));
        let t: tf_demo_parser::demo::message::MessageType = bitbuffer::BitRead::read(&mut s).unwrap();
        let Message::GameEvent(e) = Message::from_type(t, &mut s, state).unwrap() else { panic!() };
        e.event
    }

    #[test]
    fn a_note_is_a_bubble_shown_until_hidden_at_its_spot_or_on_its_player() {
        let c = upward_until(|c| c.dec.extractor().snapshot().players.iter().any(|p| p.alive));
        let player = c.dec.extractor().snapshot().players.iter().find(|p| p.alive).cloned().unwrap();
        let spot = Note { id: 7, text: "lobby".into(), at: [1.0, 2.0, 3.0], player: None };
        let GameEvent::ShowAnnotation(a) = event(&c.dec, &bubble(&c.dec, &spot).unwrap()) else { panic!() };
        assert_eq!((a.id, a.text.to_string(), a.follow_ent_index), (7, "lobby".into(), 0));
        assert_eq!([a.world_pos_x, a.world_pos_y, a.world_pos_z], [1.0, 2.0, 3.0]);
        assert!(a.lifetime < 0.0 && !a.show_distance, "for good, no distance");
        let on = Note { id: 8, text: "watch".into(), at: [0.0; 3], player: Some(player.id.0) };
        let GameEvent::ShowAnnotation(a) = event(&c.dec, &bubble(&c.dec, &on).unwrap()) else { panic!() };
        assert_eq!(a.follow_ent_index, player.ent);
        let GameEvent::HideAnnotation(h) = event(&c.dec, &c.dec.hide_annotation(8).unwrap()) else { panic!() };
        assert_eq!(h.id, 8);
    }

    #[test]
    fn a_note_on_a_dead_player_is_no_bubble_until_they_respawn() {
        use crate::output::Team;
        let dead = |c: &super::super::Cursor| {
            let players = c.dec.extractor().snapshot().players;
            players.iter().find(|p| !p.alive && matches!(p.team, Team::Red | Team::Blue)).map(|p| p.id.0)
        };
        let c = upward_until(|c| dead(c).is_some());
        let note = Note { id: 1, text: "x".into(), at: [0.0; 3], player: dead(&c) };
        assert!(bubble(&c.dec, &note).is_none(), "TF2 would leave it where their body was, then move it");
    }

    #[test]
    fn a_note_on_a_player_not_in_the_match_is_no_bubble_yet() {
        let c = upward_until(|c| c.dec.extractor().snapshot().players.iter().any(|p| p.alive));
        let gone = Note { id: 1, text: "x".into(), at: [0.0; 3], player: Some(u16::MAX) };
        assert!(bubble(&c.dec, &gone).is_none());
    }
}
