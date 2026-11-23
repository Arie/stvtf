# Development

## How the code is laid out

- `src/net`: the Source 2013 netchannel client (protocol 24): anonymous handshake, header and
  checksum, split packets, Snappy and LZSS compression, reliable reassembly, signon messages. The
  server is spoken to on whichever address answers the challenge: one on the same host behind
  Docker answers from the bridge gateway.
- `src/session.rs`: the signon state machine, reliable send/ack, tick acks every 100 ms, the
  silence timeout (15 s) and in-place map changes. The same driver runs live and over captures. A
  reliable message goes again only when the server's ack shows it lost, never on a timer.
- `src/watch.rs`: the loop around a session: which build to offer, reconnecting, stopping.
- `src/decode`: [tf-demo-parser](https://codeberg.org/demostf/parser) parses each message stream
  with the same-build send tables; `extract/` turns its game state into the JSON of
  [output.md](output.md).
- `src/output`: the types of every JSON line.
- `src/relay`: the relay ([relay.md](relay.md)); `relay/play` plays demos.
- `src/demo`: reading and recording `.dem` files.

## Checks

Install Rust with [rustup](https://rustup.rs). `rust-toolchain.toml` picks stable with `rustfmt`
and `clippy`; older Rust versions aren't supported. CI (`.github/workflows/ci.yml`) runs:

```
cargo +nightly fmt --all --check                           # format (rustfmt.toml; nightly for import grouping)
cargo clippy --all-targets --locked -- -D warnings         # lint (lint set in Cargo.toml [lints])
cargo test --locked                                        # tests
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --locked    # the library's docs build
cargo deny check                                           # advisories and licenses (cargo install cargo-deny)
```

`ln -s ../../.githooks/pre-commit .git/hooks/pre-commit` runs fmt and clippy before each commit
(not `core.hooksPath`, which would replace any global git hooks). Dependabot opens weekly update
pull requests.

## Tests

Unit tests cover the codec and protocol layers. The rest, all run by `cargo test`:

| file | what it does |
|---|---|
| `tests/captures.rs` | replays SourceTV traffic from a bot server (`tests/fixtures/*.cap`, made with `connect --record`) through the session and decoder, including a map change |
| `tests/golden.rs` | compares a whole replay with its recorded output (`bots_cp_badlands.jsonl.gz`) |
| `tests/payload.rs` | the start of a real pl_upward match (`tests/fixtures/pl_upward.dem`) |
| `tests/flags.rs` | a bball match (`tests/fixtures/ctf_ballin_sky.dem`) |
| `tests/detail.rs` | also a ctf_turbine match (`tests/fixtures/ctf_turbine.dem`), for votes and medic defences |
| `tests/modes.rs` | the starts of pass time, robot and player destruction, special delivery, Mann vs. Machine and territorial control matches |
| `tests/build_follow.rs` | which build a watch offers, against a fake SourceTV that refuses every connect |
| `tests/relay.rs` | clients connected to a relay of a capture |
| `tests/play.rs` | clients connected to a relay playing a demo |
| `tests/cli.rs` | the program itself, from a made-up relay password to `connect --record` and a replay of the recording |
| `tests/sizes.rs` | no source file has more than 600 lines of code |
| `src/recovery.rs` | loses packets on purpose against a fake server that answers requests for full updates |
| `src/hostile.rs` | feeds every parser seeded garbage and corrupted traffic |

Two need something outside the repository, so they are ignored unless asked for:

- `tests/live.rs` checks what recordings cannot show, against a real TF2 server with bots in a
  throwaway Docker container: `cargo test --test live -- --ignored --nocapture`.
  `STVTF_LIVE_UPDATE=1` updates to the current TF2 build, `STVTF_LIVE_IMAGE` picks the image and
  `STVTF_LIVE_SOAK` the seconds it watches (60).
- `tests/smoke.rs` decodes a directory of demos:
  `STVTF_SMOKE_DIR=~/demos cargo test --release --test smoke -- --ignored --nocapture`.

## Fuzzing

- `src/hostile.rs`: `STVTF_FUZZ_ROUNDS=100` multiplies how much it tries and `STVTF_FUZZ_SEED=N`
  picks other inputs. CI does both weekly.
- `fuzz/` holds coverage-guided fuzz targets for the same parsers (`channel`, `client_messages`,
  `decoder`, `out_of_band`, `relay_hub`). CI runs them weekly too and keeps a crash's input as a
  build artifact. To run one, with nightly Rust and `cargo install cargo-fuzz`:

```
RUSTFLAGS='--cfg fuzzing' cargo +nightly run --manifest-path fuzz/Cargo.toml --bin seed   # the starting inputs
cargo +nightly fuzz run channel
```

## Tools

- `STVTF_DEBUG` makes the extractor print what it sees: a server class or temp entity class by
  name, `ent:N`, `gameevents`, `convars` or `errors`.
- `cargo llvm-cov` measures coverage.
- `cargo run --example cut_demo -- IN.dem OUT.dem N` cuts a demo after its tick N, counted from the
  demo's start, to make a fixture.
- `cargo run --example add_build -- DEMO BUILD` compiles a TF2 build in (see
  [builds.md](builds.md)).
