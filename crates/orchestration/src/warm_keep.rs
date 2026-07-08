//! PTT warm-keep policy (ADR-030/Q36, doc 04 §5, doc 07 §6).
//!
//! A single push-to-talk press cold-loads the STT sidecar, and left alone the
//! idle sweep unloads it 60 s later ([`crate::model_lifecycle::IDLE_UNLOAD`]).
//! But a *voice burst* — several queries in a short span — would then pay the
//! cold-load SLA (< 2 s, doc 04 §5) on every press. ADR-030/Q36's heuristic:
//! **≥ 2 PTT presses within a trailing 5-minute window pin STT warm** (skip the
//! idle-unload), and it un-pins once the presses age out of the window.
//!
//! This tracker only *decides* warm-keep; the caller (the voice PTT handler, M6)
//! drives [`crate::model_lifecycle::ModelLifecycle::set_warm_kept`] from the
//! boolean it returns, and the idle sweep honors the pin. Pure logic, no clock of
//! its own — every timestamp is the caller's epoch ms, the same clock the sweep
//! uses (doc 04 §5), so the window is coherent across the two.

use std::collections::VecDeque;
use std::time::Duration;

/// Trailing window over which PTT presses are counted (ADR-030/Q36).
pub const WINDOW: Duration = Duration::from_secs(5 * 60);
/// Presses within [`WINDOW`] at or above which STT is pinned warm (ADR-030/Q36).
pub const THRESHOLD: usize = 2;

/// Counts recent PTT presses in a trailing [`WINDOW`] and reports whether STT
/// should be warm-kept (≥ [`THRESHOLD`] presses).
#[derive(Debug, Default)]
pub struct PttWarmKeep {
    /// Epoch-ms timestamps of presses still inside the trailing window, oldest
    /// first. Bounded by [`THRESHOLD`]: once the pin is on, older presses beyond
    /// the ones needed to hold it are pruned lazily, so this never grows unbounded
    /// under a sustained burst.
    presses: VecDeque<i64>,
}

impl PttWarmKeep {
    pub fn new() -> Self {
        Self { presses: VecDeque::new() }
    }

    /// Record a PTT press at `now_ms` and return whether STT should now be
    /// warm-kept. Call [`ModelLifecycle::set_warm_kept`] with the result.
    ///
    /// [`ModelLifecycle::set_warm_kept`]: crate::model_lifecycle::ModelLifecycle::set_warm_kept
    pub fn record_press(&mut self, now_ms: i64) -> bool {
        self.presses.push_back(now_ms);
        self.warm_at(now_ms)
    }

    /// Recompute warm-keep at `now_ms` *without* a new press — for the idle sweep,
    /// so a pin lapses as the last presses age out of the window.
    pub fn is_warm(&mut self, now_ms: i64) -> bool {
        self.warm_at(now_ms)
    }

    /// Presses currently inside the trailing window (test/telemetry visibility).
    pub fn presses_in_window(&mut self, now_ms: i64) -> usize {
        self.prune(now_ms);
        self.presses.len()
    }

    fn warm_at(&mut self, now_ms: i64) -> bool {
        self.prune(now_ms);
        // Keep memory bounded: only the most recent THRESHOLD presses can matter
        // for the pin decision, so drop any excess from the front.
        while self.presses.len() > THRESHOLD {
            self.presses.pop_front();
        }
        self.presses.len() >= THRESHOLD
    }

    fn prune(&mut self, now_ms: i64) {
        let floor = now_ms - WINDOW.as_millis() as i64;
        while self.presses.front().is_some_and(|&t| t < floor) {
            self.presses.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_press_does_not_pin() {
        let mut w = PttWarmKeep::new();
        assert!(!w.record_press(0), "a lone PTT press idles out normally");
    }

    #[test]
    fn two_presses_within_the_window_pin_warm() {
        let mut w = PttWarmKeep::new();
        assert!(!w.record_press(0));
        assert!(
            w.record_press(60_000),
            "two presses one minute apart pin STT (ADR-030/Q36)"
        );
    }

    #[test]
    fn a_press_outside_the_window_does_not_count() {
        let mut w = PttWarmKeep::new();
        assert!(!w.record_press(0));
        // Second press just over 5 min later: the first has aged out, so this is
        // effectively a fresh lone press.
        let later = WINDOW.as_millis() as i64 + 1;
        assert!(!w.record_press(later), "the aged-out first press must not pin");
    }

    #[test]
    fn the_pin_lapses_as_presses_age_out() {
        let mut w = PttWarmKeep::new();
        w.record_press(0);
        assert!(w.record_press(1_000), "two quick presses pin");
        // 5 min + 1 ms after the *last* press, both have aged out.
        let expired = 1_000 + WINDOW.as_millis() as i64 + 1;
        assert!(!w.is_warm(expired), "the pin lapses once the burst ages out");
    }

    #[test]
    fn memory_stays_bounded_under_a_sustained_burst() {
        let mut w = PttWarmKeep::new();
        for i in 0..1_000 {
            w.record_press(i * 100);
        }
        assert!(
            w.presses_in_window(1_000 * 100) <= THRESHOLD,
            "the deque never grows past THRESHOLD under a burst"
        );
    }
}
