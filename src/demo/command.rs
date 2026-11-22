//! The kinds of a demo's packets, and their sizes.

/// A packet's command info: flags, view origins and angles, then the in and out sequence numbers.
pub(super) const META_LEN: usize = 84;

/// Where the view angles are in it: after the flags and the first view origin.
pub(super) const VIEW_ANGLES: usize = 16;

/// Longer than any packet a demo has: a longer length is corrupt and is not read into memory.
pub(super) const MAX_PACKET: usize = 1 << 26;

pub(super) const SIGNON: u8 = 1;

pub(super) const MESSAGE: u8 = 2;

pub(super) const SYNC_TICK: u8 = 3;

pub(super) const CONSOLE_CMD: u8 = 4;

pub(super) const USER_CMD: u8 = 5;

pub(super) const DATA_TABLES: u8 = 6;

pub(super) const STOP: u8 = 7;

pub(super) const STRING_TABLES: u8 = 8;
