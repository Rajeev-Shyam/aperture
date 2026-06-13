//! Sessionization (doc 08 §3).
//!
//! A new `session_id` starts after [`config::SESSION_GAP_MIN`] minutes of no
//! input activity `[ASSUMPTION]`. Sessions bound n-gram extraction so overnight
//! gaps don't fabricate sequences (doc 08 §3-§4). The assigned id is written
//! back onto [`Event::session_id`] (doc 15 §1 / event.rs).

use aperture_contracts::event::Event;

use crate::config;

/// Tracks the current session boundary by wall-clock gap (doc 08 §3).
#[derive(Debug, Default)]
pub struct Sessionizer {
    /// Current session id; `None` until the first event is seen.
    current_session: Option<i64>,
    /// `ts` (epoch ms) of the last event assigned to a session.
    last_event_ts: Option<i64>,
}

impl Sessionizer {
    /// Fresh sessionizer with no open session.
    pub fn new() -> Self {
        Self::default()
    }

    /// Assign a `session_id` for `ev`, rolling to a new session when the gap
    /// since the previous event exceeds [`config::SESSION_GAP_MIN`] (doc 08 §3).
    ///
    /// Returns the id and (as a side effect) advances the internal clock.
    pub fn assign(&mut self, _ev: &Event) -> i64 {
        // TODO(M3): if last_event_ts is None or (ev.ts - last_event_ts) >
        // SESSION_GAP_MIN*60_000, allocate a new session id (monotonic, doc 03);
        // else reuse current. Update last_event_ts = ev.ts. The 15-min gap is
        // wall-clock (doc 08 §9 keeps temporal buckets on local wall-clock too).
        let _gap_ms = config::SESSION_GAP_MIN * 60_000;
        todo!("M3: gap-based session rollover (doc 08 §3)")
    }

    /// The id of the currently open session, if any.
    pub fn current(&self) -> Option<i64> {
        self.current_session
    }
}
