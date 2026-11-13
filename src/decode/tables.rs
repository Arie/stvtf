//! The string tables as the frames so far left them, as far as a jump back needs: which entries
//! each table has, and the values of those whose later ones a client must not keep. Those are
//! `userinfo` (who is in each player slot) and `instancebaseline` (what an entity of each class
//! starts from: the full updates are written against the decoder's); the rest only grow, or do not
//! matter at an earlier moment (the lights, which models are loaded).
//!
//! A client that has the tables of a later moment is set back with `svc_UpdateStringTable`s of the
//! entries that differ ([`Tables::set_back`]).

use crate::net::bits::BitBuf;
use crate::net::messages::SVC_UPDATESTRINGTABLE;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use tf_demo_parser::ParserState;
use tf_demo_parser::demo::message::Message;
use tf_demo_parser::demo::message::stringtable::UpdateStringTableMessage;
use tf_demo_parser::demo::packet::stringtable::{ExtraData, StringTableEntry};

/// The tables whose values are kept.
fn kept(name: &str) -> bool {
    matches!(name, "userinfo" | "instancebaseline")
}

/// An entry's value: its text, and its data with the data's bits, to compare.
#[derive(Clone)]
struct Value {
    text: Option<String>,
    data: Option<(ExtraData<'static>, Vec<u8>)>,
}

impl Value {
    fn same(&self, other: &Value) -> bool {
        self.text == other.text && self.data.as_ref().map(|d| &d.1) == other.data.as_ref().map(|d| &d.1)
    }
}

fn data_bits(d: &ExtraData<'_>) -> Vec<u8> {
    let mut s = d.data.clone();
    let mut out = Vec::new();
    while s.bits_left() >= 8 {
        out.push(s.read_int::<u8>(8).unwrap_or(0));
    }
    let rest = s.bits_left();
    if rest > 0 {
        out.push(s.read_int::<u8>(rest).unwrap_or(0));
        out.push(rest as u8);
    }
    out
}

struct Table {
    name: String,
    /// The most entries it has room for.
    max: u16,
    entries: HashSet<u16>,
    /// For a kept table, its entries' values.
    values: Option<BTreeMap<u16, Value>>,
}

/// The table of the models loaded, by model index.
const MODEL_PRECACHE: &str = "modelprecache";

/// Sprites a beam can be drawn with, best first, as `modelprecache` names them.
const BEAM_SPRITES: [&str; 3] = ["sprites/laserbeam.vmt", "sprites/physbeam.vmt", "sprites/laser.vmt"];

/// What the clients' `modelprecache` has had since their signon, which a jump back does not take
/// from them: how many entries, and where the match's own laser is.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub(crate) struct ModelsSent {
    next: u16,
    laser: Option<u16>,
}

impl ModelsSent {
    /// The clients were sent models up to `next`.
    #[cfg(test)]
    pub(crate) fn next_for_tests(next: u16) -> Self {
        ModelsSent { next, laser: None }
    }

    /// These, with what `tables` have: the clients were sent them too.
    pub(super) fn with_tables(self, tables: &Tables) -> ModelsSent {
        ModelsSent { next: self.next.max(tables.models), laser: self.laser.or(tables.match_laser()) }
    }
}

/// The model index to draw beams with, and the message that loads it, to send first, if the
/// clients do not have it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BeamModel {
    pub(crate) index: u16,
    pub(crate) load: Option<BitBuf>,
}

#[derive(Default)]
pub(crate) struct Tables {
    /// By table id.
    by_id: Vec<Table>,
    /// How many updates rewrote an entry of a kept table.
    rewritten: u32,
    /// The best of [`BEAM_SPRITES`] loaded: its rank there and its model index.
    beam_sprite: Option<(usize, u16)>,
    /// One past the last entry of `modelprecache`.
    models: u16,
}

impl Tables {
    /// Takes the string table messages among a stream's.
    pub(crate) fn note(&mut self, messages: &[Message<'_>]) {
        for msg in messages {
            match msg {
                Message::CreateStringTable(t) => {
                    let name = t.table.name.to_string();
                    if name == MODEL_PRECACHE {
                        for (i, e) in &t.table.entries {
                            self.note_model(*i, e.text.as_deref());
                        }
                    }
                    let values = kept(&name).then(|| t.table.entries.iter().map(|(i, e)| (*i, value(e))).collect());
                    let entries = t.table.entries.iter().map(|(i, _)| *i).collect();
                    self.by_id.push(Table { name, max: t.table.max_entries, entries, values });
                }
                Message::UpdateStringTable(u) => {
                    if self.by_id.get(usize::from(u.table_id)).is_some_and(|t| t.name == MODEL_PRECACHE) {
                        for (i, e) in &u.entries {
                            self.note_model(*i, e.text.as_deref());
                        }
                    }
                    let Some(table) = self.by_id.get_mut(usize::from(u.table_id)) else { continue };
                    for (i, e) in &u.entries {
                        let fresh = table.entries.insert(*i);
                        let Some(values) = &mut table.values else { continue };
                        let v = values.entry(*i).or_insert(Value { text: None, data: None });
                        let was = v.clone();
                        if let Some(t) = &e.text {
                            v.text = Some(t.to_string());
                        }
                        // No data clears it, as a game client does (a player who left, in
                        // `userinfo`).
                        v.data = e.extra_data.as_ref().map(|d| (d.to_owned(), data_bits(d)));
                        // Sent again as it was is no change.
                        if !fresh && !v.same(&was) {
                            self.rewritten += 1;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn note_model(&mut self, index: u16, name: Option<&str>) {
        self.models = self.models.max(index.saturating_add(1));
        let Some(rank) = name.and_then(|n| BEAM_SPRITES.iter().position(|s| n.eq_ignore_ascii_case(s))) else { return };
        if self.beam_sprite.is_none_or(|(r, _)| rank < r) {
            self.beam_sprite = Some((rank, index));
        }
    }

    /// The model index of a sprite to draw beams with, if one is loaded.
    pub(crate) fn beam_sprite(&self) -> Option<u16> {
        self.beam_sprite.map(|(_, i)| i)
    }

    /// The match's own laser, if it loaded it.
    pub(crate) fn match_laser(&self) -> Option<u16> {
        self.beam_sprite.filter(|(rank, _)| *rank == 0).map(|(_, i)| i)
    }

    /// Whether the match loaded the laser after the clients were given the relay's at `before`: a
    /// game client has the name already and adds nothing, so every model the match loads after it
    /// is one index off for them until they sign on again.
    pub(crate) fn laser_loaded_twice(&self, before: Option<u16>) -> bool {
        before.is_some_and(|b| self.match_laser().is_some_and(|m| m != b))
    }

    /// The model index to draw beams with: the solid laser, loaded by the match or else by an
    /// `svc_UpdateStringTable` (also given) adding it to `modelprecache`. A game client adds an
    /// entry past its last at the end, whatever its index, so it goes in the next free one: past
    /// all the clients were `sent`, which after a jump back is more than these tables have.
    /// `before` is where the clients were given it: they keep it there until they sign on again,
    /// whatever the match loads there later (for an index it has, a game client takes only the
    /// data), and a jump back leaves it too. None when the table is full or unknown.
    pub(crate) fn laser(&self, state: &ParserState, before: Option<u16>, sent: ModelsSent) -> Option<BeamModel> {
        if let Some(index) = before {
            return Some(BeamModel { index, load: None });
        }
        let sent = sent.with_tables(self);
        if let Some(index) = sent.laser {
            return Some(BeamModel { index, load: None });
        }
        let (id, table) = self.by_id.iter().enumerate().find(|(_, t)| t.name == MODEL_PRECACHE)?;
        let slot = sent.next;
        if slot >= table.max {
            return None;
        }
        let entry = StringTableEntry { text: Some(Cow::Borrowed(BEAM_SPRITES[0])), extra_data: None };
        let msg = UpdateStringTableMessage { entries: vec![(slot, entry)], table_id: u8::try_from(id).ok()? };
        Some(BeamModel { index: slot, load: Some(super::encode_message(SVC_UPDATESTRINGTABLE, &msg, state)?) })
    }

    /// How many entries of `userinfo` and `instancebaseline` were rewritten so far: a client with
    /// the tables of a later moment has the wrong ones for this only if the count grew since.
    pub(crate) fn rewritten(&self) -> u32 {
        self.rewritten
    }

    /// `svc_UpdateStringTable`s that set a client with the tables of `later` back to these: the
    /// entries whose values differ, as they are here. Entries only `later` has stay, as a slot or
    /// class not used yet. None if one does not encode, or the tables are not the same ones.
    pub(crate) fn set_back(&self, later: &Tables, state: &ParserState) -> Option<Vec<BitBuf>> {
        let mut out = Vec::new();
        for (id, (ours, theirs)) in self.by_id.iter().zip(&later.by_id).enumerate() {
            if ours.name != theirs.name {
                return None;
            }
            let (Some(ours), Some(theirs)) = (&ours.values, &theirs.values) else { continue };
            let entries: Vec<(u16, StringTableEntry<'_>)> = ours
                .iter()
                .filter(|(i, v)| theirs.get(i).is_none_or(|w| !v.same(w)))
                .map(|(i, v)| {
                    let entry = StringTableEntry {
                        text: v.text.as_deref().map(Cow::Borrowed),
                        extra_data: v.data.as_ref().map(|d| d.0.clone()),
                    };
                    (*i, entry)
                })
                .collect();
            if entries.is_empty() {
                continue;
            }
            let msg = UpdateStringTableMessage { entries, table_id: u8::try_from(id).ok()? };
            out.push(super::encode_message(SVC_UPDATESTRINGTABLE, &msg, state)?);
        }
        Some(out)
    }
}

fn value(e: &StringTableEntry<'_>) -> Value {
    Value {
        text: e.text.as_ref().map(ToString::to_string),
        data: e.extra_data.as_ref().map(|d| (d.to_owned(), data_bits(d))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitbuffer::{BitRead, BitReadBuffer, BitReadStream, LittleEndian};
    use tf_demo_parser::demo::message::MessageType;
    use tf_demo_parser::demo::message::stringtable::{CreateStringTableMessage, StringTableMeta};
    use tf_demo_parser::demo::packet::stringtable::StringTable;

    fn entry(text: &str, data: &[u8]) -> StringTableEntry<'static> {
        let data = BitReadStream::new(BitReadBuffer::new_owned(data.to_vec(), LittleEndian));
        StringTableEntry { text: Some(Cow::Owned(text.into())), extra_data: Some(ExtraData::new(data)) }
    }

    fn userinfo(entries: Vec<(u16, StringTableEntry<'static>)>) -> Message<'static> {
        let table = StringTable {
            name: "userinfo".into(),
            entries,
            max_entries: 256,
            fixed_user_data_size: None,
            client_entries: None,
            compressed: false,
        };
        Message::CreateStringTable(CreateStringTableMessage { table })
    }

    fn update(entries: Vec<(u16, StringTableEntry<'static>)>) -> Message<'static> {
        Message::UpdateStringTable(UpdateStringTableMessage { entries, table_id: 0 })
    }

    fn value_of(t: &Tables, i: u16) -> Option<Vec<u8>> {
        t.by_id[0].values.as_ref()?.get(&i)?.data.as_ref().map(|d| d.1.clone())
    }

    #[test]
    fn a_client_with_later_tables_is_set_back_to_earlier_ones() {
        let mut state = ParserState::new(24, |_| true, false);
        state.string_tables.push(StringTableMeta { max_entries: 256, fixed_userdata_size: None });
        let created = userinfo(vec![(0, entry("0", b"alice")), (1, entry("1", b"bob"))]);
        // Later, slot 1 changed hands (and was sent again as it was once), and slot 2 filled.
        let changes = update(vec![(1, entry("1", b"carol")), (2, entry("2", b"dave"))]);
        let again = update(vec![(0, entry("0", b"alice"))]);
        let mut earlier = Tables::default();
        earlier.note(std::slice::from_ref(&created));
        let mut later = Tables::default();
        later.note(&[created, changes, again]);
        assert_eq!((earlier.rewritten(), later.rewritten()), (0, 1), "only a change of value counts");

        let back = earlier.set_back(&later, &state).unwrap();
        assert_eq!(back.len(), 1);
        let mut s = BitReadStream::new(BitReadBuffer::new(back[0].bytes(), LittleEndian));
        let t = MessageType::read(&mut s).unwrap();
        let msg = Message::from_type(t, &mut s, &state).unwrap();
        let Message::UpdateStringTable(u) = &msg else { panic!("{msg:?}") };
        assert_eq!(u.entries.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [1], "only the slot that changed");
        later.note(&[msg]);
        assert_eq!(value_of(&later, 1), value_of(&earlier, 1), "bob again");
        assert_eq!(value_of(&later, 0), value_of(&earlier, 0));
        assert!(value_of(&later, 2).is_some(), "a slot used only later stays");
    }

    fn models(entries: Vec<(u16, StringTableEntry<'static>)>) -> Message<'static> {
        let table = StringTable {
            name: MODEL_PRECACHE.into(),
            entries,
            max_entries: 2048,
            fixed_user_data_size: None,
            client_entries: None,
            compressed: false,
        };
        Message::CreateStringTable(CreateStringTableMessage { table })
    }

    fn model(name: &str) -> StringTableEntry<'static> {
        StringTableEntry { text: Some(Cow::Owned(name.into())), extra_data: None }
    }

    /// Given the laser at a slot, the clients keep it there: a game client takes only the data of
    /// a later entry there, not its name.
    #[test]
    fn the_laser_keeps_its_slot_when_the_match_takes_it() {
        let mut state = ParserState::new(24, |_| true, false);
        state.string_tables.push(StringTableMeta { max_entries: 2048, fixed_userdata_size: None });
        let mut t = Tables::default();
        t.note(&[models(vec![(0, model("maps/x.bsp")), (1, model("models/a.mdl"))])]);
        let laser = t.laser(&state, None, ModelsSent::default()).unwrap();
        assert_eq!(laser.index, 2, "the next free");
        assert!(laser.load.is_some());
        t.note(&[update(vec![(2, model("models/b.mdl"))])]);
        assert_eq!(
            t.laser(&state, Some(2), ModelsSent::default()).map(|m| (m.index, m.load.is_none())),
            Some((2, true)),
            "stays, no message"
        );
        t.note(&[update(vec![(3, model(BEAM_SPRITES[0]))])]);
        assert_eq!(
            t.laser(&state, Some(2), ModelsSent::default()).map(|m| (m.index, m.load.is_none())),
            Some((2, true)),
            "the match's own too"
        );
        // A new signon: the clients have the match's tables only.
        let mut fresh = Tables::default();
        fresh.note(&[models(vec![(0, model("maps/x.bsp")), (1, model("models/a.mdl")), (2, model("models/b.mdl"))])]);
        assert_eq!(
            fresh.laser(&state, None, ModelsSent::default()).map(|m| (m.index, m.load.is_some())),
            Some((3, true)),
            "the next free again"
        );
    }

    /// After a jump back the decoder's tables are of an earlier moment, the clients' are not: the
    /// laser goes past every model they were sent, or where the match's own is for them.
    #[test]
    fn the_laser_after_a_jump_back_goes_past_what_the_clients_have() {
        let mut state = ParserState::new(24, |_| true, false);
        state.string_tables.push(StringTableMeta { max_entries: 2048, fixed_userdata_size: None });
        let created = models(vec![(0, model("maps/x.bsp")), (1, model("models/a.mdl"))]);
        let mut earlier = Tables::default();
        earlier.note(std::slice::from_ref(&created));
        let mut later = Tables::default();
        later.note(&[created.clone(), update(vec![(2, model("models/b.mdl")), (3, model("models/c.mdl"))])]);
        let sent = ModelsSent::default().with_tables(&later);
        assert_eq!(earlier.laser(&state, None, sent).map(|m| (m.index, m.load.is_some())), Some((4, true)));
        later.note(&[update(vec![(4, model(BEAM_SPRITES[0]))])]);
        let sent = sent.with_tables(&later);
        assert_eq!(
            earlier.laser(&state, None, sent).map(|m| (m.index, m.load.is_none())),
            Some((4, true)),
            "the match's"
        );
        assert_eq!(sent.with_tables(&earlier).next, 5, "it only grows");
    }

    /// The match loading the laser after the clients were given the relay's: a game client has the
    /// name, adds nothing, and every model after it is one off for it.
    #[test]
    fn the_match_loading_the_laser_after_the_relays_is_told() {
        let mut t = Tables::default();
        t.note(&[models(vec![(0, model("maps/x.bsp")), (1, model("models/a.mdl"))])]);
        assert!(!t.laser_loaded_twice(Some(2)));
        assert!(!t.laser_loaded_twice(None));
        t.note(&[update(vec![(2, model("models/b.mdl")), (3, model(BEAM_SPRITES[0]))])]);
        assert!(t.laser_loaded_twice(Some(2)));
        assert!(!t.laser_loaded_twice(None), "the clients have only the match's");
        assert!(!t.laser_loaded_twice(Some(3)), "where the clients have it");
    }

    #[test]
    fn a_player_who_left_is_set_back_too() {
        let mut state = ParserState::new(24, |_| true, false);
        state.string_tables.push(StringTableMeta { max_entries: 256, fixed_userdata_size: None });
        let created = userinfo(vec![(0, entry("0", b"alice"))]);
        // Later, alice left: her slot's data cleared.
        let left = update(vec![(0, StringTableEntry { text: None, extra_data: None })]);
        let mut earlier = Tables::default();
        earlier.note(std::slice::from_ref(&created));
        let mut later = Tables::default();
        later.note(&[created, left]);
        assert_eq!(later.rewritten(), 1, "a clearing is a change");
        assert_eq!(value_of(&later, 0), None);
        let back = earlier.set_back(&later, &state).unwrap();
        let mut s = BitReadStream::new(BitReadBuffer::new(back[0].bytes(), LittleEndian));
        let t = MessageType::read(&mut s).unwrap();
        let msg = Message::from_type(t, &mut s, &state).unwrap();
        later.note(&[msg]);
        assert_eq!(value_of(&later, 0), value_of(&earlier, 0), "alice again");
    }
}
