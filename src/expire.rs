use crate::knock_state::KnockState;
use crate::{ActiveKnocks, KnockKey, KnockStates};
use quanta::Instant;
use std::cmp::{Ordering, Reverse};
use std::collections::binary_heap::PeekMut;
use std::collections::BinaryHeap;
use std::net::IpAddr;
use tracing::{info, instrument, trace};

/// Expiration entry
///
/// Keys are equal if they reference the same knock key,
/// and are ordered based on their expiration time.
struct ExpirationEntry {
    expires: Instant,
    knock_key: KnockKey,
    addr: IpAddr,
}

impl PartialEq for ExpirationEntry {
    fn eq(&self, other: &Self) -> bool {
        self.knock_key == other.knock_key
    }
}

impl Eq for ExpirationEntry {}

impl PartialOrd for ExpirationEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ExpirationEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.expires.cmp(&other.expires)
    }
}

/// Expiration queue
#[derive(Default)]
pub struct ExpirationQueue {
    // We use a Reverse here as std's BinaryHeap is a max-heap, while we want a min-heap
    inner: BinaryHeap<Reverse<ExpirationEntry>>,
}

impl ExpirationQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, expires: Instant, knock_key: KnockKey, addr: IpAddr) {
        self.inner.push(Reverse(ExpirationEntry {
            expires,
            knock_key,
            addr,
        }))
    }

    /// Attempt to clean the next expired entry from our queue
    ///
    /// Returns true if an entry was cleaned (even if it was stale)
    #[instrument(skip_all)]
    pub fn try_clean_next(
        &mut self,
        knock_states: &mut KnockStates,
        active_knocks: &mut ActiveKnocks,
    ) -> bool {
        let now = Instant::recent();

        let Some(item) = self.inner.peek_mut() else {
            return false;
        };

        if item.0.expires > now {
            return false;
        }

        let Reverse(entry) = PeekMut::pop(item);

        if let Some(knock) = knock_states.remove(entry.knock_key) {
            active_knocks.remove(&entry.addr);

            if matches!(knock, KnockState::Passed { .. }) {
                // Note: this doesn't actually close the door, this is done in the knock validation stage.
                // That being said, this does speed that check up.
                info!(addr = %entry.addr, "Door closed");
            } else {
                info!(addr = %entry.addr, "Cleaned knock attempts");
            }
        } else {
            trace!(addr = %entry.addr, "Cleaned stale entry");
        }

        true
    }
}
