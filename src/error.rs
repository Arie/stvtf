//! Building the crate's [`Error`]s as they go up: what was being done, and why it failed.

use std::fmt::Display;

use crate::Error;

/// What a bare I/O error was doing, until a [`Context`] says.
const READ_OR_WRITE: &str = "read or write";

use crate::net::handshake::{GamePort, Rejected, Stopped};

/// A failure inside this library or one it builds on, kept out of this library's API.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct InternalError(Inner);

#[derive(Debug, thiserror::Error)]
enum Inner {
    #[error(transparent)]
    Bits(bitbuffer::BitError),
    #[error(transparent)]
    Parse(tf_demo_parser::ParseError),
    #[error(transparent)]
    Signal(ctrlc::Error),
    #[error(transparent)]
    Snappy(snap::Error),
    #[error(transparent)]
    GamePort(GamePort),
    #[error(transparent)]
    Rejected(Rejected),
    #[error(transparent)]
    Stopped(Stopped),
    /// A demo with no signon yet.
    #[error("{} has no signon yet", .0.display())]
    NotYet(std::path::PathBuf),
}

/// Each dependency's error, as an [`Error::Internal`].
macro_rules! internal {
    ($($variant:ident($t:ty)),*) => {$(
        impl From<$t> for Error {
            fn from(e: $t) -> Self {
                Error::Internal(InternalError(Inner::$variant(e)))
            }
        }
    )*};
}
internal!(
    Bits(bitbuffer::BitError),
    Parse(tf_demo_parser::ParseError),
    Signal(ctrlc::Error),
    Snappy(snap::Error),
    GamePort(GamePort),
    Rejected(Rejected),
    Stopped(Stopped)
);

impl From<std::num::TryFromIntError> for Error {
    fn from(e: std::num::TryFromIntError) -> Self {
        Error::Invalid { reason: format!("cannot take a value: {e}") }
    }
}

impl Error {
    /// A demo at `path` with no signon yet.
    pub(crate) fn not_yet(path: &std::path::Path) -> Self {
        Error::Internal(InternalError(Inner::NotYet(path.to_owned())))
    }

    /// This error and the ones it was caused by.
    fn chain(&self) -> impl Iterator<Item = &Error> {
        std::iter::successors(Some(self), |e| match e {
            Error::Failed { source, .. } => Some(source),
            _ => None,
        })
    }

    fn inner(&self) -> impl Iterator<Item = &Inner> {
        self.chain().filter_map(|e| match e {
            Error::Internal(InternalError(inner)) => Some(inner),
            _ => None,
        })
    }

    /// The server's refusal, if that is what this is.
    pub(crate) fn rejected(&self) -> Option<&Rejected> {
        self.inner().find_map(|i| if let Inner::Rejected(r) = i { Some(r) } else { None })
    }

    /// The game port given for `SourceTV`'s, if that is what this is.
    pub(crate) fn game_port(&self) -> Option<&GamePort> {
        self.inner().find_map(|i| if let Inner::GamePort(g) = i { Some(g) } else { None })
    }

    /// Whether this is a stop asked for.
    pub(crate) fn is_stopped(&self) -> bool {
        self.inner().any(|i| matches!(i, Inner::Stopped(_)))
    }

    /// Whether this is a demo with no signon yet.
    pub(crate) fn is_not_yet(&self) -> bool {
        self.inner().any(|i| matches!(i, Inner::NotYet(_)))
    }

    /// The kind of the I/O error that caused this, if one did.
    pub(crate) fn io_kind(&self) -> Option<std::io::ErrorKind> {
        self.chain().find_map(|e| if let Error::Io { source, .. } = e { Some(source.kind()) } else { None })
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io { doing: READ_OR_WRITE.to_string(), source }
    }
}

impl From<std::array::TryFromSliceError> for Error {
    fn from(e: std::array::TryFromSliceError) -> Self {
        Error::Invalid { reason: format!("cannot take a value: {e}") }
    }
}

/// An [`Error::Invalid`] saying what is wrong, formatted as `format!` does.
macro_rules! invalid {
    ($($arg:tt)*) => {
        $crate::Error::Invalid { reason: format!($($arg)*) }
    };
}

/// Returns an [`Error::Invalid`] saying what is wrong.
macro_rules! bail {
    ($($arg:tt)*) => {
        return Err($crate::error::invalid!($($arg)*))
    };
}

pub(crate) use bail;
pub(crate) use invalid;

/// What was being done when a result failed: "cannot {doing}", and why.
pub(crate) trait Context<T> {
    /// Says what was being done (`doing`, a verb phrase: "read the header").
    fn context(self, doing: impl Display) -> crate::Result<T>;

    /// As [`Context::context`], with `doing` made only on failure.
    fn with_context<D: Display>(self, doing: impl FnOnce() -> D) -> crate::Result<T>;
}

impl<T, E: Into<Error>> Context<T> for Result<T, E> {
    fn context(self, doing: impl Display) -> crate::Result<T> {
        self.map_err(|e| because(doing.to_string(), e.into()))
    }

    fn with_context<D: Display>(self, doing: impl FnOnce() -> D) -> crate::Result<T> {
        self.map_err(|e| because(doing().to_string(), e.into()))
    }
}

/// Nothing there is what is wrong: `doing` says what was missing.
impl<T> Context<T> for Option<T> {
    fn context(self, missing: impl Display) -> crate::Result<T> {
        self.ok_or_else(|| invalid!("{missing}"))
    }

    fn with_context<D: Display>(self, missing: impl FnOnce() -> D) -> crate::Result<T> {
        self.ok_or_else(|| invalid!("{}", missing()))
    }
}

/// `source` as the cause of failing to do `doing`: a bare I/O error takes `doing` as its own.
fn because(doing: String, source: Error) -> Error {
    match source {
        Error::Io { doing: bare, source } if bare == READ_OR_WRITE => Error::Io { doing, source },
        source => Error::Failed { doing, source: Box::new(source) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(e: &dyn std::error::Error) -> String {
        let mut said = e.to_string();
        let mut at = e.source();
        while let Some(cause) = at {
            said = format!("{said}: {cause}");
            at = cause.source();
        }
        said
    }

    #[test]
    fn an_error_says_what_could_not_be_done_and_why() {
        let io: Result<(), _> = Err(std::io::Error::other("disk full"));
        let e = io.context("write the demo").unwrap_err();
        assert!(matches!(e, Error::Io { .. }));
        assert_eq!(chain(&e), "cannot write the demo: disk full");
        let deeper = Err::<(), _>(e).context("record the match").unwrap_err();
        assert_eq!(chain(&deeper), "cannot record the match: cannot write the demo: disk full");
        let missing = None::<u8>.context("the demo has no data tables").unwrap_err();
        assert_eq!(missing.to_string(), "the demo has no data tables");
    }
}
