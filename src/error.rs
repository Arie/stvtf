//! What the library's calls return when they fail: an [`Error`] of a kind to act on, saying what
//! happened and why.

use std::fmt;

/// A result of the library's calls.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What went wrong: its [`ErrorKind`], what happened, and its causes ([`std::error::Error::source`]).
pub struct Error {
    kind: ErrorKind,
    inner: anyhow::Error,
}

/// What kind of failure an [`Error`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Reading or writing a file or a socket, or resolving an address.
    Io,
    /// Data that is not what it should be: a demo, a capture or tables that are cut short or
    /// broken, or an argument out of range.
    Invalid,
    /// A build with no tables compiled in, and no demo of it given.
    UnknownBuild,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, inner: impl Into<anyhow::Error>) -> Self {
        Error { kind, inner: inner.into() }
    }

    /// What kind of failure this is.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

/// Of an I/O error if one caused it, else of data that is not what it should be.
impl From<anyhow::Error> for Error {
    fn from(inner: anyhow::Error) -> Self {
        let io = inner.chain().any(<dyn std::error::Error>::is::<std::io::Error>);
        Error { kind: if io { ErrorKind::Io } else { ErrorKind::Invalid }, inner }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::new(ErrorKind::Io, e)
    }
}

/// Errors of what the library reads with: of data that is not what it should be.
macro_rules! invalid {
    ($($t:ty),*) => {$(
        impl From<$t> for Error {
            fn from(e: $t) -> Self {
                Error::new(ErrorKind::Invalid, e)
            }
        }
    )*};
}
invalid!(std::array::TryFromSliceError, bitbuffer::BitError, tf_demo_parser::ParseError);

impl From<ctrlc::Error> for Error {
    fn from(e: ctrlc::Error) -> Self {
        Error::new(ErrorKind::Io, e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.inner, f)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {:?}", self.kind, self.inner)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        AsRef::<dyn std::error::Error + Send + Sync>::as_ref(&self.inner).source()
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Context;

    use super::*;

    #[test]
    fn an_error_says_what_and_why_and_is_of_its_cause() {
        let io: anyhow::Result<()> = Err(std::io::Error::other("disk full")).context("writing a demo");
        let e = Error::from(io.unwrap_err());
        assert_eq!(e.kind(), ErrorKind::Io);
        assert_eq!(e.to_string(), "writing a demo");
        assert_eq!(format!("{e:#}"), "writing a demo: disk full");
        assert_eq!(format!("{:#}", anyhow::Error::from(e)), "writing a demo: disk full");
        assert_eq!(Error::from(anyhow::anyhow!("not a demo")).kind(), ErrorKind::Invalid);
    }
}
