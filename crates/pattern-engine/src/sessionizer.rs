//! Sessionization (doc 08 §3, ADR-032/Q28).
//!
//! A new `session_id` starts on an idle gap decided by a **rolling idle-gap
//! distribution** (applied forward — never retro-sessionizing), with
//! [`config::SESSION_GAP_COLD_START_MIN`] minutes as the cold-start default
//! `[ASSUMPTION]`. Sessions bound n-gram extraction so overnight gaps don't
//! fabricate sequences (doc 08 §3-§4). The assigned id is written back onto
//! [`Event::session_id`] (doc 15 §1 / event.rs).
//!
//! ## The rolling threshold (ADR-032)
//! We keep a bounded reservoir of observed inter-event gaps that were *not*
//! session breaks (gaps under the current threshold) plus the breaks themselves,
//! and set the threshold at a high quantile of the "working" gaps — bounded to
//! `[MIN_GAP, MAX_GAP]` so a pathological stream can't collapse or explode
//! sessions. Cold start (< [`MIN_SAMPLES`] samples) uses the fixed default.

use aperture_contracts::event::Event;

use crate::config;

/// Bounds on the adaptive gap threshold (ADR-032: bounded, self-adjusting).
/// `[ASSUMPTION — tuned at M3]`.
const MIN_GAP_MS: i64 = 5 * 60_000; // never below 5 min
const MAX_GAP_MS: i64 = 45 * 60_000; // never above 45 min
/// Gap samples required before the adaptive threshold replaces the cold-start
/// default. `[ASSUMPTION]`.
const MIN_SAMPLES: usize = 200;
/// Bounded reservoir size for working-gap samples.
const RESERVOIR: usize = 1024;
/// The quantile of working gaps the threshold sits at. `[ASSUMPTION]`: the 99th
/// percentile of within-session gaps ≈ "anything longer is a break".
const QUANTILE: f64 = 0.99;

/// Tracks the current session boundary by wall-clock gap (doc 08 §3).
#[derive(Debug, Default)]
pub struct Sessionizer {
    /// Current session id; `None` until the first event is seen.
    current_session: Option<i64>,
    /// `ts` (epoch ms) of the last event assigned to a session.
    last_event_ts: Option<i64>,
    /// Monotonic id source (persisted ids come from the DB row max at hydrate).
    next_id: i64,
    /// Bounded sample of observed *working* gaps (ms), ring-buffer semantics.
    working_gaps: Vec<i64>,
    /// Ring cursor into `working_gaps` once full.
    cursor: usize,
}

impl Sessionizer {
    /// Fresh sessionizer with no open session.
    pub fn new() -> Self {
        Self {
            next_id: 1,
            ..Self::default()
        }
    }

    /// Hydrate the id source so new sessions continue after the DB's max
    /// (doc 03: `session_id` is monotonic).
    pub fn with_next_id(next_id: i64) -> Self {
        Self {
            next_id: next_id.max(1),
            ..Self::default()
        }
    }

    /// The gap (ms) that currently constitutes a session break (ADR-032):
    /// adaptive once warmed up, cold-start default before that, always clamped
    /// to `[MIN_GAP_MS, MAX_GAP_MS]`.
    pub fn current_gap_threshold_ms(&self) -> i64 {
        if self.working_gaps.len() < MIN_SAMPLES {
            return config::SESSION_GAP_COLD_START_MIN * 60_000;
        }
        let mut sorted = self.working_gaps.clone();
        sorted.sort_unstable();
        let idx = ((sorted.len() as f64 - 1.0) * QUANTILE).round() as usize;
        sorted[idx].clamp(MIN_GAP_MS, MAX_GAP_MS)
    }

    /// Assign a `session_id` for `ev`, rolling to a new session when the gap
    /// since the previous event exceeds [`Self::current_gap_threshold_ms`]
    /// (doc 08 §3, ADR-032 — forward-applied only).
    ///
    /// Returns the id and (as a side effect) advances the internal clock and
    /// folds the observed gap into the rolling distribution.
    pub fn assign(&mut self, ev: &Event) -> i64 {
        let threshold = self.current_gap_threshold_ms();
        let rolled = match self.last_event_ts {
            None => true,
            Some(prev) => ev.ts.saturating_sub(prev) > threshold,
        };

        if let Some(prev) = self.last_event_ts {
            let gap = ev.ts.saturating_sub(prev);
            if !rolled && gap > 0 {
                // A working (within-session) gap — feed the distribution.
                if self.working_gaps.len() < RESERVOIR {
                    self.working_gaps.push(gap);
                } else {
                    self.working_gaps[self.cursor] = gap;
                    self.cursor = (self.cursor + 1) % RESERVOIR;
                }
            }
        }

        if rolled {
            let id = self.next_id;
            self.next_id += 1;
            self.current_session = Some(id);
        }
        self.last_event_ts = Some(ev.ts);
        self.current_session.expect("session opened above")
    }

    /// The id of the currently open session, if any.
    pub fn current(&self) -> Option<i64> {
        self.current_session
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_contracts::EventType;

    fn ev_at(ts: i64) -> Event {
        Event {
            id: 0,
            ts,
            r#type: EventType::WindowFocus,
            app: None,
            process: Some("x.exe".into()),
            window_title: None,
            payload: serde_json::json!({}),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        }
    }

    #[test]
    fn cold_start_uses_15_min_gap() {
        let mut s = Sessionizer::new();
        let a = s.assign(&ev_at(0));
        let b = s.assign(&ev_at(14 * 60_000)); // inside 15 min
        assert_eq!(a, b, "within the cold-start gap → same session");
        let c = s.assign(&ev_at(14 * 60_000 + 16 * 60_000)); // > 15 min later
        assert_ne!(b, c, "past the gap → new session (doc 08 §3)");
    }

    #[test]
    fn ids_are_monotonic_and_forward_only() {
        let mut s = Sessionizer::new();
        let a = s.assign(&ev_at(0));
        let b = s.assign(&ev_at(100 * 60_000));
        let c = s.assign(&ev_at(200 * 60_000));
        assert!(a < b && b < c, "sessions only roll forward (ADR-032)");
    }

    #[test]
    fn adaptive_threshold_stays_bounded() {
        let mut s = Sessionizer::new();
        // Warm the distribution with tiny gaps (1 s each).
        let mut ts = 0;
        s.assign(&ev_at(ts));
        for _ in 0..(MIN_SAMPLES + 10) {
            ts += 1_000;
            s.assign(&ev_at(ts));
        }
        let th = s.current_gap_threshold_ms();
        assert!(
            th >= MIN_GAP_MS,
            "threshold clamped to the floor even when all gaps are tiny (got {th})"
        );
        assert!(th <= MAX_GAP_MS);
    }
}
