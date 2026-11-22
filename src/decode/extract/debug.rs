//! `STVTF_DEBUG`: debug output a developer can ask the extractor to print as it runs.

use std::sync::LazyLock;

/// `STVTF_DEBUG`, read once: a server class, temp entity class, `ent:N`, `gameevents`,
/// `convars` or `errors`.
pub(super) static DEBUG: LazyLock<Option<String>> = LazyLock::new(|| std::env::var("STVTF_DEBUG").ok());

/// Whether `STVTF_DEBUG` asks for this topic.
pub(crate) fn debug(topic: &str) -> bool {
    DEBUG.as_deref() == Some(topic)
}

/// The entity `STVTF_DEBUG=ent:N` asks to trace.
pub(super) fn debug_entity() -> Option<u32> {
    DEBUG.as_deref()?.strip_prefix("ent:")?.parse().ok()
}
