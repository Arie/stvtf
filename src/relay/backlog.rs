//! The backlog: the reliable messages each client is given in order from its signon on, the
//! server's signon and the string table changes since.

use super::client::{Client, Phase};
use crate::net::bits::BitBuf;
use crate::net::messages::Signon;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;

/// A reliable message a client signing on needs, and the signon state it waits for after it.
pub(super) struct Entry {
    pub(super) bits: BitBuf,
    pub(super) gate: Option<Signon>,
}

pub(super) struct Backlog {
    entries: Vec<Entry>,
    /// The string table changes in it: a server's unreliable parts come again until acked, and
    /// the relay's own are added once.
    tables: HashSet<BitBuf>,
    /// The last string table change added.
    last_table: Option<BitBuf>,
    /// Every reliable message joins it until the first entities after a signon start.
    pub(super) open: bool,
}

/// How a string table change added to the backlog reaches the clients past their signon.
#[derive(Clone, Copy)]
pub(super) enum Delivery {
    /// It came in a datagram's unreliable part, which the clients in the game get as it is; with
    /// `queue`, reliably as well (a demo has it once: a lost datagram would lose it for good).
    InDatagrams { queue: bool },
    /// It was passed over (a jump): reliably.
    Reliably,
}

impl Backlog {
    pub(super) fn new() -> Self {
        Backlog { entries: Vec::new(), tables: HashSet::new(), last_table: None, open: true }
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn get(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    pub(super) fn push(&mut self, bits: BitBuf, gate: Option<Signon>) {
        self.entries.push(Entry { bits, gate });
    }

    pub(super) fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    /// An entry of the backlog before (a handover's), a string table change known as one.
    pub(super) fn restore(&mut self, bits: BitBuf, gate: Option<Signon>) {
        if super::handover::is_table_change(&bits) {
            self.tables.insert(bits.clone());
            self.last_table = Some(bits.clone());
        }
        self.push(bits, gate);
    }

    /// Empty and open: the next signon starts afresh.
    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.tables.clear();
        self.last_table = None;
        self.open = true;
    }

    pub(super) fn has_table_change(&self, msg: &BitBuf) -> bool {
        self.tables.contains(msg)
    }

    /// Those string table changes are added again when they come again.
    pub(super) fn forget_table_changes<'a>(&mut self, msgs: impl IntoIterator<Item = &'a BitBuf>) {
        for m in msgs {
            self.tables.remove(m);
        }
    }

    /// Adds string table change `msg` and gives it to the clients: those at the end of the backlog
    /// as `delivery` says, the others as they get to it. One the same as an earlier one is a change
    /// too, unless that was the last (a server's repeat).
    pub(super) fn add_table_change(
        &mut self,
        msg: &BitBuf,
        clients: &mut HashMap<SocketAddr, Client>,
        delivery: Delivery,
    ) {
        if self.last_table.as_ref() == Some(msg) {
            return;
        }
        self.last_table = Some(msg.clone());
        self.tables.insert(msg.clone());
        self.push(msg.clone(), None);
        for c in clients.values_mut().filter(|c| !c.held) {
            let at_end = c.cursor + 1 == self.len();
            match delivery {
                Delivery::InDatagrams { queue } if c.phase == Phase::Active && at_end => {
                    c.cursor += 1;
                    if queue {
                        c.queue.push(msg.clone());
                    }
                }
                Delivery::Reliably if c.phase != Phase::Signon && at_end => {
                    c.queue.push(msg.clone());
                    c.cursor += 1;
                }
                _ => c.pump(self),
            }
        }
    }
}
