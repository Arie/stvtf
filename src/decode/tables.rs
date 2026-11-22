//! The string tables as the frames so far left them, as far as a jump back needs: which entries
//! each table has, and the entry values of the two tables where a client must not keep later
//! values. Those are `userinfo` (who is in each player slot) and `instancebaseline` (what an entity
//! of each class starts from; full updates are written against the decoder's baselines). The other
//! tables only grow, or do not matter at an earlier moment (the lights, which models are loaded).
//!
//! A client that has the tables of a later moment is set back with `svc_UpdateStringTable`s of the
//! entries that differ ([`Tables::set_back`]).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};

use tf_demo_parser::ParserState;
use tf_demo_parser::demo::message::Message;
use tf_demo_parser::demo::message::stringtable::UpdateStringTableMessage;
use tf_demo_parser::demo::packet::stringtable::{ExtraData, StringTable, StringTableEntry};

use crate::net::bits::BitBuf;
use crate::net::messages::SVC_UPDATESTRINGTABLE;

/// The most entries put in one message of a snapshot: well within the message's length limit.
const SNAPSHOT_ENTRIES: usize = 64;

/// The tables whose values are kept.
fn kept(name: &str) -> bool {
    matches!(name, "userinfo" | "instancebaseline")
}

/// An entry's value: its text, and its data with the data's bytes, which are what is compared.
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

/// What the clients' `modelprecache` holds since their signon, which a jump back does not remove:
/// how many entries, and the index of the match's own laser.
#[derive(Copy, Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
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

    /// These combined with what `tables` hold, which the clients were sent too.
    pub(super) fn with_tables(self, tables: &Tables) -> ModelsSent {
        ModelsSent { next: self.next.max(tables.models), laser: self.laser.or(tables.match_laser()) }
    }
}

/// The model index to draw beams with. If the clients do not have that model, also the message
/// that loads it, which must be sent first.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BeamModel {
    pub(crate) index: u16,
    pub(crate) load: Option<BitBuf>,
}

/// Where a game client puts each `instancebaseline` entry, by server index. An entry with an
/// index past the end of the client's table goes at its end. Baselines are found by name, so
/// moving them is safe.
#[derive(Clone, Debug, Default)]
pub(crate) struct Placed {
    table: Option<u8>,
    at: HashMap<u16, u16>,
    next: u16,
}

impl Placed {
    fn place(&mut self, i: u16) -> u16 {
        *self.at.entry(i).or_insert_with(|| {
            self.next += 1;
            self.next - 1
        })
    }
}

#[derive(Default)]
pub(crate) struct Tables {
    /// By table id.
    by_id: Vec<Table>,
    placed: Placed,
    /// How many updates rewrote an entry of a kept table.
    rewritten: u32,
    /// The best of [`BEAM_SPRITES`] loaded: its rank there and its model index.
    beam_sprite: Option<(usize, u16)>,
    /// One past the last entry of `modelprecache`.
    models: u16,
    /// The lowest index of a loaded `.mdl` model (not part of the map, not a sprite).
    studio: Option<u16>,
}

impl Tables {
    /// Renumbers `instancebaseline` entries as a game client places them. Returns the indexes of
    /// the updates in which an entry moved.
    pub(crate) fn place(&mut self, messages: &mut [Message<'_>]) -> Vec<usize> {
        let mut changed = Vec::new();
        let mut id = self.by_id.len();
        for (k, msg) in messages.iter_mut().enumerate() {
            match msg {
                Message::CreateStringTable(t) => {
                    if t.table.name == "instancebaseline" {
                        self.placed = Placed { table: u8::try_from(id).ok(), ..Placed::default() };
                        for (i, _) in &mut t.table.entries {
                            *i = self.placed.place(*i);
                        }
                    }
                    id += 1;
                }
                Message::UpdateStringTable(u) if Some(u.table_id) == self.placed.table => {
                    let mut moved = false;
                    for (i, _) in &mut u.entries {
                        let to = self.placed.place(*i);
                        moved |= to != *i;
                        *i = to;
                    }
                    if moved {
                        changed.push(k);
                    }
                }
                _ => {}
            }
        }
        changed
    }

    /// Where the `instancebaseline` entries were placed so far.
    pub(crate) fn placed(&self) -> Placed {
        self.placed.clone()
    }

    /// Sets the placement, for a decoder fed streams that were already placed.
    pub(crate) fn set_placed(&mut self, placed: Placed) {
        self.placed = placed;
    }

    /// Records the string table messages among a stream's `messages`.
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
        if name.and_then(|n| n.rsplit_once('.')).is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("mdl"))
            && self.studio.is_none_or(|i| index < i)
        {
            self.studio = Some(index);
        }
        let Some(rank) = name.and_then(|n| BEAM_SPRITES.iter().position(|s| n.eq_ignore_ascii_case(s))) else { return };
        if self.beam_sprite.is_none_or(|(r, _)| rank < r) {
            self.beam_sprite = Some((rank, index));
        }
    }

    /// The model index of a sprite to draw beams with, if one is loaded.
    pub(crate) fn beam_sprite(&self) -> Option<u16> {
        self.beam_sprite.map(|(_, i)| i)
    }

    /// The model index of some prop's or player's model, if one is loaded.
    pub(crate) fn studio(&self) -> Option<u16> {
        self.studio
    }

    /// The model index of the match's own laser, if the match loaded it.
    pub(crate) fn match_laser(&self) -> Option<u16> {
        self.beam_sprite.filter(|(rank, _)| *rank == 0).map(|(_, i)| i)
    }

    /// Whether the match loaded the laser after the clients were given the relay's at `before`: a
    /// game client has the name already and adds nothing, so every model the match loads after it
    /// is one index off for them until they sign on again.
    pub(crate) fn laser_loaded_twice(&self, before: Option<u16>) -> bool {
        before.is_some_and(|b| self.match_laser().is_some_and(|m| m != b))
    }

    /// The model index to draw beams with: the solid laser. If the clients do not have it yet,
    /// also the `svc_UpdateStringTable` that adds it to `modelprecache`. A game client puts an
    /// entry past its last one at the end, whatever its index, so the laser goes in the next free
    /// index: past everything the clients were `sent`, which after a jump back is more than these
    /// tables have. `before` is the index where the clients were given it. They keep it there
    /// until they sign on again, whatever the match loads there later (for an index it has, a game
    /// client takes only the data), and a jump back leaves it too. None when the table is full or
    /// unknown.
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
        Some(BeamModel { index: slot, load: Some(super::decoder::encode_message(SVC_UPDATESTRINGTABLE, &msg, state)?) })
    }

    #[cfg(test)]
    pub(crate) fn names_for_tests(&self, name: &str) -> Option<BTreeMap<u16, Option<String>>> {
        let t = self.by_id.iter().find(|t| t.name == name)?;
        Some(t.values.as_ref()?.iter().map(|(i, v)| (*i, v.text.clone())).collect())
    }

    /// How many entries of `userinfo` and `instancebaseline` were rewritten so far. A client with
    /// the tables of a later moment has wrong entries for this moment only if the count grew since.
    pub(crate) fn rewritten(&self) -> u32 {
        self.rewritten
    }

    /// `svc_UpdateStringTable`s that bring a client with these tables to `snapshot`, a demo's
    /// tables as its recording started: the entries they lack, and those of the kept tables whose
    /// values differ. A player's demo starts well after its signon.
    pub(crate) fn up_to(&self, snapshot: &[StringTable<'_>], state: &ParserState) -> Vec<BitBuf> {
        let mut out = Vec::new();
        for (id, (ours, theirs)) in self.by_id.iter().zip(snapshot).enumerate() {
            let Ok(table_id) = u8::try_from(id) else { break };
            if ours.name != theirs.name {
                continue;
            }
            let differs = |i: &u16, e: &StringTableEntry<'_>| {
                !ours.entries.contains(i)
                    || ours.values.as_ref().is_some_and(|v| v.get(i).is_none_or(|w| !w.same(&value(e))))
            };
            let entries: Vec<_> = theirs.entries.iter().filter(|(i, e)| differs(i, e)).cloned().collect();
            for some in entries.chunks(SNAPSHOT_ENTRIES) {
                let msg = UpdateStringTableMessage { entries: some.to_vec(), table_id };
                out.extend(super::decoder::encode_message(SVC_UPDATESTRINGTABLE, &msg, state));
            }
        }
        out
    }

    /// `svc_UpdateStringTable`s that set a client with the tables of `later` back to these: the
    /// entries whose values differ, as they are here. Entries only `later` has are left alone: a
    /// slot or class not used yet. None if one does not encode, or the tables are not the same
    /// ones.
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
            out.push(super::decoder::encode_message(SVC_UPDATESTRINGTABLE, &msg, state)?);
        }
        Some(out)
    }
}

/// A `userinfo` entry's data is the engine's `player_info_t`: the name, the user id, then the
/// `SteamID` as text.
const NAME_BYTES: usize = 32;
const GUID_AT: usize = NAME_BYTES + 4;
const GUID_BYTES: usize = 33;

/// The text up to its NUL.
fn c_str(bytes: &[u8]) -> &[u8] {
    &bytes[..bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len())]
}

impl Tables {
    /// An `svc_UpdateStringTable` of `userinfo` that gives each player in it the name `names` has
    /// for their `SteamID` (`None` gives back the game's own name), cut to the length the game
    /// takes. None when none of them is in the table.
    pub(crate) fn named(&self, names: &BTreeMap<String, Option<String>>, state: &ParserState) -> Option<BitBuf> {
        let (id, table) = self.by_id.iter().enumerate().find(|(_, t)| t.name == "userinfo")?;
        let mut entries = Vec::new();
        for (i, v) in table.values.as_ref()? {
            let Some((own, bytes)) = &v.data else { continue };
            let Some(guid) = bytes.get(GUID_AT..GUID_AT + GUID_BYTES).map(c_str) else { continue };
            let Some(name) = std::str::from_utf8(guid).ok().and_then(|g| names.get(g)) else { continue };
            let data = match name {
                None => own.clone(),
                Some(name) => {
                    let mut bytes = bytes.clone();
                    let mut end = name.len().min(NAME_BYTES - 1);
                    while !name.is_char_boundary(end) {
                        end -= 1;
                    }
                    bytes[..NAME_BYTES].fill(0);
                    bytes[..end].copy_from_slice(&name.as_bytes()[..end]);
                    ExtraData::new(bitbuffer::BitReadStream::new(bitbuffer::BitReadBuffer::new_owned(
                        bytes,
                        bitbuffer::LittleEndian,
                    )))
                }
            };
            entries.push((*i, StringTableEntry { text: v.text.clone().map(Cow::Owned), extra_data: Some(data) }));
        }
        if entries.is_empty() {
            return None;
        }
        let msg = UpdateStringTableMessage { entries, table_id: u8::try_from(id).ok()? };
        super::decoder::encode_message(SVC_UPDATESTRINGTABLE, &msg, state)
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
    use bitbuffer::{BitRead, BitReadBuffer, BitReadStream, LittleEndian};
    use tf_demo_parser::demo::message::MessageType;
    use tf_demo_parser::demo::message::stringtable::{CreateStringTableMessage, StringTableMeta};

    use super::*;

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

    /// A `player_info_t` of `name` and `steamid`.
    fn player_info(name: &str, steamid: &str) -> Vec<u8> {
        let mut info = vec![0u8; 140];
        info[..name.len()].copy_from_slice(name.as_bytes());
        info[GUID_AT..GUID_AT + steamid.len()].copy_from_slice(steamid.as_bytes());
        info[100] = 7;
        info
    }

    #[test]
    fn players_are_named_in_userinfo_by_steamid_and_given_their_own_name_back() {
        let mut state = ParserState::new(24, |_| true, false);
        state.string_tables.push(StringTableMeta { max_entries: 256, fixed_userdata_size: None });
        let created = userinfo(vec![
            (0, entry("0", &player_info("alice", "[U:1:5]"))),
            (1, entry("1", &player_info("bob", "[U:1:6]"))),
            (2, entry("2", &player_info("tv", "BOT"))),
        ]);
        let mut tables = Tables::default();
        tables.note(std::slice::from_ref(&created));
        let read = |msg: &BitBuf| {
            let mut s = BitReadStream::new(BitReadBuffer::new(msg.bytes(), LittleEndian));
            let t = MessageType::read(&mut s).unwrap();
            let Message::UpdateStringTable(u) = Message::from_type(t, &mut s, &state).unwrap() else { panic!() };
            u.entries.into_iter().map(|(i, e)| (i, data_bits(&e.extra_data.unwrap()))).collect::<Vec<_>>()
        };
        // Longer than the game takes: cut at a whole character.
        let long = format!("{}é", "x".repeat(30));
        let names =
            BTreeMap::from([("[U:1:6]".to_string(), Some(long)), ("[U:1:9]".to_string(), Some("nobody".into()))]);
        let told = read(&tables.named(&names, &state).unwrap());
        assert_eq!(told.len(), 1, "only who is in the match");
        let (slot, info) = &told[0];
        assert_eq!((*slot, c_str(&info[..NAME_BYTES])), (1, "x".repeat(30).as_bytes()));
        assert_eq!(info[NAME_BYTES..], player_info("bob", "[U:1:6]")[NAME_BYTES..], "the rest as it was");
        let own = read(&tables.named(&BTreeMap::from([("[U:1:6]".to_string(), None)]), &state).unwrap());
        assert_eq!(own, [(1, player_info("bob", "[U:1:6]"))]);
        assert!(tables.named(&BTreeMap::new(), &state).is_none());
    }
}
