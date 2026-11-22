//! How a connection goes, and what the server and world are.

use serde::{Deserialize, Serialize};

use super::Position;

/// The connection's state (`type: status`, tagged by `state`).
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    /// Connecting to `server`, as a client of `build`.
    Connecting {
        /// The address connected to.
        server: String,
        /// The build offered to the server.
        build: String,
    },
    /// Signon progress.
    New,
    /// Signon progress.
    Prespawn,
    /// Signon progress.
    Spawn,
    /// Data is flowing.
    Full {
        /// The map, once the server has said.
        map: Option<String>,
    },
    /// The server announced a map change.
    Changelevel,
    /// Signon restarts for a new map: reset the view.
    MapChange,
    /// The session ended.
    Disconnected {
        /// Why.
        reason: String,
        /// Messages that could not be decoded.
        decode_errors: u64,
        /// Packets received.
        packets: u64,
        /// Packets lost.
        lost: u64,
    },
    /// A host is fetching a recording to play: `percent` of it has arrived, if the host knows.
    Fetching {
        /// Of a hundred.
        percent: Option<u8>,
    },
    /// A host is decoding a recording it fetched.
    Decoding,
    /// A connection attempt failed; another follows.
    ConnectFailed {
        /// Why.
        reason: String,
    },
    /// Watching ended for good. [`crate::watch::watch`] returns an error instead; a host that
    /// reports that error to its own readers sends this.
    GaveUp {
        /// Why.
        reason: String,
    },
    /// The relay is up: clients can watch through `listen` (see [`crate::relay`]).
    Relay {
        /// The address it listens on.
        listen: String,
    },
}

/// The server, at every signon.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[expect(missing_docs, reason = "the server's own values")]
pub struct Server {
    pub tick: u32,
    pub map: String,
    pub hostname: String,
    /// Seconds per tick.
    pub tick_interval: f64,
    pub max_players: u8,
    pub spawn_count: u32,
}

/// The world's bounds, once per map. They include the 3D skybox.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[expect(missing_docs, reason = "self-explanatory")]
pub struct World {
    pub tick: u32,
    pub min: Position,
    pub max: Position,
}
