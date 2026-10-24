# stvtf

The SourceTV client behind [stv.tf](https://stv.tf): a headless spectator for Team
Fortress 2. It joins an STV port like any spectator and prints JSON lines for a top-down
overview: snapshots (players, projectiles, buildings, objectives) and events (kills, damage,
shots, ubers, captures, rounds, chat). No server plugin is needed, and since SourceTV is
delayed by `tv_delay`, nothing live leaves the server.

```
stvtf connect HOST:PORT [--password tv] [--hz 20] [--seconds N] [--record FILE.cap]
                [--name stv.tf] [--tables SAME_BUILD.dem] [--crc HEX] [--build N]
stvtf replay  FILE.cap [--hz 20] [--tables SAME_BUILD.dem] [--build N]
stvtf demo    FILE.dem [--hz 20]
```

For example, `STVTF_PASSWORD=tv stvtf connect 1.2.3.4:27020 > match.jsonl`.

- `HOST:PORT` is the SourceTV port (`tv_port`). Given the game port, it tries game port + 5.
- `--password` is the `tv_password`. Prefer `STVTF_PASSWORD`: other local users can read
  command-line arguments.
- `--hz` caps the snapshot rate (default 20; 0 for no snapshots). Snapshots come at most that often,
  in whole ticks, and no more often than the STV sends frames (`tv_snapshotrate`): the default is
  every 4 ticks, 16.7 a second on a 66-tick server, which takes every frame of an STV at the
  default `tv_snapshotrate 16`. Events are never rate limited.
- `--record` saves the raw datagrams to a new file; `replay` decodes it offline. `demo` runs a
  `.dem` through the same extractor.
- Flags take `--hz 10` or `--hz=10`; an unknown flag is an error.

It reconnects with backoff when dropped, follows map changes, and exits when reconnecting
cannot help (wrong password, tables of another build). Ctrl-C, SIGTERM or closing its output
disconnects cleanly, so SourceTV does not hold the slot for 300 s; a second Ctrl-C exits at once.

The same loop is a library, `stvtf::watch`, which [stvtf-web](../stvtf-web), the stv.tf server, uses. It
hands each line to a callback as a `stvtf::Line`: an enum with a variant per line type, whose types
(in `stvtf::output`) serialize to exactly the JSON below and read it back, so JSON is only written
where it leaves the program. It returns a `WatchError` that tells a rejection
(`Rejected { kind: Fatal::BadPassword, .. }`) apart from other failures.

Names and chat come from players: valid JSON, but any Unicode (right-to-left overrides,
zero-width characters). Escape them before display.

## TF2 builds: tables and CRC

SourceTV sends no class tables: the client builds them from its game DLL and proves it with
a SendTable CRC at signon. Both are compiled in for the current build (`CURRENT_BUILD` and
`builtin` in `src/lib.rs`, tables in `assets/`). The build comes from the STV port's A2S_INFO
reply, falling back to the current build. A reported build it has neither built in nor been
given `--tables` and `--crc` for also falls back (anyone can forge that reply), and is shown
as `server_build` in the `connecting` status; the server then refuses the class tables if it
really runs that build.

For another build, pass `--tables` (any STV or POV demo of that build) and `--crc` (its
SendTable CRC; a wrong one gets "Server uses different class tables"). To find the CRC, run
that build's dedicated server and read `g_SendTableCRC` from `engine_srv.so` (offset via
`readelf -sW`), as `tools/read_crc.py` in [stv-watch](https://github.com/dmitry-ra/stv-watch)
does. To compile a build in, cut a demo of it after its first `DataTables` packet into
`assets/tables_BUILD.dem` and add a `builtin` arm.

## Output

One JSON object per line, each with `type` and `tick` (66.67 per second, restarting at each
map load). Live output also has `time`: receive time in Unix milliseconds, about `tv_delay`
after the moment in the game.

Positions are Hammer units `[x, y, z]`; angles are degrees, `yaw` 0 = +x, counter-clockwise.
Players are identified by their `userid` (`id`), which events refer to as `player`,
`attacker`, `victim` and so on. Teams are `red`, `blue`, `spectator` or `none`; classes are
lower-case names (`scout` … `engineer`, `unknown`).

### Connection

```json
{"type":"status","state":"connecting","server":"1.2.3.4:27020","build":"11138802"}
{"type":"status","state":"new"}                      // signon progress: new, prespawn, spawn
{"type":"status","state":"full","map":"cp_badlands"}  // data flowing
{"type":"status","state":"changelevel"}               // the server announced a map change
{"type":"status","state":"map_change"}                // signon restarts for the new map; reset your view
{"type":"status","state":"disconnected","reason":"...","decode_errors":0,"packets":11935,"lost":0}
{"type":"status","state":"connect_failed","reason":"connect refused: #GameUI_ServerRejectServerFull"}
{"type":"status","state":"gave_up","reason":"..."}       // not from the CLI: a host of the library reporting watch's error
```

### Map

```json
{"type":"server","map":"cp_gullywash_f9","hostname":"SourceTV","tick_interval":0.015,"max_players":25,"spawn_count":3}
{"type":"world","min":[-14892.0,-4864.0,-14672.0],"max":[4096.0,15420.0,1008.0]}
```

`server` comes at every signon, `world` once per map. Its bounds include the 3D skybox, far
outside the playable area, so fit an overview image to per-map coordinates instead.

### Snapshot

```json
{
  "type": "snapshot", "tick": 54210,
  "players": [{
    "id": 7, "ent": 6, "name": "Totally Not A Bot", "steamid": "BOT",
    "team": "blue", "class": "medic", "alive": true, "hp": 150, "max_hp": 150,
    "pos": [985.9, 110.5, 161.3], "yaw": 132.4, "pitch": 8.1,
    "uber": 87, "ping": 31, "conds": ["HealthOverHealed"], "ubered": null,
    "medigun": {"kind": "kritzkrieg", "releasing": false, "resist": null, "target": 4},
    "weapon": {"item": 35, "clip": null}
  }],
  "projectiles": [{"id": 378, "kind": "Sticky", "team": "blue", "pos": [-690.5, 638.5, -11.0], "yaw": 0.0, "owner": 11, "crit": false}],
  "buildings": [{
    "id": 492, "kind": "sentry", "team": "blue", "level": 1, "hp": 150, "max_hp": 150,
    "pos": [-319.8, -4231.9, 192.0], "yaw": 90.0, "builder": 5, "sapped": false,
    "mini": false, "building": false
  }],
  "objectives": [
    {"kind": "control_point", "index": 2, "pos": [-5632.0, 5248.0, 68.2], "owner": "none",
     "capping_team": "blue", "cap_progress": 0.49, "locked": false, "blocked": false,
     "on_point": {"red": 0, "blue": 2}},
    {"kind": "cart", "id": 51, "pos": [100.0, 200.0, 0.0], "team": "blue", "progress": 0.72, "cappers": 2, "speed": 2},
    {"kind": "flag", "id": 84, "pos": [1428.0, 896.0, -211.0], "team": "red", "status": "carried", "carrier": 6, "returns_in": null}
  ]
}
```

- Snapshots are full state, not deltas. Each also has `round` (`pregame`, `preround`, `running`,
  `team_win`, `stalemate`, `game_over`, …), `score` (`{"red": n, "blue": n}`) and `timers`, the HUD
  round timers: `[{"team", "remaining", "paused", "setup"}]`, one per team on King of the Hill.
  Captures add time to those; for a steady clock use `round_time` (since the round went live, null
  before) or `match_time`. `time_left` is what remains of `mp_timelimit` (or null); `win_limit` is
  `mp_winlimit`.
- Dead players stay listed with `"alive": false` at the `pos`/`yaw` where they died (null if that
  was before the tool connected). So do players who joined a team but have not picked a class.
- `uber` is the charge percentage; `ping` the scoreboard ping (updated about every 2 s).
- `conds` are TF2 player conditions by name (`Invulnerable` for an über, `Stealthed` for cloak, `Disguised`, `Burning`, `CritBoosted`, `BlastJumping`, `Zoomed`, …).
- `ubered` is the medigun charge a player is under, or null: `uber`, `kritz`, `quickfix`,
  `vaccinator_bullet`, `vaccinator_blast` or `vaccinator_fire`. It covers the medic and whoever they charge.
- Medics carry `medigun`: `{"kind": "medigun"|"kritzkrieg"|"quickfix"|"vaccinator", "releasing": bool,
  "resist": "bullet"|"blast"|"fire" (vaccinator only), "target": player id being healed or null}`.
- `weapon` is the weapon in hand: `{"item": item definition index, "clip": rounds loaded or null}`, or null
  if none is known. `clip` is `null` for a weapon without a clip.
- Pyros carry `flames` while firing: `{"crit": bool, "hit": bool}` (`hit`: hurting someone), else null.
- Dead players carry `respawn`: seconds until they respawn, once the server has set it.
- `score` is the player's scoreboard line: `{"points", "kills", "deaths", "assists", "damage", "healing",
  "dominations", "streak"}`. Points, kills and deaths are the server's match totals; `damage` (dealt to
  others), `healing` (given) and `assists` are counted from the events since the match started, as logs.tf
  does, or since connecting. `dominations` and `streak` (current killstreak) are as they stand.
- An engineer carrying a building has `carrying`: `sentry`, `dispenser` or `teleporter`. The building
  is not in `buildings` while it is carried.
- Projectile `kind` is `Rocket`, `Pipe`, `Sticky`, `LooseCannon`, `Flare`, `HealingArrow` (crossbow),
  `Arrow` (huntsman), `EnergyBall` (Cow Mangler), `EnergyRing` (Bison, Pomson), `Fireball`
  (Dragon's Fury), `Cleaver`, `Jarate`, `MadMilk`, `GasPasser`, `ShortCircuitOrb` or `Unknown`;
  `yaw` is its rotation and `owner` the player who fired it.
- Teleporters carry `"entrance": true|false`; sentries carry `mini`, `building` (still constructing),
  `aim` (turret direction), `target` (player id) and `attacking`.
- Carts appear only on payload maps, with the pushing `team`, `progress` (0 → 1), `cappers` and
  `speed` (negative when rolling back).
- On maps with flags, `captures` (`{"red": n, "blue": n}`) counts the flags each team captured
  this round, which is what TF2's CTF HUD shows; `score` stays the rounds won.
- Flags are CTF intelligence briefcases (`team` red or blue) and neutral ones such as the bball
  ball (`team` none). `status` is `home`, `carried` (`pos` is then the carrier's) or `dropped`,
  with `returns_in` seconds until it goes home. Maps with several flag spawns list only the
  enabled ones. Where the mode gives a flag a value, it has `points` (absent for 0): the points a
  robot destruction reactor holds to be stolen, the bottles a player destruction pickup holds.
- Game modes add objectives:
  - pass time: the `ball` (`{"kind": "ball", "id", "pos", "team", "carrier"}`, `team` none
    while neutral, `pos` the carrier's while carried; absent while it waits to respawn) and the
    `goal`s (`{"kind": "goal", "id", "pos", "team"}`, the team that scores there);
    `captures` then counts the goals.
  - robot destruction: `robot`s (`{"kind": "robot", "id", "pos", "team", "hp", "max_hp"}`) and
    the `core`s they drop (`{"kind": "core", "id", "pos", "team"}`).
  - Mann vs. Machine: the `tank` (`{"kind": "tank", "id", "pos", "health"}`, 0 → 1), revive
    markers (`{"kind": "revive", "id", "pos", "owner"}`, the dead player a medic can bring back)
    and `money` on the ground (`{"kind": "money", "id", "pos"}`).
- Robot and player destruction add `points`: `{"red", "blue", "max" (to win, or null),
  "red_leader", "blue_leader"}` (player destruction's team leaders). Other maps that reuse their
  logic for their own HUD (Versus Saxton Hale, tug of war) have it too, with whatever they count.
- Mann vs. Machine adds `mvm`: `{"wave", "waves", "enemies" (robots in the wave), "between_waves",
  "money" (on the ground), "classes"}`. `classes` lists the wave's robots as its HUD does:
  `{"name" (the icon: "scout", "soldier_barrage", "sentry_buster", "tank", …), "count" (left),
  "active" (on the field), "giant", "crit", "support" (not counted towards the wave), "mission"
  (sentry busters, spies, engineers)}`.
- `cap_progress` runs 0 → 1 while `capping_team` caps, else 0. The server sends it every few
  seconds; like the HUD, the tool extrapolates it in between. `cap_rate` is progress per second.
  `on_point` counts the players of each team standing on the point.

### Events

| type | fields |
|---|---|
| `kill` | `victim`, `attacker` (null for world/suicide), `assister`, `weapon` (kill icon name), `crit`, `headshot`, `backstab`, `suicide`, `dominated`, `revenge`, `feign` (Dead Ringer: not a real death), `airborne` (victim was blast jumping), `victim_pos`, `attacker_pos` |
| `damage` | `victim`, `attacker`, `amount`, `health` (left), `crit`, `minicrit`, `weapon_id` |
| `shot` | `player`, `origin`, `pitch`, `yaw`, `weapon_id`, `crit`: one per hitscan shot (bullets, shotgun, sniper, minigun) from TE_FireBullets |
| `explosion` | `origin`, `weapon_id`: rockets, pipes, stickies, … |
| `impact` | `kind` (`blood`, `sparks`, `ricochet`), `pos`: where a bullet hit |
| `mark` | `pos`: a decal (bullet hole, scorch) left on the world |
| `sentry_shot` | `sentry` (building id), `builder`, `target`, `shells`: a sentry fired (its shell count dropped) |
| `spawn` | `player`, `team`, `class`, `pos` (where they spawned: the event waits for the player's new position, which arrives after it) |
| `uber` | `medic`, `target`, `medigun` (kind, as above) |
| `uber_ready`, `uber_ended` | `medic`, `medigun`: the charge filled up / a released charge ran out |
| `airblast` | `player`: a pyro's compression blast |
| `medic_death` | `medic`, `attacker`, `healing` (this life), `drop` (died with a full charge) |
| `capture_blocked` | `point`, `name`, `blocker`, `victim` |
| `reflect` | `player` (the pyro), `owner` (of the projectile), `weapon_id` |
| `banner` | `player`, `kind` (`buff_banner`, `battalions_backup`, `concheror`, `other`) |
| `scores` | `red`, `blue`, `game_over`: the end-of-round panel |
| `stalemate`, `overtime` | (none) |
| `build` | `player`, `object` (`sentry`/`dispenser`/`teleporter`/`sapper`), `id` |
| `building_destroyed` | `owner`, `attacker`, `object`, `id`, `weapon`, `was_building` |
| `building_upgrade` | `player`, `object`, `id`: a building went up a level |
| `building_repair` | `id`, `amount`: a building was repaired |
| `time_added` | `team` (the timer's: `red`/`blue` on king of the hill, else usually `none`), `seconds`: time added to a round timer |
| `team_change` | `player`, `team`, `old_team`, `disconnect` |
| `class_change` | `player`, `class` |
| `player_leave` | `player`, `name`, `reason` |
| `round_start` | `full_reset` |
| `round_active`, `setup_finished` | (none) |
| `round_win` | `team`, `reason`, `round_time` |
| `flag` | `event` (`pickup`, `captured`, `dropped`, `defended`, `returned`), `player`, `carrier` (the carrier a `defended` stopped), `team` (the flag's) |
| `heal` | `patient`, `healer` (null for on-hit heals), `amount`, `source` (`medigun`, `dispenser`, `crossbow`, `on_hit`, `other`; medigun/dispenser are inferred from the healer's class) |
| `blast_jump` | `player`, `kind` (`rocket`, `sticky`, `rocket_pack`), `phase` (`start`, `land`) |
| `pickup` | `player`, `item` (`health`, `ammo`), `size` (`small`, `medium`, `full`, null when unknown) |
| `point_captured` | `point` (index into the control points), `name`, `team`, `cappers` (player ids) |
| `capture_broken` | `point`, `name`: a capture was stopped before it finished |
| `killed_capper` | `point`, `killer`, `victim`, `assister` (null if none): a player capping was killed |
| `medic_defended` | `player`, `medic`: a player protected a medic |
| `vote_options` | `options`: the choices of a call vote that just started |
| `vote_cast` | `player` (null if unknown), `option` (index into the last `vote_options`), `team` |
| `tournament` | `player` (null for the whole match), `ready` (null for a name change): ready-up changes |
| `announcer` | `sound` (raw name of a broadcast sound, e.g. `Announcer.RoundEnds5minutes`; not all are announcer lines), `team` |
| `time_remaining` | `seconds`: the map time limit's remaining time |
| `voice_command` | `player`, `menu`, `item`: a voice menu command (`menu` 0 is `Z`, `item` 0 is `Medic!`) |
| `server_text` | `text` (colour codes stripped): a server message, such as a hint or a vote result |
| `game_over` | `reason` |
| `chat` | `player` (null for console), `name`, `text` (colour codes stripped), `kind`, `parts` (null, or the coloured spans of a line with TF2 colour codes: `{"c": "RRGGBB"/"team"/null, "t": text}`; `team` is the sender's team colour, null the default) |
| `teleported` | `player`, `builder`, `dist` (units between entrance and exit) |
| `stun` | `stunner`, `victim`, `capping`, `big` (a full stun, such as a moonshot) |
| `sapped` | `player` (the spy), `owner`, `object`, `sapper` (its entity) |
| `building_removed`, `building_detonated` | `player`, `object`, `id`: a building went without being destroyed (a new round, a detonation, its engineer leaving) / its engineer blew it up |
| `det_stickies` | `player`: a demoman detonated their stickies |
| `capture_started` | `point`, `name`, `team` (capping), `owner`, `cappers` |
| `point_locked` | `point`, `name`, `locked` |
| `npc_damage` | `id` (entity), `attacker` (null for none, such as a sapper's drain), `amount`, `health` (left), `crit`, `weapon_id`, `boss`: damage to a building, a tank, a robot or a boss |
| `hud_text` | `channel`, `pos` (`[x, y]` on the screen, 0 → 1, -1 centred), `color`, `hold` (seconds), `text`: a map's or plugin's HUD text (`game_text`) |
| `hint` | `text`: a hint shown on the HUD |
| `vote_started` | `team` (`none` for everyone), `caller`, `issue` (`#TF_vote_nextlevel`, …), `details`, `yes_no`, `target` (the player a kick vote is about, or null) |
| `vote_passed` | `team`, `issue`, `details` |
| `vote_failed` | `team`, `reason` (TF2's `vote_create_failed_t`) |
| `pass_get`, `pass_free` | pass time: `player` got the ball / lost it (`attacker`: who knocked it loose, or null) |
| `pass_caught` | `passer`, `catcher`, `dist`, `duration` (seconds in the air) |
| `pass_score` | `scorer`, `assister` (or null), `points` |
| `pass_stolen` | `victim`, `attacker` |
| `pass_blocked` | `owner` (who threw it), `blocker` |
| `ball_carrier_killed` | `attacker`, `victim`, `assister` |
| `robot_killed` | robot destruction: `attacker`, `weapon` |
| `points_scored` | `player`, `method` (`reactor_captured`, `cores_collected`, `reactor_returned`, `reactor_stolen`), `amount` (negative for points lost) |
| `special_score` | player destruction: `player` delivered their pickups |
| `team_leader_killed` | `killer`, `victim` |
| `rocket_open` | special delivery: `team`, whose rocket opened |
| `round_selected` | territorial control: `round`, the map's name for the round's points |
| `upgrade` | Mann vs. Machine: `player` bought an upgrade: `item` (item definition index of what was upgraded, 30015 the Power Up Canteen), `attribute` (attribute definition index), `cost` |

For "fired" markers, diff the projectile ids of consecutive snapshots.

## How it works

- `src/net`: the Source 2013 netchannel client (protocol 24): anonymous handshake, header and
  checksum, split and Snappy packets, reliable reassembly, signon messages. The server is spoken
  to on whichever address answers the challenge: one on the same host behind Docker answers from
  the bridge gateway.
- `src/session.rs`: the signon state machine, reliable send/ack, tick acks every 100 ms, silence
  timeout and in-place map changes; the same driver runs live and over captures.
- `src/decode`: [tf-demo-parser](https://crates.io/crates/tf-demo-parser) parses each message
  stream with the same-build send tables; `extract.rs` turns its game state into the JSON above.


## Known limits

- Each instance takes one STV slot. One that vanishes without a goodbye holds it for 300 s, so
  the tool always disconnects when it stops or reconnects.
- After a map change the server may drop the client or restart signon a few times within
  `tv_delay`; the tool follows until the delayed stream reaches the new map.
- Memory grows with the current map's data (about 26 MB for a 3-minute 6v6) and is freed at each
  map change. Past 1 GiB (`Options::max_decoder_bytes`) the tool reconnects to start afresh.
- SourceTV relays often send deltas from a frame a few ticks back, so a projectile that came and
  went in between is never deleted. Like a game client, the tool builds each frame's entities from
  the base frame's and drops the projectiles and buildings left out of it.
- Temp entities (`explosion`, `shot`, `impact`, `mark`) fill omitted properties from the one just before
  them in the same message if it is of the same class, else with zeros, as the engine does.
- `shot` covers hitscan only; melee swings have no event.
- Not reported, as other lines or snapshots already carry them: `player_domination` (the kill's
  `dominated`), `player_invulned` (`uber`), `environmental_death` (the `kill`), `ctf_flag_captured`
  and `flagstatus_update` (`flag`), `capper_killed` (`killed_capper`), `arena_round_start` and
  `arena_win_panel` (the round lines), `control_point_starttouch`/`endtouch` (`on_point`),
  `player_score_changed`, `teams_changed`, `teamplay_pre_round_time_left`, `teamplay_update_timer`,
  `teamplay_restart_round`, `teamplay_round_restart_seconds`, `win_limit_changed`,
  `post_inventory_application`, `player_regenerate`, `player_activate`, `player_connect_client`
  and `player_initial_spawn`; `weapon_equipped` (the snapshot's `weapon`) and `take_health` (the
  heals and pickups). `ammo_pickup` names no player.
- Not reported as server bookkeeping, client-only or cosmetic: `hltv_status`, `server_cvar`,
  `server_addban`, `entity_killed`, `remove_nemesis_relationships`, `recalculate_holidays`,
  `score_stats_accumulated_*`, `stats_resetround`, `nav_blocked`, `break_breakable`, `break_prop`,
  `localplayer_pickup_weapon`, `item_found`, `achievement_earned`, `player_killed_achievement_zone`,
  `quest_progress`, `teamplay_teambalanced_player`, `arrow_impact`, `rd_robot_impact`,
  `rps_taunt_event`, `kill_refills_meter`, `player_buff`, `player_destroyed_pipebomb`,
  `flag_carried_in_detection_zone`; and the user messages for screen effects, the local client's
  HUD, MvM's spending panels and announcements, stats panels and taunt sounds. A player's MvM
  money is sent only to that player, so it is not in SourceTV.

## Development

Install Rust with [rustup](https://rustup.rs); `rust-toolchain.toml` picks stable with
`rustfmt` and `clippy`; older Rust versions aren't supported. CI (`.github/workflows/ci.yml`) runs:

```
cargo fmt                                         # format (settings in rustfmt.toml)
cargo clippy --all-targets -- -D warnings         # lint (lint set in Cargo.toml [lints])
cargo test --release                              # tests
cargo deny check                                  # advisories and licenses (cargo install cargo-deny)
```

`ln -s ../../.githooks/pre-commit .git/hooks/pre-commit` runs fmt and clippy before each commit
(not `core.hooksPath`, which would replace any global git hooks). Dependabot
opens weekly update pull requests.

Unit tests cover the codec and protocol layers. `tests/captures.rs` replays SourceTV traffic
from a bot server (`tests/fixtures/*.cap`, made with `connect --record`) through the session
and decoder, including a map change, and `tests/golden.rs` compares a whole replay with its recorded
output (`bots_cp_badlands.jsonl.gz`); `tests/payload.rs` runs the start of a real pl_upward
match (`tests/fixtures/pl_upward.dem`) and `tests/flags.rs` a bball match
(`tests/fixtures/ctf_ballin_sky.dem`). `tests/detail.rs` also decodes a ctf_turbine match
(`tests/fixtures/ctf_turbine.dem`) for votes and medic defences. `tests/modes.rs` runs the starts
of pass time, robot and player destruction, special delivery, Mann vs. Machine and territorial
control matches. `cargo run --example cut_demo -- IN.dem OUT.dem N`
cuts a demo after tick N, to make a fixture.

## Credits

- The network layer is a port of [stv-watch](https://github.com/dmitry-ra/stv-watch) by
  dmitry-ra (MIT), a SourceTV client for Half-Life 2: Deathmatch.
- Decoding is [tf-demo-parser](https://codeberg.org/demostf/parser) and
  [bitbuffer](https://crates.io/crates/bitbuffer) by Robin Appelman of
  [demos.tf](https://demos.tf) (MIT or Apache-2.0).
- `tests/fixtures/pl_upward.dem` is cut from a serveme.tf match's STV demo on demos.tf, and
  `tests/fixtures/ctf_ballin_sky.dem` is a bball match's STV demo and
  `tests/fixtures/ctf_turbine.dem` a ctf_turbine match's (demos.tf 1287498), both from demos.tf.
- The game-mode fixtures are the starts of STV demos from demos.tf, cut with `cut_demo`:
  `pass_stadium.dem` (pass time, demos.tf 1516963), `rd_asteroid.dem` (robot destruction,
  954995), `pd_watergate.dem` (player destruction, 340112), `sd_doomsday.dem` (special delivery,
  1232410), `mvm_crossing.dem` (Mann vs. Machine on mvm_gd_crossing_b3b, 1484401) and
  `tc_hydro.dem` (territorial control, 1503662).
