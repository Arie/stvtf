//! A headless `SourceTV` spectator for Team Fortress 2.
//!
//! [`watch::watch`] connects to a `SourceTV` port and passes every event and snapshot
//! (the JSON described in the README) to a callback, reconnecting when dropped.
//! [`capture`] replays recorded traffic and [`decode::demo_file`] runs a `.dem`
//! through the same extractor.

#![warn(missing_docs)]

pub mod builds;
pub mod capture;
pub mod decode;
pub mod demo;
pub mod output;
pub mod relay;
pub mod watch;

mod error;
mod net;
mod session;
mod sync;

pub use self::error::InternalError;
pub use self::output::Line;
pub use self::session::{DEFAULT_SNAPSHOT_HZ, DEFAULT_TICK_INTERVAL, Fatal, Options, parse_snapshot_hz};

#[cfg(fuzzing)]
pub mod fuzzing;

#[cfg(test)]
mod hostile;
#[cfg(test)]
mod recovery;
#[cfg(test)]
mod testing;

/// What went wrong in a call of this library.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A file or socket could not be opened, read or written, or an address resolved.
    #[error("cannot {doing}")]
    Io {
        /// What was being done.
        doing: String,
        /// The I/O error.
        source: std::io::Error,
    },
    /// Data that is not what it should be: a demo, capture, tables or message cut short or
    /// broken, or an argument out of range.
    #[error("{reason}")]
    Invalid {
        /// What is wrong.
        reason: String,
    },
    /// A build with no tables compiled in, and no demo of it given.
    #[error("cannot find built-in tables for build {build}")]
    UnknownBuild {
        /// The build (`A2S_INFO` `version`).
        build: String,
    },
    /// What could not be done, and why.
    #[error("cannot {doing}")]
    Failed {
        /// What was being done.
        doing: String,
        /// Why it failed.
        source: Box<Error>,
    },
    /// A failure in a library this one builds on.
    #[error(transparent)]
    Internal(#[from] InternalError),
}

/// A result of this library's calls.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// A demo at `path` with no signon yet.
    pub(crate) fn not_yet(path: &std::path::Path) -> Self {
        Error::Internal(InternalError(error::Inner::NotYet(path.to_owned())))
    }

    /// This error and the ones it was caused by.
    fn chain(&self) -> impl Iterator<Item = &Error> {
        std::iter::successors(Some(self), |e| match e {
            Error::Failed { source, .. } => Some(source),
            _ => None,
        })
    }

    fn inner(&self) -> impl Iterator<Item = &error::Inner> {
        self.chain().filter_map(|e| match e {
            Error::Internal(InternalError(inner)) => Some(inner),
            _ => None,
        })
    }

    /// The server's refusal, if that is what this is.
    pub(crate) fn rejected(&self) -> Option<&net::handshake::Rejected> {
        self.inner().find_map(|i| if let error::Inner::Rejected(r) = i { Some(r) } else { None })
    }

    /// The game port that was given instead of the `SourceTV` port, if that is what this is.
    pub(crate) fn game_port(&self) -> Option<&net::handshake::GamePort> {
        self.inner().find_map(|i| if let error::Inner::GamePort(g) = i { Some(g) } else { None })
    }

    /// Whether this is a stop that was asked for.
    pub(crate) fn is_stopped(&self) -> bool {
        self.inner().any(|i| matches!(i, error::Inner::Stopped(_)))
    }

    /// Whether this is a demo with no signon yet.
    pub(crate) fn is_not_yet(&self) -> bool {
        self.inner().any(|i| matches!(i, error::Inner::NotYet(_)))
    }

    /// The kind of the I/O error that caused this, if one did.
    pub(crate) fn io_kind(&self) -> Option<std::io::ErrorKind> {
        self.chain().find_map(|e| if let Error::Io { source, .. } = e { Some(source.kind()) } else { None })
    }
}
