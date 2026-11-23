# Relay

```
stvtf connect HOST:PORT … --relay 0.0.0.0:27121 [--relay-password PW] [--relay-slots 10]
stvtf relay   HOST:PORT [--listen 0.0.0.0:27020] [--relay-password PW] [--relay-slots 10]
              [--password tv] [--name …] [--tables …] [--crc …] [--build N]
stvtf relay   FILE.cap  [--listen …] [--relay-password PW] [--relay-slots 10] [--speed 1]
              [--tables …] [--build N]
stvtf relay   FILE.dem  [--listen …] [--relay-password PW] [--relay-slots 10] [--speed 1]
```

`relay` takes its target as a demo when it is an existing file ending in `.dem`, as a capture when
it is any other existing file, and as a server otherwise.

`--relay` passes the server on to TF2 clients (or other stvtf watchers) that connect to that
address with `connect HOST:PORT; password PW`, as `tv_relay` does. It uses the watch's own
connection, so it costs the server no slot. In the library it is `Watch::relay`, a
`RelayConfig`. `relay` does only the relaying and prints only status lines.

- **Password**: `--relay-password`, else `STVTF_RELAY_PASSWORD`, else a random one printed to
  stderr. An empty password lets anyone in. A wrong one is refused as the server refuses it.
- **Full rate**: a relay passes on every frame the server sends, at its `tv_snapshotrate` (66 a
  second with a competitive config). `--hz` only limits the snapshots stvtf prints.
- **Slots**: 10 clients at once by default (`--relay-slots`, 1 to 255). Each client gets what the
  server sends the relay, on the order of 20 to 40 KB/s for a 6v6. A full relay refuses a client
  with `#GameUI_ServerRejectServerFull`.
- **Status line**: once the relay listens, `{"type":"status","state":"relay","listen":"0.0.0.0:27121"}`
  (with `time`).
  The address is useful with port 0, which picks a free port.
- **No reflection**: a connectionless reply is never larger than the query, until the address
  has proved itself by echoing a challenge. `A2S_INFO` first gets a challenge, as on current
  srcds, and a connect with a wrong challenge gets no reply at all. The challenges are derived
  from the address with a secret key, so nothing is stored before a client connects.
- **Log**: a line each through `tracing` (the CLI writes them to stderr) for a
  client that connects, gives a wrong password, leaves or times out (how long it was silent, and
  which phase it was in for how long), by its name and its address less the last part; and, of a
  relay playing a demo, for a jump, a pause and playing on, a demo that goes on into another or
  why it does not, a handover, and a demo that cannot be played. `RUST_LOG=stvtf::relay=debug` also traces
  each client's handshake packets, its signon timing, the transfers it is sent, and every 5 s how
  many of its packets carried server data.

## Relaying a server

Nothing is re-encoded: clients get the server's own message bits.

- The reliable messages of the server's signon are logged and replayed to each client. A
  `net_SignonState` of NEW, PRESPAWN or SPAWN holds the rest back until the client echoes it, as a
  server does. After signon, only string table updates join the log.
- Entities are deltas from the frame the relay last acked, so a client can only follow from a
  full update onward. The relay keeps the datagrams since the last full update (up to 64) and
  sends them to a client that joins in that window. Otherwise it asks the server for a full
  update (tick ack -1), at most once every 2 s, and passes it on. From then on, each datagram's
  unreliable part goes to the client unchanged. A client that asks for a full update itself (it
  lost a frame) is given one the same way.
- A map change restarts the signon on the relay's connection, and every client signs on again.
- A client that has not been heard from is dropped: after 120 s while it signs on (loading a map
  is silent), after 30 s in the game.
- A client in the game gets exactly one packet per server packet, as from the STV itself:
  `net_graph 5` shows the same rates and no packets without entities. Reliable data rides
  along, and only a server silent for 100 ms gets a keepalive in between. Signon transfers go
  out on all eight subchannels at once.

## Relaying a capture

Given a capture, `relay` replays the capture's first session instead of connecting, at `--speed`:
it starts when the first client connects and pauses while a client signs on. `tests/relay.rs`
connects stvtf to it and gets the same output as `stvtf replay`. A client that joins a capture
outside the 64-datagram window gets no entities, since a capture cannot be asked for a full
update.

## Playing a demo

`stvtf relay FILE.dem` plays a demo to its clients at `--speed`. Its clock starts when the program
does, so a client that joins later starts where the demo is by then. With nobody in, nothing is
played. A player's own (POV) demo is played too, with the view fixed and prediction off.

In the library it is `Relay::play`, which follows a `Playback`. A `Playback` holds a `Want`: the
demo, the tick wanted at an instant, paused or not, the speed and the camera. `Playback::set_want`
changes it as playback goes (a room on stv.tf sets it as its own playback moves). The demo may
still be being written.

### Pace and jumps

- Frames go out at their own pace, up to the moment wanted, each when its own `net_Tick` is due
  (an STV demo has no frames while the server is paused).
- More than a second off the moment wanted, playback jumps: it decodes on without sending (to go
  back, from before there), and every client gets a full update of where it went, and whether the
  match is paused there.
- A thread decodes the whole demo on the side as soon as it is opened (following a growing one),
  keeping a checkpoint every 2000 ticks (30 s). A jump decodes from the last checkpoint before
  where it goes, so at most 30 s of the match; decoding runs at about 500 times game speed.
- A checkpoint is the string table changes so far, a full update and the frames after it, and
  where the file goes on.
- Paused, no frames go out. The clients are not told, so their clock runs on: their full update
  holds the round timers where they are, and playing on they get another. The match's own pauses
  in the demo they are told (`svc_SetPause`).
- With nobody in, what was decoded is dropped after a minute.

### String tables after a jump

- Ahead, the clients are sent the changes passed over.
- Back, they have the tables of later on. Where an entry of `userinfo` (a player left or joined)
  or `instancebaseline` changed value since, they are first sent those entries as they were
  (`svc_UpdateStringTable`), and the stream's own changes again when playback comes to them.
  Where those cannot be written, they sign on again.

### Full updates and deltas

A game client sent a delta from a frame it lacks crashes, and any datagram may be lost, so a
playing relay does as a server does.

- A full update is written from the decoded entities (`decode::world`): each entity entering
  with its props that differ from its class's static baseline, as a server writes one. Nothing is
  asked of a game server.
- A client gets one when it comes in, when it asks for one, and after a jump, a pause and playing
  on. It is of the frame the demo's next one builds on (in a recorded live session, the last one
  stvtf acked, often a few ticks back).
- A client gets no entities until it acks its full update, and another full update if it has not
  in 600 ms.
- From then on each frame's entities are a delta from the frame that client last acked: the
  demo's own when that is the one it builds on, else written anew, listing every prop that changed
  on the way.
- A client whose acked frame is no longer kept (3 s of ticks are, 128 frames at most) gets a full
  update again. So do the clients when a frame of the demo is one they cannot follow (a full
  update, or a delta from a frame older than they may hold).
- A full update is followed by the last round start, win panel or game over event decoded, so the
  HUD shows the round's end, or hides it, as at that moment. Before the demo's first such event
  the relay sends a round start of its own.

### What is changed in the demo's stream

- An STV demo's signon states carry another spawn count than its ServerInfo; a game client
  refuses that, so they are given the ServerInfo's.
- A frame's own `net_SignonState` (the changelevel a demo ends with) is not passed on: a game
  client would leave the map for one. A client's signon is the relay's.

### Going on to another demo

- For another demo file a client signs on again (`net_SignonState(CONNECTED)` first, as a
  server's reconnect), but never while it is still in a signon. The next demo waits until every
  client is through, 30 s at most; one still signing on then is dropped with a reason (it can
  `retry`).
- Another demo of the same map load of the same server (its ServerInfo says so: a match recorded
  again, by the next process) goes on from the one playing with no signon. The one playing is
  decoded up to where the other begins, the clients keep what they have and get a full update,
  and the new demo's signon is what a client coming in later is given.
- That does not work when the one playing ends before the other begins, nor when a later client
  would get the relay's laser model elsewhere than the others have it. Then they sign on again.

### Handing clients to another process

A playing relay hands its clients to the process after it, so a restart drops nobody.

- `Playback::hand_over(wait)` gives a `Handover` (or none), which serializes. The next process
  calls `Relay::take_over` on a relay bound to the same address, before `Relay::play`.
- It carries each client's connection (sequence numbers, reliable state, challenge), all they
  were sent since their signon, and where the demo was.
- The next relay decodes the demo up to there, giving the clients nothing, and goes on with a full
  update. Wanted on another demo, or with no frame there, it signs them on again, still connected.
- A client still in its signon or a transfer a second after the handover is asked for is told to
  `retry`.

### What a room adds

A `Playback` also holds what a room shows the clients in the game besides the demo.

| method | |
|---|---|
| `keep_beams` | lines in the world, drawn with a laser model the relay loads for the clients, or else a beam sprite the match loaded. Sent again every 2 s. If the match later loads the laser itself, the clients sign on again. |
| `keep_notes` | the game's text bubbles: `Note { id, text, at, player }`. A note on a player is hidden while they are dead. |
| `keep_light` | one light. Sent again every 4 s. |
| `push_voice` | voice (`VoiceIn`), as a player's own. A speaker who is no player (a caster, a coach) talks from a slot nobody is in, or on a full server as SourceTV, one at a time. |
| `keep_names` | `Names`: players' names by SteamID (in `userinfo`) and teams' by `red` and `blue` (`mp_tournament_redteamname`, `blueteamname`), given again after every full update. |
| `keep_score` | points added to the teams' `m_iScore`: in every full update, and in the frame of a round won, which is then written for each client from the frame it acked. A change in what is added gives a full update. |

### The camera

`Want::camera` replaces the demo's director cues for clients with `spec_autodirector 1`.

- `Camera::Follow { player }` shows the game through a player (their user id).
- `Camera::Along(path)` moves the camera along a `Path { id, began, steps }`, each `Step` being
  `{ secs, pos, aim }`: seconds in, where the camera is and the point it looks at. It stays at the
  last step.
- A path of one step is a camera that stands still. The relay sends it as `hltv_fixed`, in whole
  degrees.
- While a path moves, clients watch a hidden player (the cameraman) that looks at an invisible
  prop the relay moves each frame; that is the only way TF2 moves a camera smoothly. Once the
  path ends the relay switches to `hltv_fixed` and removes both.
- The same path under a new `id` sets off again. A path advances with the frames played, so not
  over a pause or a jump, and faster at a higher speed.
- With the camera given up (`None`), clients are sent to a living player.

### A demo that cannot be played

The clients get nothing. `Playback::problem` says why, and the log says it once.

- A demo that is not there, or not written up to the end of its signon, is tried again every
  200 ms. (The CLI itself needs the file to exist when it starts.)
- One that fails while playing (the file went, or is corrupt) is opened again after 200 ms.
- One that cannot be opened for another reason is not tried again: one whose class tables are not
  this build's, say, as a game client builds its classes from its own game.

## Recording demos

`Watch::demos` records the session as `.dem` files. It is library only: there is no flag for it.

- It takes a `demo::Files`, whose `create(map, first_tick)` gives the file to write; so the host
  names the files.
- One file per connection: a reconnect or a new map starts another. It is created at the first
  frame. If that fails, the reason is logged and nothing is recorded until the next connection.
- A file holds the signon the connection got, with the data tables of the build, then one frame
  per datagram, every message as the server sent it. So a demo has the STV's full
  `tv_snapshotrate`, whatever `--hz` is.
- Every frame reaches the file at once and the header's counts are written every 100 ms, so a
  file can be played while it is being written.

`demo::with_voice` copies a demo with voice written into it, as the game's own voice messages.

## Library entry points

| | |
|---|---|
| `Relay::bind(&RelayConfig)` | binds the address |
| `Relay::watch` | relays a server it watches (what `Watch::relay` does for `stvtf::watch`) |
| `Relay::replay` | replays a capture |
| `Relay::play(&Playback, …)` | plays demos, following the `Playback` |
| `Relay::take_over(&Handover)` | takes the clients of the process before it; false if it cannot |
| `Relay::local_addr`, `Relay::clients` | the address bound, and how many clients are in |
