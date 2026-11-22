//! The crate's [`Error`]s as they go up: what was being done, and why it failed.

use std::fmt::Display;

use crate::Error;

/// What a bare I/O error was doing, until a [`Context`] says.
const READ_OR_WRITE: &str = "read or write";

use crate::net::handshake::{GamePort, Rejected, Stopped};

/// A failure inside this library or a dependency, kept out of the API.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct InternalError(pub(crate) Inner);

#[derive(Debug, thiserror::Error)]
pub(crate) enum Inner {
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

/// An [`Error::Invalid`], formatted as `format!` does.
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

/// A `None` is the error: the argument says what was missing.
impl<T> Context<T> for Option<T> {
    fn context(self, missing: impl Display) -> crate::Result<T> {
        self.ok_or_else(|| invalid!("{missing}"))
    }

    fn with_context<D: Display>(self, missing: impl FnOnce() -> D) -> crate::Result<T> {
        self.ok_or_else(|| invalid!("{}", missing()))
    }
}

/// `source` as why `doing` failed; a bare I/O error takes `doing` as its own.
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
