//! Locks that still work after a thread panicked while holding them.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `m`, even if a thread panicked while holding it.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
