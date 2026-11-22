use super::decoder::Decoder;
use super::send_tables::Tables;
use crate::demo::{Next, Packet, Reader};

#[test]
#[ignore = "a demo in STVTF_SPEED_DEMO"]
fn decoding_speed() {
    let path = std::env::var("STVTF_SPEED_DEMO").unwrap();
    let tables = Tables::from_demo(std::path::Path::new(&path)).unwrap();
    for world in [false, true] {
        let mut r = Reader::new(std::fs::File::open(&path).unwrap()).unwrap();
        let mut dec = Decoder::new(&tables, 0.0).unwrap();
        if world {
            dec.scene_mut().keep_world();
        }
        let t = std::time::Instant::now();
        let mut n = 0;
        while let Next::Packet(p) = r.next_packet().unwrap() {
            if let Packet::Signon { data, .. } | Packet::Message { data, .. } = p {
                dec.feed(&data, data.len() * 8);
                n += 1;
            }
        }
        eprintln!("world {world}: {n} packets in {:.1} s", t.elapsed().as_secs_f32());
    }
}
