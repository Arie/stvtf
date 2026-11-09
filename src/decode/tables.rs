//! The string tables as the frames so far left them, as far as a jump back needs: which entries
//! each table has, and the values of those whose later ones a client must not keep. Those are
//! `userinfo` (who is in each player slot) and `instancebaseline` (what an entity of each class
//! starts from: the full updates are written against the decoder's); the rest only grow, or do not
//! matter at an earlier moment (the lights, which models are loaded).
//!
//! A client that has the tables of a later moment is set back with `svc_UpdateStringTable`s of the
//! entries that differ ([`Tables::set_back`]).

use crate::net::bits::{BitBuf, BitReader, BitWriter};
use crate::net::messages::TYPE_BITS;
use bitbuffer::{BitWriteStream, LittleEndian};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use tf_demo_parser::ParserState;
use tf_demo_parser::demo::message::Message;
use tf_demo_parser::demo::message::stringtable::UpdateStringTableMessage;
use tf_demo_parser::demo::packet::stringtable::{ExtraData, StringTableEntry};
use tf_demo_parser::demo::parser::Encode;

const SVC_UPDATESTRINGTABLE: u32 = 13;

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

/// Sprites a beam can be drawn with, best first, as `modelprecache` names them.
const BEAM_SPRITES: [&str; 3] = ["sprites/laserbeam.vmt", "sprites/physbeam.vmt", "sprites/laser.vmt"];

#[derive(Default)]
pub(crate) struct Tables {
    /// By table id.
    by_id: Vec<Table>,
    /// How many updates rewrote an entry of a kept table.
    rewritten: u32,
    /// The best of [`BEAM_SPRITES`] loaded: its rank there and its model index.
    beam_sprite: Option<(usize, u16)>,
}

impl Tables {
    /// Takes the string table messages among a stream's.
    pub(crate) fn note(&mut self, messages: &[Message<'_>]) {
        for msg in messages {
            match msg {
                Message::CreateStringTable(t) => {
                    let name = t.table.name.to_string();
                    if name == "modelprecache" {
                        for (i, e) in &t.table.entries {
                            self.note_model(*i, e.text.as_deref());
                        }
                    }
                    let values = kept(&name).then(|| t.table.entries.iter().map(|(i, e)| (*i, value(e))).collect());
                    let entries = t.table.entries.iter().map(|(i, _)| *i).collect();
                    self.by_id.push(Table { name, max: t.table.max_entries, entries, values });
                }
                Message::UpdateStringTable(u) => {
                    if self.by_id.get(usize::from(u.table_id)).is_some_and(|t| t.name == "modelprecache") {
                        let models: Vec<(u16, Option<&str>)> =
                            u.entries.iter().map(|(i, e)| (*i, e.text.as_deref())).collect();
                        for (i, text) in models {
                            self.note_model(i, text);
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
                        if let Some(d) = &e.extra_data {
                            v.data = Some((d.to_owned(), data_bits(d)));
                        }
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
        let Some(rank) = name.and_then(|n| BEAM_SPRITES.iter().position(|s| n.eq_ignore_ascii_case(s))) else { return };
        if self.beam_sprite.is_none_or(|(r, _)| rank < r) {
            self.beam_sprite = Some((rank, index));
        }
    }

    /// The model index of a sprite to draw beams with, if one is loaded.
    pub(crate) fn beam_sprite(&self) -> Option<u16> {
        self.beam_sprite.map(|(_, i)| i)
    }

    /// The model index to draw beams with: the solid laser, loaded by the match or else by an
    /// `svc_UpdateStringTable` (also given) putting it in `modelprecache`'s last slot, which
    /// servers leave free; None when that slot is taken or the table unknown.
    pub(crate) fn laser(&self, state: &ParserState) -> Option<(u16, Option<BitBuf>)> {
        if let Some((0, index)) = self.beam_sprite {
            return Some((index, None));
        }
        let (id, table) = self.by_id.iter().enumerate().find(|(_, t)| t.name == "modelprecache")?;
        let slot = table.max.checked_sub(1)?;
        if table.entries.contains(&slot) {
            return None;
        }
        let entry = StringTableEntry { text: Some(Cow::Borrowed(BEAM_SPRITES[0])), extra_data: None };
        let msg = UpdateStringTableMessage { entries: vec![(slot, entry)], table_id: u8::try_from(id).ok()? };
        let mut bytes = Vec::new();
        let nbits = {
            let mut s = BitWriteStream::new(&mut bytes, LittleEndian);
            msg.encode(&mut s, state).ok()?;
            s.bit_len()
        };
        let mut w = BitWriter::new();
        w.write_bits(SVC_UPDATESTRINGTABLE, TYPE_BITS);
        w.write_buf(&BitReader::slice(&bytes, 0..nbits));
        Some((slot, Some(w.finish())))
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
            let mut bytes = Vec::new();
            let nbits = {
                let mut s = BitWriteStream::new(&mut bytes, LittleEndian);
                msg.encode(&mut s, state).ok()?;
                s.bit_len()
            };
            let mut w = BitWriter::new();
            w.write_bits(SVC_UPDATESTRINGTABLE, TYPE_BITS);
            w.write_buf(&BitReader::slice(&bytes, 0..nbits));
            out.push(w.finish());
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
    use bitbuffer::{BitRead, BitReadBuffer, BitReadStream};
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
}
