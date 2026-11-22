//! A room's names in TF2: players' names are set in `userinfo`, as a server does when a name
//! changes, and teams' names in the tournament cvars (`mp_tournament_blueteamname`,
//! `mp_tournament_redteamname`). The demo's own messages give the clients the game's names again
//! (a player joining, a jump back), so they are given the room's names after every full update,
//! and when the demo rewrote a name.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::player::Player;
use crate::net::bits::BitBuf;
use crate::net::messages::set_convar;

/// The names the clients in the game see in place of the game's: players' by `SteamID`
/// (`[U:1:N]`), teams' by side (`red`, `blue`).
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct Names {
    /// Players' names, by `SteamID`.
    #[serde(default)]
    pub players: BTreeMap<String, String>,
    /// Teams' names, by side.
    #[serde(default)]
    pub teams: BTreeMap<String, String>,
}

/// How often the players in the match are looked at again: one named may have joined.
const CHECK: Duration = Duration::from_millis(500);
/// The teams, the cvar that names each, and its name when the room gives none.
const TEAMS: [(&str, &str, &str); 2] =
    [("blue", "mp_tournament_blueteamname", "BLU"), ("red", "mp_tournament_redteamname", "RED")];

/// The names as the clients were last given them.
#[derive(Default)]
pub(super) struct Shown {
    version: Option<u64>,
    names: Arc<Names>,
    /// The messages that set them: sent again after every full update.
    pub(super) msgs: Vec<BitBuf>,
    /// How many string table entries the demo had rewritten then.
    rewritten: u32,
    tried: Option<Instant>,
}

impl Player<'_> {
    /// Gives the clients the room's names when they changed, when a player named joined and when
    /// the demo told them a name again; those no longer named get the game's own name back.
    pub(super) fn show_names(&mut self, version: u64, names: &Arc<Names>) {
        let s = &self.shown_names;
        if names.players.is_empty() && names.teams.is_empty() && s.msgs.is_empty() && s.names.players.is_empty() {
            return;
        }
        if s.version == Some(version) && s.tried.is_some_and(|t| t.elapsed() < CHECK) {
            return;
        }
        let Some(c) = self.cur.as_ref().filter(|c| c.started) else { return };
        let given: BTreeMap<String, Option<String>> =
            names.players.iter().map(|(id, n)| (id.clone(), Some(n.clone()))).collect();
        let mut msgs: Vec<BitBuf> = c.dec.scene().named(&given).into_iter().collect();
        let team = |side: &str, own: &'static str| names.teams.get(side).map_or(own, String::as_str).to_string();
        let teams: Vec<BitBuf> = TEAMS.iter().map(|(side, cvar, own)| set_convar(cvar, &team(side, own))).collect();
        // No longer named: the game's own names again, sent once.
        let dropped: BTreeMap<String, Option<String>> =
            s.names.players.keys().filter(|id| !names.players.contains_key(*id)).map(|id| (id.clone(), None)).collect();
        let mut once: Vec<BitBuf> = c.dec.scene().named(&dropped).into_iter().collect();
        if !names.teams.is_empty() {
            msgs.extend(teams);
        } else if !s.names.teams.is_empty() {
            once.extend(teams);
        }
        let rewritten = c.dec.scene().tables_rewritten();
        let again = rewritten != s.rewritten;
        let mut hub = self.relay.hub();
        for m in once.iter().chain(msgs.iter().filter(|m| again || !s.msgs.contains(m))) {
            hub.queue_to_active(m);
        }
        hub.set_notes(self.shown_notes.bubbles().chain(&msgs).cloned().collect());
        drop(hub);
        self.shown_names =
            Shown { version: Some(version), names: names.clone(), msgs, rewritten, tried: Some(Instant::now()) };
    }
}
