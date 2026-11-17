//! A `SourceTV` relay: the connection a watch already has, passed on to the TF2 clients (or
//! stvtf watchers) that connect to us, as `tv_relay` does. It costs the server no slot.
//!
//! Nothing is re-encoded: every client gets the server's own message bits.
//!
//! - Signon: the reliable messages of the server's signon go into a backlog replayed to each
//!   client, stopping after each `net_SignonState` until the client echoes it, as a server
//!   waits. After signon only string table messages join it, so a late client still gets
//!   the tables as they are now; other reliable messages go to the clients already in.
//! - Entities: the server deltas each frame from the one we last acked, so a client can only
//!   follow from a full update on. The datagrams since the last full update are kept while
//!   few, and a client ready for entities gets them at once; once there are too many, it makes
//!   us ask the server for a full update (tick ack -1). From then on every datagram's
//!   unreliable part goes to the client unchanged. A client that loses a frame asks for a full
//!   update, and we ask the server.
//! - A map change re-runs the server's signon on our connection: the backlog starts again, and
//!   every client signs on again with it.
//! - A client in the game gets one packet per server datagram, as from the server itself.
//! - Reliable messages go one transfer at a time, the next when the last is acked, so they can
//!   reach a client up to about a round trip behind the datagrams: 12 ticks at most, on 8% of
//!   a capture's lines, with 200 ms of ping. Their own order stays.
//!
//! Connectionless replies are never larger than what asked for them, unless the address proved
//! it is real by answering a challenge, so the relay cannot be used to reflect traffic.

mod addr;
mod admission;
mod backlog;
mod client;
mod client_msg;
mod handover;
mod hub;
mod out;
mod play;
mod serve;
mod server_msg;
mod transfer;
mod tuning;

pub(crate) use self::addr::masked_addr;
pub(crate) use self::client_msg::{ClientMsg, client_messages};
pub use self::handover::Handover;
pub use self::play::{Beam, Camera, Light, Note, Playback, VoiceIn, Want};
pub use self::serve::{DEFAULT_SLOTS, MAX_SLOTS, Relay, RelayConfig, random_password};

#[cfg(fuzzing)]
mod fuzz_bench;
#[cfg(test)]
mod tests;

#[cfg(fuzzing)]
pub(crate) use self::fuzz_bench::FuzzBench;
