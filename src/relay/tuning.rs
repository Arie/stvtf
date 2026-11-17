//! How long the relay waits and how much it keeps.

use std::time::Duration;

/// How often the trace sums up what each client was sent.
pub(super) const TRACE_EVERY: Duration = Duration::from_secs(5);

/// TF2's spectator panel.
pub(super) const SPECTATORS: &str = "specgui";

/// How long after a HUD reset, and after going into the game, a client gets the notes again.
pub(super) const NOTES_AFTER_RESET: Duration = Duration::from_millis(300);

pub(super) const NOTES_AFTER_ENTERING: Duration = Duration::from_secs(2);

pub(super) const CLIENT_TIMEOUT: Duration = Duration::from_secs(30);

/// A client in its signon may be silent this long: a game client loading a map sends nothing.
pub(super) const SIGNON_TIMEOUT: Duration = Duration::from_secs(120);

/// How long another signon waits for the clients in one: a caster does not wait on a slow
/// viewer, who is dropped and retries.
pub(super) const SIGNON_HOLD: Duration = Duration::from_secs(30);

/// A client hears from us at least this often, so its connection does not time out.
pub(super) const KEEPALIVE: Duration = Duration::from_millis(100);

pub(super) const READ_POLL: Duration = Duration::from_millis(5);

/// A client acks -1 until the full update it asked for arrives; only after this long is it
/// asking for another.
pub(super) const FULL_UPDATE_GRACE: Duration = Duration::from_secs(1);

/// How long a client has to ack a full update before it is sent another: no delta goes to it
/// until it has, as it would crash on one from a frame it lacks.
pub(super) const KEY_WAIT: Duration = Duration::from_millis(if cfg!(test) { 30 } else { 600 });

/// Datagrams kept since the last full update, for clients that join after it.
pub(super) const MAX_SINCE_FULL: usize = 64;

/// After a full update, the next is asked of the server no sooner than this, however many
/// clients want one: each costs the server a full snapshot.
pub(super) const FULL_UPDATE_GAP: Duration = Duration::from_secs(2);
