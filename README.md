# stvtf

The SourceTV client behind [stv.tf](https://stv.tf): a headless spectator for Team
Fortress 2. It joins an STV port like any spectator and prints JSON lines for a top-down
overview: snapshots (players, projectiles, buildings, objectives) and events (kills, damage,
shots, ubers, captures, rounds, chat). It can also pass the server on to TF2 clients, as a
SourceTV relay. No server plugin is needed, and since SourceTV is delayed by `tv_delay`, nothing
live leaves the server.

| page | |
|---|---|
| [docs/output.md](docs/output.md) | every JSON line and field |
| [docs/relay.md](docs/relay.md) | relaying to TF2 clients, playing and recording demos |
| [docs/builds.md](docs/builds.md) | TF2 builds: class tables and the CRC |
| [docs/development.md](docs/development.md) | code layout, checks and tests |

## Usage

```
stvtf connect HOST:PORT [--password tv] [--hz 20] [--seconds N] [--record FILE.cap]
                [--name stv.tf] [--tables SAME_BUILD.dem] [--crc HEX] [--build N]
                [--relay 0.0.0.0:27121]
stvtf replay  FILE.cap [--hz 20] [--tables SAME_BUILD.dem] [--build N]
stvtf demo    FILE.dem [--hz 20]
stvtf relay   HOST:PORT|FILE.cap|FILE.dem [--listen 0.0.0.0:27020]
```

For example, `STVTF_PASSWORD=tv stvtf connect 1.2.3.4:27020 > match.jsonl`.

| command | |
|---|---|
| `connect` | watches a SourceTV server and prints its lines |
| `replay` | prints the lines of a capture made with `--record` |
| `demo` | prints the lines of a `.dem` |
| `relay` | only relays, and prints only status lines (see [docs/relay.md](docs/relay.md)) |

| flag | |
|---|---|
| `HOST:PORT` | the SourceTV port (`tv_port`). Given the game port, it tries game port + 5. |
| `--password` | the `tv_password`. Prefer `STVTF_PASSWORD`: other local users can read command-line arguments. |
| `--hz` | the most snapshots a second (default 20; 0 for none). See below. |
| `--seconds` | stops after this many seconds |
| `--record` | saves the raw datagrams to a new file, for `replay` |
| `--name` | the name it joins the server with (default `stv.tf`) |
| `--tables`, `--crc`, `--build` | for a TF2 build that is not built in (see [docs/builds.md](docs/builds.md)) |
| `--relay` | also lets TF2 clients watch through this address (see [docs/relay.md](docs/relay.md)) |

Flags take `--hz 10` or `--hz=10`; an unknown flag is an error. `--version` prints the version.
Log lines go to stderr; `RUST_LOG` sets their level (default `info`).

### Snapshot rate

Snapshots come at most `--hz` times a second, in whole ticks, and no more often than the STV
sends frames (`tv_snapshotrate`). The default of 20 gives one every 4 ticks, 16.7 a second on a
66-tick server:

| `tv_snapshotrate` | snapshots |
|---|---|
| 16 (the default) | every frame the STV sends |
| 66 (a competitive config) | every fourth frame |

Events are never rate limited. A relay passes on every frame, whatever `--hz` is.

### Connection

- It reconnects when dropped, waiting 2 s at first and up to 30 s, and follows map changes.
- A server silent for 15 s counts as dropped.
- It exits when reconnecting cannot help (wrong password, tables of another build).
- Ctrl-C, SIGTERM or closing its output disconnects cleanly, so SourceTV does not hold the slot
  for 300 s. A second signal exits at once.

### Names and chat

Names and chat come from players: valid JSON, but any Unicode (right-to-left overrides,
zero-width characters). Escape them before display.

## Library

The same loop is a library, `stvtf::watch`, which [stvtf-web](../stvtf-web), the stv.tf server,
uses.

- It hands each line to a callback as a `stvtf::Line`: an enum with a variant per line type.
- The types (in `stvtf::output`) serialize to exactly the JSON in
  [docs/output.md](docs/output.md) and read it back, so JSON is only written where it leaves the
  program.
- It returns a `WatchError` that tells a rejection (`Rejected { kind: Fatal::BadPassword, .. }`)
  apart from other failures.

## Known limits

- Each instance takes one STV slot. One that vanishes without a goodbye holds it for 300 s, so
  the tool always disconnects when it stops or reconnects.
- After a map change the server sends the tool a packet every 2 s until the delayed stream reaches
  the new map, `tv_delay` later; the tool waits through it.
- What the output leaves out is listed in [docs/output.md](docs/output.md#not-reported).

## Credits

- The network layer is a port of [stv-watch](https://github.com/dmitry-ra/stv-watch) by
  dmitry-ra (MIT), a SourceTV client for Half-Life 2: Deathmatch.
- Decoding is [tf-demo-parser](https://codeberg.org/demostf/parser) and
  [bitbuffer](https://crates.io/crates/bitbuffer) by Robin Appelman of
  [demos.tf](https://demos.tf) (MIT or Apache-2.0).
- From demos.tf: `tests/fixtures/pl_upward.dem` is cut from a serveme.tf match's STV demo,
  `tests/fixtures/ctf_ballin_sky.dem` is a bball match's STV demo and
  `tests/fixtures/ctf_turbine.dem` a ctf_turbine match's (demos.tf 1287498).
- The game-mode fixtures are the starts of STV demos from demos.tf, cut with `cut_demo`:
  `pass_stadium.dem` (pass time, demos.tf 1516963), `rd_asteroid.dem` (robot destruction,
  954995), `pd_watergate.dem` (player destruction, 340112), `sd_doomsday.dem` (special delivery,
  1232410), `mvm_crossing.dem` (Mann vs. Machine on mvm_gd_crossing_b3b, 1484401) and
  `tc_hydro.dem` (territorial control, 1503662).
