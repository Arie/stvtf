//! A signon's data tables, held until a demo can take them.

use crate::net::messages::{SVC_CLASSINFO, TYPE_BITS};

/// Whether a signon packet opens with the class list (`svc_ClassInfo`). The data tables go just
/// before that one: TF2 playing a demo itself takes them nowhere else (first of all, it crashes
/// once loaded).
pub(crate) fn opens_with_classes(signon: &[u8]) -> bool {
    signon.first().is_some_and(|b| u32::from(*b) & ((1 << TYPE_BITS) - 1) == SVC_CLASSINFO)
}

/// Data tables waiting for their place in a signon: just before the packet that opens with the
/// class list, else at its end.
pub(crate) struct HeldTables<T>(Option<T>);

impl<T> HeldTables<T> {
    pub(crate) fn new(tables: Option<T>) -> Self {
        HeldTables(tables)
    }

    /// The tables, if they go before signon packet `signon`.
    pub(crate) fn before(&mut self, signon: &[u8]) -> Option<T> {
        if opens_with_classes(signon) { self.0.take() } else { None }
    }

    /// The tables, if they are still to go: the signon ends here.
    pub(crate) fn at_end(&mut self) -> Option<T> {
        self.0.take()
    }
}
