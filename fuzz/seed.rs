//! Writes starting inputs from the fixtures, from the repository's root:
//! `RUSTFLAGS='--cfg fuzzing' cargo +nightly run --manifest-path fuzz/Cargo.toml --bin seed`

fn main() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
    let bytes = std::fs::read(format!("{root}/tests/fixtures/bots_cp_badlands.cap")).unwrap();
    let (channel, decoder) = stvtf::fuzzing::seeds(&bytes);
    for (target, inputs) in [("relay_hub", channel.clone()), ("channel", channel), ("decoder", decoder)] {
        let dir = format!("{root}/fuzz/corpus/{target}");
        std::fs::create_dir_all(&dir).unwrap();
        for (n, input) in inputs.iter().enumerate() {
            std::fs::write(format!("{dir}/seed_{n}"), input).unwrap();
        }
    }
}
