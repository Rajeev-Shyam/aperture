//! In-process event bus (doc 15 §1, doc 02 §8): a thin wrapper over
//! `tokio::sync::broadcast` carrying the normalized [`Event`] envelope.
//!
//! **The bus is at-most-once; SQLite is the truth** (doc 15 §1). A slow or
//! lagging subscriber may miss messages (`broadcast` drops the oldest on
//! overflow) — that is *by design*. Anything that must survive a miss is read
//! back from the durable `events` table (doc 03), never replayed off the bus.
//!
//! Transport boundary (doc 02 §8): Tier-0 components talk over this in-process
//! bus; the Tauri `invoke`/event channel bridges core <-> WebView separately.
//! This crate is runtime plumbing only — it imports the [`Event`] contract and
//! adds no schema of its own.

use aperture_contracts::Event;
use tokio::sync::broadcast;

/// Default channel capacity (number of buffered events per subscriber lag
/// window). When a receiver falls this far behind, `broadcast` drops the oldest
/// messages and the receiver observes a [`broadcast::error::RecvError::Lagged`]
/// — consistent with the at-most-once contract (doc 15 §1).
// TODO(M0:) tune capacity against the focus-storm burst budget (doc 04 §8 /
// doc 05 debounce); 1024 is a placeholder. [VERIFY]
pub const DEFAULT_CAPACITY: usize = 1024;

/// The shared event bus. Clone freely — every clone publishes to the same
/// underlying channel. Subscribers are created on demand via [`EventBus::subscribe`].
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<Event>,
}

impl EventBus {
    /// Create a bus with [`DEFAULT_CAPACITY`].
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// Create a bus with an explicit per-subscriber buffer capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, _rx) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Publish an [`Event`] to all current subscribers.
    ///
    /// Returns the number of subscribers that received it. An error
    /// ([`broadcast::error::SendError`]) means there are currently no
    /// subscribers — **not a durability failure**: durability is SQLite's job
    /// (doc 15 §1), so callers typically ignore this result.
    // TODO(M0:) the Tier-0 single-writer pipeline persists to `events` first,
    // then publishes here (doc 02 §7, doc 03 §1). Wire that ordering at the call
    // site so the durable form is always written before the at-most-once notify.
    pub fn publish(&self, event: Event) -> Result<usize, broadcast::error::SendError<Event>> {
        self.sender.send(event)
    }

    /// Subscribe to the bus. The returned [`broadcast::Receiver`] sees only
    /// events published *after* it is created (at-most-once; no backfill — read
    /// history from SQLite instead, doc 03 §1).
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }

    /// Current subscriber count (diagnostics / tests).
    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
