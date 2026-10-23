/// Room for any UDP datagram.
pub const MAX_DATAGRAM: usize = 65536;

pub mod bits;
pub mod compress;
pub mod handshake;
pub mod messages;
pub mod netchan;
pub mod receiver;
pub mod reliable;
pub mod split;
