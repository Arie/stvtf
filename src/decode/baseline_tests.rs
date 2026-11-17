use std::borrow::Cow;

use bitbuffer::{BitRead, BitReadBuffer, BitReadStream, LittleEndian};
use tf_demo_parser::demo::message::stringtable::{CreateStringTableMessage, UpdateStringTableMessage};
use tf_demo_parser::demo::message::{Message, MessageType};
use tf_demo_parser::demo::packet::stringtable::{ExtraData, StringTable, StringTableEntry};

use super::decoder::{Decoder, encode_message};
use super::for_tests::tests_tables;
use crate::net::bits::BitBuf;
use crate::net::messages::{SVC_CREATESTRINGTABLE, SVC_UPDATESTRINGTABLE};

fn entry(class: &str, data: u8) -> StringTableEntry<'static> {
    let data = BitReadStream::new(BitReadBuffer::new_owned(vec![data], LittleEndian));
    StringTableEntry { text: Some(Cow::Owned(class.into())), extra_data: Some(ExtraData::new(data)) }
}

/// Creates `instancebaseline` with 42 entries.
fn created(dec: &mut Decoder) -> BitBuf {
    let table = StringTable {
        name: "instancebaseline".into(),
        entries: (0..42).map(|i| (i, entry(&i.to_string(), 0))).collect(),
        max_entries: 1024,
        fixed_user_data_size: None,
        client_entries: None,
        compressed: false,
    };
    let msg = CreateStringTableMessage { table };
    let bits = encode_message(SVC_CREATESTRINGTABLE, &msg, dec.parser_state_for_tests()).unwrap();
    let life = dec.feed(bits.bytes(), bits.nbits());
    assert!(life.stream.is_none(), "in order: as it came");
    bits
}

/// An update's stream as passed on.
fn update(dec: &mut Decoder, entries: Vec<(u16, StringTableEntry<'static>)>) -> BitBuf {
    let msg = UpdateStringTableMessage { entries, table_id: 0 };
    let bits = encode_message(SVC_UPDATESTRINGTABLE, &msg, dec.parser_state_for_tests()).unwrap();
    let life = dec.feed(bits.bytes(), bits.nbits());
    life.bits(&bits).clone()
}

fn indices(dec: &Decoder, bits: &BitBuf) -> Vec<u16> {
    let mut s = BitReadStream::new(BitReadBuffer::new(bits.bytes(), LittleEndian));
    let t = MessageType::read(&mut s).unwrap();
    let Message::UpdateStringTable(u) = Message::from_type(t, &mut s, dec.parser_state_for_tests()).unwrap() else {
        panic!()
    };
    u.entries.iter().map(|(i, _)| *i).collect()
}

/// An entry past the count goes at the client's end; later ones follow it.
#[test]
fn baselines_go_where_a_game_client_puts_them() {
    let mut dec = Decoder::new(&tests_tables(), 0.0).unwrap();
    created(&mut dec);

    let gap = update(&mut dec, vec![(39, entry("234", 1)), (54, entry("88", 1))]);
    assert_eq!(indices(&dec, &gap), [39, 42], "where the client puts it");
    let skipped = update(&mut dec, vec![(42, entry("89", 1))]);
    assert_eq!(indices(&dec, &skipped), [43], "after it");
    let again = update(&mut dec, vec![(54, entry("88", 2)), (55, entry("90", 2))]);
    assert_eq!(indices(&dec, &again), [42, 44]);
    let names = dec.table_names_for_tests("instancebaseline").unwrap();
    let at = |i: u16| names.get(&i).cloned().flatten();
    assert_eq!((at(42), at(43), at(44)), (Some("88".into()), Some("89".into()), Some("90".into())));
}

/// A checkpoint's placed streams, then the demo's own.
#[test]
fn a_decoder_fed_placed_streams_goes_on_placing_the_servers() {
    let mut dec = Decoder::new(&tests_tables(), 0.0).unwrap();
    let mut passed = vec![created(&mut dec)];
    passed.push(update(&mut dec, vec![(54, entry("88", 1))]));
    passed.push(update(&mut dec, vec![(42, entry("89", 1))]));
    let mut later = Decoder::new(&tests_tables(), 0.0).unwrap();
    for bits in &passed {
        later.feed(bits.bytes(), bits.nbits());
    }
    later.set_placed(dec.placed());
    let again = update(&mut later, vec![(54, entry("88", 2)), (42, entry("89", 2))]);
    assert_eq!(indices(&later, &again), [42, 43]);
}
