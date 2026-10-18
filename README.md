# stvmap

A headless SourceTV spectator for Team Fortress 2. It connects to any STV port
like a normal spectator and prints what a top-down match overview needs as JSON
lines: periodic snapshots (players, projectiles, buildings, objectives) and
discrete events (kills, damage, shots, explosions, ubers, captures, rounds, chat).

It needs no plugin on the game server. Everything it sees is what SourceTV
broadcasts, so it is already delayed by the server's `tv_delay`; nothing live
leaves the server.

```
stvmap connect HOST:PORT [--password tv] [--hz 20] [--seconds N] [--record FILE.cap]
                 [--name stvmap] [--tables SAME_BUILD.dem] [--crc HEX] [--build N]
stvmap replay  FILE.cap [--hz 20] [--tables SAME_BUILD.dem] [--build N]
stvmap demo    FILE.dem [--hz 20]
```

For example, `STVMAP_PASSWORD=tv stvmap connect 1.2.3.4:27020 > match.jsonl`.

- `HOST:PORT` is the SourceTV port (`tv_port`), not the game port.
- `--password` is the server's `tv_password`. Prefer the `STVMAP_PASSWORD` environment
  variable: other local users can read command-line arguments (`ps`).
- Flags can be written `--hz 10` or `--hz=10`; an unknown flag is an error. `--help` prints the usage.
- `--hz` caps the snapshot rate (default 20 per second, 0 disables snapshots). Events are never rate limited.
- `--record` saves the raw inbound datagrams to a new file (it refuses to overwrite one); `replay`
  decodes such a capture offline through the same code.
- `demo` runs a recorded `.dem` through the same extractor.

The process reconnects by itself (with backoff) when the server drops it and
follows map changes on the same connection. It exits with an error when
reconnecting cannot help: a wrong password or class tables of another build.
Ctrl-C, or closing its output (`| head`), disconnects cleanly (otherwise SourceTV
holds the slot for 300 s); a second Ctrl-C exits at once.

The same loop is available as a library (`stvmap::watch`), which
[stvmap-web](../stvmap-web) uses for a live browser view. `watch` returns a
`WatchError` that tells a rejection (`Rejected { kind: Fatal::BadPassword, .. }`)
apart from other failures.

Names, chat and other strings in the output come from the game server and its
players. They are valid JSON, but can hold any Unicode, such as right-to-left
overrides and zero-width characters: escape them before showing them in a
terminal or a web page.

## TF2 builds: tables and CRC

A SourceTV client receives no entity class tables over the network: the server
expects the client to build them from its own game DLL and checks that by a
SendTable CRC sent during signon. Both are compiled in for the current TF2 build
(`CURRENT_BUILD` and `builtin` in `src/lib.rs`, tables in `assets/`), so nothing
needs to be passed. The build is read from the STV port's A2S_INFO reply and
falls back to the current build when the server does not answer.

For another build, pass:

1. `--tables`: any STV or POV demo recorded on that build.
2. `--crc`: that build's SendTable CRC. A wrong CRC makes the server drop the
   client with "Server uses different class tables".

To get the CRC of a new build, start that build's dedicated server locally and
read `g_SendTableCRC` from `engine_srv.so` (symbol offset via `readelf -sW`), as
`tools/read_crc.py` in [stv-watch](https://github.com/dmitry-ra/stv-watch) does.
To add the build, cut a demo of it after its data tables (the first `DataTables`
packet; everything after it is unused) into `assets/tables_BUILD.dem` and add a
`builtin` arm.

## Output

One JSON object per line. Every object has `type` and `tick` (the server tick,
66.67 per second, restarting at each map load). Live output (`connect`) also
has `time`: wall-clock receive time in milliseconds since the Unix epoch. Since
STV delays everything, `time` is roughly `tv_delay` after the moment in the game.

Units: positions are Hammer units `[x, y, z]`, angles are degrees, `yaw` 0 = +x
and counter-clockwise. Players are identified by their server `userid` (`id`),
which events refer to as `player`, `attacker`, `victim` and so on. Teams are
`"red"`, `"blue"`, `"spectator"` or `"none"`. Classes are lower-case names
(`"scout"` … `"engineer"`, `"unknown"`).

### Connection

```json
{"type":"status","state":"connecting","server":"1.2.3.4:27020","build":"11138802"}
{"type":"status","state":"new"}                      // signon progress: new, prespawn, spawn
{"type":"status","state":"full","map":"cp_badlands"}  // data flowing
{"type":"status","state":"changelevel"}               // the server announced a map change
{"type":"status","state":"map_change"}                // signon restarts for the new map; reset your view
{"type":"status","state":"disconnected","reason":"...","decode_errors":0,"packets":11935,"lost":0}
{"type":"status","state":"connect_failed","reason":"connect refused: #GameUI_ServerRejectServerFull"}
```

### Map

```json
{"type":"server","map":"cp_gullywash_f9","hostname":"SourceTV","tick_interval":0.015,"max_players":25,"spawn_count":3}
{"type":"world","min":[-14892.0,-4864.0,-14672.0],"max":[4096.0,15420.0,1008.0]}
```

`server` comes at every (re)signon; `world` once per map with the world bounds the
server sends (`m_WorldMins`/`m_WorldMaxs`). These include the map's 3D skybox, which
TF2 maps keep far outside the playable area, so they are too wide to fit an overview
image to; use per-map overview coordinates (or the extent of observed positions) for that.

### Snapshot

```json
{
  "type": "snapshot", "tick": 54210,
  "players": [{
    "id": 7, "ent": 6, "name": "Totally Not A Bot", "steamid": "BOT",
    "team": "blue", "class": "medic", "alive": true, "hp": 150, "max_hp": 150,
    "pos": [985.9, 110.5, 161.3], "yaw": 132.4, "pitch": 8.1,
    "uber": 87, "ping": 31, "conds": ["HealthOverHealed"], "ubered": null,
    "medigun": {"kind": "kritzkrieg", "releasing": false, "resist": null, "target": 4}
  }],
  "projectiles": [{"id": 378, "kind": "Sticky", "team": "blue", "pos": [-690.5, 638.5, -11.0], "yaw": 0.0, "owner": 11, "crit": false}],
  "buildings": [{
    "id": 492, "kind": "sentry", "team": "blue", "level": 1, "hp": 150, "max_hp": 150,
    "pos": [-319.8, -4231.9, 192.0], "yaw": 90.0, "builder": 5, "sapped": false,
    "mini": false, "building": false
  }],
  "objectives": [
    {"kind": "control_point", "index": 2, "pos": [-5632.0, 5248.0, 68.2], "owner": "none",
     "capping_team": "blue", "cap_progress": 0.49, "locked": false, "blocked": false},
    {"kind": "cart", "id": 51, "pos": [100.0, 200.0, 0.0], "team": "blue", "progress": 0.72, "cappers": 2, "speed": 2}
  ]
}
```

- Every snapshot also has `"round"` (`pregame`, `preround`, `running`, `team_win`, `stalemate`,
  `game_over`, …), `"score": {"red": n, "blue": n}` (rounds/points as on the scoreboard) and
  `"timers"`: the round timers shown in the HUD, `[{"team": "none"|"red"|"blue", "remaining": seconds,
  "paused": bool, "setup": bool}]` (one per team on King of the Hill). The round timer gets time
  added on captures; for a steady clock use `round_time` (seconds since the round went live,
  stopping at the win, null before it starts) or `match_time` (seconds since the match started).
  `time_left` is what remains of `mp_timelimit` (null without one), the clock TF2 shows when a map
  has no active round timer; `win_limit` is `mp_winlimit`.
- Snapshots are full state, not deltas. Dead players stay listed with `"alive": false`, at the
  `pos`/`yaw` where they died (the server's own position for a dead player is their spectator
  camera, which follows teammates); these are null if they died before the tool connected.
- `uber` is the medigun charge percentage. `ping` is the scoreboard ping the server reports (updated about every 2 s).
- `conds` are TF2 player conditions by name (`Invulnerable` for an über, `Stealthed` for cloak, `Disguised`, `Burning`, `CritBoosted`, `BlastJumping`, `Zoomed`, …).
- `ubered` is the medigun charge a player is under, or null: `uber`, `kritz`, `quickfix`,
  `vaccinator_bullet`, `vaccinator_blast` or `vaccinator_fire`. It covers the medic and whoever they charge.
- Medics carry `medigun`: `{"kind": "medigun"|"kritzkrieg"|"quickfix"|"vaccinator", "releasing": bool,
  "resist": "bullet"|"blast"|"fire" (vaccinator only), "target": player id being healed or null}`.
- Projectile `kind` is `Rocket`, `Pipe`, `Sticky`, `LooseCannon`, `Flare`, `HealingArrow` (crossbow),
  `Arrow` (huntsman), `EnergyBall` (Cow Mangler), `EnergyRing` (Bison, Pomson), `Fireball`
  (Dragon's Fury), `Cleaver`, `Jarate`, `MadMilk`, `GasPasser`, `ShortCircuitOrb` or `Unknown`;
  `yaw` is its rotation and `owner` the player who fired it.
- Teleporters carry `"entrance": true|false`; sentries carry `mini`, `building` (still constructing),
  `aim` (turret direction), `target` (player id) and `attacking`.
- Carts are only reported on payload maps (those with a train watcher; other maps use trains for
  lifts and props), with the pushing `team`, `progress` (0 → 1 along the track), `cappers` and
  `speed` (negative when rolling back).
- `cap_progress` runs 0 → 1 while `capping_team` caps the point, and is 0 otherwise. The
  server sends it only every few seconds; like TF2's own HUD the tool extrapolates it from the
  cap time and the number of cappers (decaying over 90 s when nobody is on the point), so it is
  current in every snapshot. `cap_rate` is the progress per second, to animate between snapshots.

### Events

| type | fields |
|---|---|
| `kill` | `victim`, `attacker` (null for world/suicide), `assister`, `weapon` (kill icon name), `crit`, `headshot`, `backstab`, `suicide`, `dominated`, `revenge`, `feign` (Dead Ringer: not a real death), `airborne` (victim was blast jumping), `victim_pos`, `attacker_pos` |
| `damage` | `victim`, `attacker`, `amount`, `health` (left), `crit`, `minicrit`, `weapon_id` |
| `shot` | `player`, `origin`, `pitch`, `yaw`, `weapon_id`: one per hitscan shot (bullets, shotgun, sniper, minigun) from TE_FireBullets |
| `explosion` | `origin`, `weapon_id`: rockets, pipes, stickies, … |
| `sentry_shot` | `sentry` (building id), `builder`, `target`, `shells`: a sentry fired (its shell count dropped) |
| `spawn` | `player`, `team`, `class`, `pos` (where they spawned: the event waits for the player's new position, which arrives after it) |
| `uber` | `medic`, `target`, `medigun` (kind, as above) |
| `uber_ready`, `uber_ended` | `medic`, `medigun`: the charge filled up / a released charge ran out |
| `medic_death` | `medic`, `attacker`, `healing` (this life), `drop` (died with a full charge) |
| `capture_blocked` | `point`, `name`, `blocker`, `victim` |
| `reflect` | `player` (the pyro), `owner` (of the projectile), `weapon_id` |
| `banner` | `player`, `kind` (`buff_banner`, `battalions_backup`, `concheror`) |
| `scores` | `red`, `blue`, `game_over`: the end-of-round panel |
| `stalemate`, `overtime` | (none) |
| `build` | `player`, `object` (`sentry`/`dispenser`/`teleporter`/`sapper`), `id` |
| `building_destroyed` | `owner`, `attacker`, `object`, `id`, `weapon`, `was_building` |
| `team_change` | `player`, `team`, `old_team`, `disconnect` |
| `class_change` | `player`, `class` |
| `player_leave` | `player`, `name`, `reason` |
| `round_start` | `full_reset` |
| `round_active`, `setup_finished` | (none) |
| `round_win` | `team`, `reason`, `round_time` |
| `point_captured` | `point` (index into the control points), `name`, `team`, `cappers` (player ids) |
| `game_over` | `reason` |
| `chat` | `player` (null for console), `name`, `text` (colour codes stripped), `kind` |

Projectiles appear and disappear between snapshots; a frontend that wants
explicit "fired" markers can diff the projectile ids of consecutive snapshots.

## How it works

- `src/net`: the Source 2013 netchannel client (protocol 24): the connectionless
  handshake (anonymous, no Steam login), packet header and checksum, `-2` split
  packets, `-3 SNAP` (Snappy) packets, reliable subchannel reassembly, and the
  client messages for signon.
- `src/session.rs`: the signon state machine (echoing each signon state, the
  ClientInfo with the CRC, self-promotion to FULL), reliable send/ack, tick acks
  every 100 ms (they move the server's delta reference forward), silence
  timeout and in-place map changes. The same driver runs live and over captures.
- `src/decode`: each received message stream is parsed with its exact bit length
  by [tf-demo-parser](https://crates.io/crates/tf-demo-parser) and fed to its
  `DemoHandler` with the send tables taken from the same-build demo.
  `extract.rs` turns the resulting game state into the JSON above.

The network layer is a port of [stv-watch](https://github.com/dmitry-ra/stv-watch)
(MIT), a SourceTV client for Half-Life 2: Deathmatch, adapted to TF2.

## Known limits

- One STV slot per connected instance. SourceTV counts the client as a proxy, so
  a client that vanishes without saying goodbye holds its slot for 300 s; the
  tool always sends a disconnect when it stops or reconnects.
- During the `tv_delay` window after a map change the server may drop the client
  or restart signon several times; the tool follows and settles once the delayed
  stream reaches the new map.
- Memory grows with the data of the current map (the raw message streams are kept
  in an arena for the parser) and is released at every map change: about 26 MB
  for a 3-minute 6v6, so a long map of a full server reaches a few hundred MB.
  Past 1 GiB (`Options::max_decoder_bytes`) the tool reconnects to start afresh,
  so a map that never ends cannot exhaust memory.
- SourceTV keeps an exploded projectile's entity around, unchanging, for up to seconds before
  deleting it. A game client hides it at the explosion; so does the tool, dropping the nearest
  rocket (rocket launcher explosions) or grenade (others) within 150 units of each `explosion`.
- `explosion` and `shot` come from temp entities. Like the engine, the tool fills a
  property left out of the message from the previous event of the same class in
  that message, or with 0.
- Hitscan `shot` events come from TE_FireBullets; melee swings and flamethrower
  particles have no such event.

## Development

Install Rust with [rustup](https://rustup.rs); `rust-toolchain.toml` picks the
stable toolchain with `rustfmt` and `clippy`. The minimum supported Rust version
is set by `rust-version` in `Cargo.toml` (1.88).

Every push and pull request runs these checks in GitHub Actions
(`.github/workflows/ci.yml`); run them locally before pushing:

```
cargo fmt                                         # format (settings in rustfmt.toml)
cargo clippy --all-targets -- -D warnings         # lint (lint set in Cargo.toml [lints])
cargo test --release                              # tests
cargo deny check                                  # advisories and licenses (cargo install cargo-deny)
```

To run the format and lint checks before every commit:

```
git config core.hooksPath .githooks
```

Dependabot opens weekly pull requests for crate and GitHub Actions updates.

## Tests

```
cargo test --release
```

Unit tests cover the bit codec, compression, checksum/header, split and reliable
reassembly, handshake and client message layouts. The integration tests in
`tests/captures.rs` replay real SourceTV traffic recorded from a TF2 server with
bots (`tests/fixtures/*.cap`, recorded with `connect --record`) through the full
session and decoder, including a map change followed in place. `tests/payload.rs`
runs the start of a real pl_upward match (`tests/fixtures/pl_upward.dem`) through the
demo path: setup, the cart pushed and rolling back, and the first capture.

`cargo run --example cut_demo -- IN.dem OUT.dem N` cuts a demo after demo tick N
(counted from the start of the recording), to make a fixture of a long recording.
