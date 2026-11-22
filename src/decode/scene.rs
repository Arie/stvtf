//! What a relay keeps of the stream for its clients: the entities (`world`), the string tables
//! and the last round event. A [`View`] reads it; a [`Stage`] changes what the clients are shown
//! on top (the relay's camera, a room's score).

use bitbuffer::{BitReadBuffer, BitReadStream, LittleEndian};
use tf_demo_parser::ParserState;

use super::decoder::{Baselines, Decoder, Entities, entities_in};
use super::extract::Extractor;
use super::tables::{Placed, Tables};
use super::world::{Pose, World};
use super::writer::Writer;
use crate::net::bits::{BitBuf, BitWriter};

/// The relay's side of a decoder.
#[derive(Default)]
pub(crate) struct Scene {
    /// The entities, which a full update is written from. Kept only after [`Stage::keep_world`].
    pub(super) world: Option<World>,
    /// The string tables, as far as a jump back needs them (see `tables`).
    pub(super) tables: Tables,
    /// The last round start, win panel or game over event (see [`View::round_event`]).
    pub(super) round: Option<BitBuf>,
}

/// The scene, to read.
pub(crate) struct View<'a> {
    scene: &'a Scene,
    state: &'a ParserState,
    extractor: &'a Extractor,
}

/// The scene, to change.
pub(crate) struct Stage<'a> {
    scene: &'a mut Scene,
    state: &'a ParserState,
    /// How many players the server has room for.
    slots: u8,
}

impl Decoder {
    /// The relay's side of this decoder, to read.
    pub(crate) fn scene(&self) -> View<'_> {
        View { scene: &self.scene, state: self.handler.get_parser_state(), extractor: self.extractor() }
    }

    /// The relay's side of this decoder, to change.
    pub(crate) fn scene_mut(&mut self) -> Stage<'_> {
        let slots = self.extractor().max_players();
        Stage { scene: &mut self.scene, state: self.handler.get_parser_state(), slots }
    }
}

/// The server tick of the frame the entities in message stream `data` build on, if a delta.
fn delta_base(state: &ParserState, data: &[u8]) -> Option<u32> {
    match entities_in(state, data) {
        Some(Entities::Delta(base)) => Some(base),
        _ => None,
    }
}

impl View<'_> {
    /// A keyframe: `net_Tick` and every entity in full, with the frame's server tick. A client
    /// that starts from it and gets the frames after it can follow. The frame is the one that
    /// `next` (the next frame's message stream) builds on, or the latest when `next` is a full
    /// update or not given. None without [`Stage::keep_world`], before a full update came, after a
    /// frame may have been missed, or when the frame `next` builds on is not kept.
    pub(crate) fn keyframe(&self, next: Option<&[u8]>) -> Option<(u32, BitBuf)> {
        let base = next.and_then(|d| delta_base(self.state, d));
        self.scene.world.as_ref()?.full_update(self.state, base, None)
    }

    /// [`View::keyframe`] with every running round timer held where it is, for clients whose
    /// clock runs on while no frames come (a room paused).
    pub(crate) fn keyframe_held(&self, next: Option<&[u8]>) -> Option<(u32, BitBuf)> {
        let base = next.and_then(|d| delta_base(self.state, d));
        self.scene.world.as_ref()?.full_update(self.state, base, Some(self.extractor.interval()))
    }

    /// One message with the entities of the last frame decoded, as a delta from frame `from`: for
    /// a client that acked `from`. None when either frame is not kept, or it does not encode.
    pub(crate) fn entities_from(&self, from: u32) -> Option<BitBuf> {
        self.scene.world.as_ref()?.delta_message(self.state, from, self.extractor.server_tick)
    }

    /// The frames to send after a keyframe of frame `key`, taken from `recent` (frames played, by
    /// server tick). Each frame after `key` goes as played if the client still has the frame it
    /// builds on. Otherwise (a live server's frame builds on one a few ticks back) it is rewritten
    /// as a delta from `key`, or from the last frame sent once the client dropped `key`. A game
    /// client keeps only frames with entities, and drops those older than the base of each delta
    /// it takes.
    pub(crate) fn catch_up(&self, key: u32, recent: &[(u32, BitBuf)]) -> Vec<BitBuf> {
        let Some(world) = &self.scene.world else { return Vec::new() };
        let state = self.state;
        let mut have = vec![key];
        let mut out = Vec::new();
        let own = world.own_baselines();
        for (tick, bits) in recent.iter().filter(|(t, _)| *t > key) {
            match entities_in(self.state, bits.bytes()) {
                // No entities in it: nothing builds on it.
                None => out.push(bits.clone()),
                Some(Entities::Delta(base)) if have.contains(&base) && !own => {
                    out.push(bits.clone());
                    have.retain(|t| *t >= base);
                }
                _ => {
                    // Builds on the keyframe's frame while the client has it: the stream's next
                    // frames may still build on that one.
                    let from = if have.contains(&key) { key } else { have.last().copied().unwrap_or(key) };
                    match world.delta(state, from, *tick) {
                        Some(d) => {
                            out.push(d);
                            have.retain(|t| *t >= from);
                        }
                        None if own => {}
                        None => out.push(bits.clone()),
                    }
                }
            }
            if world.has(*tick) && !have.contains(tick) {
                have.push(*tick);
            }
        }
        out
    }

    /// The last round start, win panel or game over event. A client that skipped to this moment
    /// (it jumped, or just joined) is sent it after a full update, so its HUD shows or hides the
    /// round's end as the game does. Before the demo's first such event, this is a round start
    /// written here, which hides the win panel for a client that jumped back from one.
    pub(crate) fn round_event(&self) -> Option<BitBuf> {
        use tf_demo_parser::demo::gameevent_gen::{GameEvent, TeamPlayRoundStartEvent};
        self.scene.round.clone().or_else(|| {
            Writer { state: self.state, extractor: self.extractor, tables: &self.scene.tables }
                .game_event(GameEvent::TeamPlayRoundStart(TeamPlayRoundStartEvent { full_reset: false }))
        })
    }

    /// Whether the last frame changed the relay's camera entities. The stream's entity message
    /// then cannot go to a client as it is.
    pub(crate) fn shot_touched(&self) -> bool {
        self.scene.world.as_ref().is_some_and(World::shot_touched)
    }

    /// The entity of the hidden player the clients watch through while the relay places the
    /// camera, once the latest frame has it.
    pub(crate) fn cameraman(&self) -> Option<u32> {
        self.scene.world.as_ref()?.cameraman()
    }

    /// The player slots nobody is in, highest first, without the cameraman's.
    pub(crate) fn spare_slots(&self) -> Vec<u8> {
        let man = self.cameraman();
        self.extractor.spare_slots().into_iter().filter(|s| man != Some(u32::from(*s) + 1)).collect()
    }

    /// A message stream that brings a client from this decoder's string tables to a demo's tables
    /// as its recording started (`data` is the demo's packet of them; see [`Tables::up_to`]).
    /// None when nothing differs, or `data` does not parse.
    pub(crate) fn snapshot_stream(&self, data: &[u8]) -> Option<BitBuf> {
        use tf_demo_parser::demo::packet::stringtable::StringTable;
        let mut s = BitReadStream::new(BitReadBuffer::new(data, LittleEndian));
        let count: u8 = s.read().ok()?;
        let snapshot: Vec<StringTable<'_>> = s.read_sized(usize::from(count)).ok()?;
        let msgs = self.scene.tables.up_to(&snapshot, self.state);
        if msgs.is_empty() {
            return None;
        }
        let mut w = BitWriter::new();
        for m in &msgs {
            w.write_buf(m);
        }
        Some(w.finish())
    }

    /// Whether the stream's entity messages cannot go to a client as they are (see
    /// [`World::own_baselines`]).
    pub(crate) fn own_baselines(&self) -> bool {
        self.scene.world.as_ref().is_some_and(World::own_baselines)
    }

    /// The stream's own baselines, for a decoder that continues from here.
    pub(crate) fn baselines(&self) -> Option<Baselines> {
        self.own_baselines().then(|| Baselines(Box::new(self.state.instance_baselines.clone())))
    }

    /// How many `userinfo` and `instancebaseline` entries were rewritten so far (see `tables`).
    pub(crate) fn tables_rewritten(&self) -> u32 {
        self.scene.tables.rewritten()
    }

    /// Whether the last frame set the score of a team that has something added.
    pub(crate) fn score_touched(&self) -> bool {
        self.scene.world.as_ref().is_some_and(World::score_touched)
    }

    /// A message naming the players in the match as `names` has them (see [`Tables::named`]).
    pub(crate) fn named(&self, names: &std::collections::BTreeMap<String, Option<String>>) -> Option<BitBuf> {
        self.scene.tables.named(names, self.state)
    }

    /// Messages that set a client with the string tables of `later` (a decoder further on in the
    /// same demo) back to this one's, as far as they matter; None if they cannot be written.
    pub(crate) fn set_tables_back(&self, later: &Decoder) -> Option<Vec<BitBuf>> {
        self.scene.tables.set_back(&later.scene.tables, self.state)
    }

    /// Where the `instancebaseline` entries were placed so far.
    pub(crate) fn placed(&self) -> Placed {
        self.scene.tables.placed()
    }
}

impl Stage<'_> {
    /// Sets the last round event, for a decoder that starts from a checkpoint.
    pub(crate) fn set_round_event(&mut self, event: Option<BitBuf>) {
        self.scene.round = event;
    }

    /// Sets the clients' camera from the next frame on: `pose`, or None for the director's again.
    pub(crate) fn set_shot(&mut self, pose: Option<Pose>) {
        let (model, slots) = (self.scene.tables.studio(), self.slots);
        if let Some(w) = &mut self.scene.world {
            w.set_shot(pose, model, slots);
        }
    }

    /// Sets the player slots (from 0) the cameraman must not take, because a voice uses them.
    pub(crate) fn keep_from_shot(&mut self, slots: &[u8]) {
        if let Some(w) = &mut self.scene.world {
            w.keep_from_shot(slots);
        }
    }

    /// Places the camera in the frames kept, for a paused room. Returns whether any changed;
    /// clients then need a full update.
    pub(crate) fn reshoot(&mut self) -> bool {
        let state = self.state;
        self.scene.world.as_mut().is_some_and(|w| w.reshoot(state))
    }

    /// Keeps every entity's props from the next full update on, for [`View::keyframe`].
    pub(crate) fn keep_world(&mut self) {
        self.scene.world.get_or_insert_with(World::default);
    }

    /// Adds `add` (BLU, RED) to the teams' scores in what is written for a client (see
    /// [`World::set_score_add`]). Returns whether `add` changed.
    pub(crate) fn set_score_add(&mut self, add: [i64; 2]) -> bool {
        self.scene.world.as_mut().is_some_and(|w| w.set_score_add(add))
    }

    /// Sets the placement, for a decoder fed streams that were already placed.
    pub(crate) fn set_placed(&mut self, placed: Placed) {
        self.scene.tables.set_placed(placed);
    }
}
