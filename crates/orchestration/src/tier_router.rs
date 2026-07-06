//! Tier routing: the VLM wake gate + the explicit-reasoning hand-off (doc 12 §2,
//! doc 06 §4, doc 02 §8).
//!
//! Two jobs:
//! 1. Apply doc 06 §4's **wake gate** — decide whether an event/OCR pair earns a
//!    Tier-1 VLM job, and enforce the **adaptive 3–10/h wake band** whose hard
//!    ceiling protects voice (ADR-032). VLM output **never gates a bubble** (doc
//!    02 Path A invariant, doc 06 §4): it enriches the *next* cycle, not this one.
//! 2. Route **explicit** reasoning (Path D, doc 02 §6) to the reasoning gateway —
//!    **never** the proactive loop. Anything -> Tier 2 passes only through the
//!    gateway after the transparency gate (invariant 2, doc 02 §8). This crate
//!    holds **no** gateway/network handle; it returns a routing *decision* the
//!    shell forwards.

use std::collections::HashMap;

use aperture_contracts::Event;

/// Adaptive wake-rate band the M5 gate asserts (ADR-032): floor ~3/hr
/// (cold-start), **hard ceiling ~10/hr** (non-negotiable — protects voice).
/// Value-driven: raised only when VLM-enriched suggestions out-click un-enriched.
pub const WAKES_PER_HOUR_FLOOR: u32 = 3;
/// See [`WAKES_PER_HOUR_FLOOR`]. `[ASSUMPTION; tune at M5]`.
pub const WAKES_PER_HOUR_CEILING: u32 = 10;
/// Per-app anti-thrash debounce on VLM wakes (doc 06 §4).
pub const WAKE_DEBOUNCE_PER_APP_SECS: u64 = 30;
/// The trailing window over which the wake-rate ceiling is enforced.
const RATE_WINDOW_MS: i64 = 60 * 60 * 1000;

/// Why the VLM was (or was not) woken — logged for tuning (doc 06 §4) and fed to
/// [`crate::telemetry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeReason {
    /// (a) the pattern engine asked for disambiguation (doc 06 §4, doc 08).
    PatternDisambiguation,
    /// (b) rich frame, weak OCR: `confidence < 0.55 && text_density > LOW` (doc 06 §4).
    WeakOcrRichFrame,
    /// (c) an explicit user request, e.g. enrichment "add scene summary" (doc 06 §4).
    UserExplicit,
}

impl WakeReason {
    /// Stable label for telemetry (doc 06 §4 logs reasons for tuning).
    pub fn as_str(self) -> &'static str {
        match self {
            WakeReason::PatternDisambiguation => "pattern_disambiguation",
            WakeReason::WeakOcrRichFrame => "weak_ocr_rich_frame",
            WakeReason::UserExplicit => "user_explicit",
        }
    }
}

/// The gate's verdict (doc 06 §4). `Wake` carries the reason for telemetry +
/// logging; every `Skip` reason is also logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeDecision {
    Wake(WakeReason),
    /// Capture off, mutex unlikely free, debounced, over the hourly ceiling, no
    /// trigger, or the R1 projection would fail (doc 06 §4 / doc 04 R1). OCR-only
    /// is the contract.
    Skip(SkipReason),
}

/// Why a wake was skipped — logged for tuning (doc 06 §4: every Skip is logged).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    CaptureOff,
    MutexBusy,
    Debounced,
    /// The trailing-hour wake ceiling was hit — voice is protected (ADR-032).
    RateCeiling,
    NoTrigger,
    BudgetRefused,
}

/// Minimal OCR signal the wake gate reads (doc 06 §4). Full OCR output lives in
/// `aperture-vision-ocr`; the router needs only the gate inputs.
#[derive(Debug, Clone, Copy)]
pub struct OcrSignal {
    pub confidence: f32,
    /// Text-density bucket; the gate compares against the doc 06 §4 `LOW` floor.
    pub text_density: f32,
}

/// OCR-confidence ceiling for the "rich frame, weak OCR" trigger (doc 06 §4 (b)).
pub const WEAK_OCR_CONFIDENCE: f32 = 0.55;
/// `LOW` text-density floor for the (b) trigger [ASSUMPTION; tuned at M5].
pub const LOW_TEXT_DENSITY: f32 = 8.0;

/// Where an *explicit* reasoning request must go (Path D, doc 02 §6). This is a
/// routing decision only — the router never opens a socket (invariant 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningRoute {
    /// Hand to the reasoning gateway (the only Tier-2 path, doc 02 §8). The
    /// payload still goes through preview + the transparency gate first (doc 13).
    Gateway,
    /// Never reached by the proactive loop — present only to make the invariant
    /// explicit: the proactive loop has no cloud route (doc 12 §2).
    NeverProactive,
}

/// Applies the doc 06 §4 wake gate and the Path-D routing rule (doc 12 §2). Owns
/// the per-app wake debounce timers and the trailing-hour wake ledger (the hard
/// ceiling that protects voice, ADR-032).
#[derive(Default)]
pub struct TierRouter {
    /// Per-app last-wake epoch ms (WAKE_DEBOUNCE_PER_APP_SECS anti-thrash).
    last_wake_per_app: HashMap<String, i64>,
    /// Wake timestamps in the trailing hour (front = oldest), for the rate ceiling.
    wake_ledger: std::collections::VecDeque<i64>,
}

impl TierRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The doc 06 §4 `should_wake_vlm` gate, operationalized with the ADR-032
    /// hard ceiling:
    /// ```text
    /// if !capture_on            { Skip(CaptureOff) }
    /// if !mutex_likely_free     { Skip(MutexBusy) }
    /// if debounce_active(30 s per app) { Skip(Debounced) }   // anti-thrash
    /// if wakes_last_hour >= CEILING    { Skip(RateCeiling) } // voice protected
    /// trigger = pattern_requested_disambiguation
    ///        || (ocr.confidence < 0.55 && ocr.text_density > LOW)
    ///        || user_explicit_request
    /// if !trigger               { Skip(NoTrigger) }
    /// if !budget_projection_ok  { Skip(BudgetRefused) }
    /// Wake(reason) — and stamp the debounce + ledger
    /// ```
    /// `now_ms` is the caller's clock (monotonic epoch ms); on `Wake` the app's
    /// debounce is stamped and the wake recorded for the rate ceiling.
    #[allow(clippy::too_many_arguments)]
    pub fn should_wake_vlm(
        &mut self,
        ev: &Event,
        ocr: OcrSignal,
        now_ms: i64,
        capture_on: bool,
        mutex_likely_free: bool,
        pattern_requested: bool,
        user_explicit: bool,
        budget_projection_ok: bool,
    ) -> WakeDecision {
        if !capture_on {
            return WakeDecision::Skip(SkipReason::CaptureOff);
        }
        if !mutex_likely_free {
            return WakeDecision::Skip(SkipReason::MutexBusy);
        }
        let app_key = ev
            .app
            .clone()
            .or_else(|| ev.process.clone())
            .unwrap_or_default();
        if let Some(&last) = self.last_wake_per_app.get(&app_key) {
            if now_ms - last < WAKE_DEBOUNCE_PER_APP_SECS as i64 * 1000 {
                return WakeDecision::Skip(SkipReason::Debounced);
            }
        }
        // The hard ceiling protects voice: a "valuable" VLM never starves STT.
        self.evict_stale(now_ms);
        if self.wake_ledger.len() as u32 >= WAKES_PER_HOUR_CEILING {
            return WakeDecision::Skip(SkipReason::RateCeiling);
        }

        let reason = if pattern_requested {
            Some(WakeReason::PatternDisambiguation)
        } else if ocr.confidence < WEAK_OCR_CONFIDENCE && ocr.text_density > LOW_TEXT_DENSITY {
            Some(WakeReason::WeakOcrRichFrame)
        } else if user_explicit {
            Some(WakeReason::UserExplicit)
        } else {
            None
        };
        let Some(reason) = reason else {
            return WakeDecision::Skip(SkipReason::NoTrigger);
        };
        // trigger && budget_projection_ok() (doc 04 R1 via the BudgetEnforcer).
        if !budget_projection_ok {
            return WakeDecision::Skip(SkipReason::BudgetRefused);
        }
        // Commit the wake: stamp the debounce + record for the rate ceiling.
        self.last_wake_per_app.insert(app_key, now_ms);
        self.wake_ledger.push_back(now_ms);
        WakeDecision::Wake(reason)
    }

    /// Wakes in the trailing hour (the M5 gate reads this against the 3–10 band).
    pub fn wakes_last_hour(&mut self, now_ms: i64) -> u32 {
        self.evict_stale(now_ms);
        self.wake_ledger.len() as u32
    }

    fn evict_stale(&mut self, now_ms: i64) {
        while let Some(&front) = self.wake_ledger.front() {
            if now_ms - front >= RATE_WINDOW_MS {
                self.wake_ledger.pop_front();
            } else {
                break;
            }
        }
    }

    /// Route an explicit reasoning request (Path D, doc 02 §6): always the
    /// gateway, never the proactive loop (doc 12 §2). The shell takes this
    /// decision and invokes the gateway after preview + the transparency gate.
    pub fn route_explicit_reasoning(&self) -> ReasoningRoute {
        ReasoningRoute::Gateway
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_contracts::event::EventType;

    fn ev(app: &str) -> Event {
        Event {
            id: 1,
            ts: 0,
            r#type: EventType::WindowFocus,
            app: Some(app.into()),
            process: Some(format!("{app}.exe")),
            window_title: None,
            payload: serde_json::json!({}),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        }
    }

    const RICH_WEAK: OcrSignal = OcrSignal {
        confidence: 0.4,
        text_density: 20.0,
    };

    #[test]
    fn wakes_on_a_rich_weak_frame_when_all_gates_pass() {
        let mut r = TierRouter::new();
        let d = r.should_wake_vlm(&ev("code"), RICH_WEAK, 0, true, true, false, false, true);
        assert_eq!(d, WakeDecision::Wake(WakeReason::WeakOcrRichFrame));
    }

    #[test]
    fn capture_off_and_busy_mutex_and_budget_all_skip() {
        let mut r = TierRouter::new();
        assert_eq!(
            r.should_wake_vlm(&ev("code"), RICH_WEAK, 0, false, true, false, false, true),
            WakeDecision::Skip(SkipReason::CaptureOff)
        );
        assert_eq!(
            r.should_wake_vlm(&ev("code"), RICH_WEAK, 0, true, false, false, false, true),
            WakeDecision::Skip(SkipReason::MutexBusy)
        );
        assert_eq!(
            r.should_wake_vlm(&ev("code"), RICH_WEAK, 0, true, true, false, false, false),
            WakeDecision::Skip(SkipReason::BudgetRefused)
        );
    }

    #[test]
    fn per_app_debounce_blocks_a_second_wake_within_30s() {
        let mut r = TierRouter::new();
        assert!(matches!(
            r.should_wake_vlm(&ev("code"), RICH_WEAK, 0, true, true, false, false, true),
            WakeDecision::Wake(_)
        ));
        // 10 s later, same app -> debounced.
        assert_eq!(
            r.should_wake_vlm(&ev("code"), RICH_WEAK, 10_000, true, true, false, false, true),
            WakeDecision::Skip(SkipReason::Debounced)
        );
        // 31 s later -> allowed again (a different app was never debounced).
        assert!(matches!(
            r.should_wake_vlm(&ev("code"), RICH_WEAK, 31_000, true, true, false, false, true),
            WakeDecision::Wake(_)
        ));
    }

    #[test]
    fn hard_ceiling_protects_voice() {
        let mut r = TierRouter::new();
        // Fire the ceiling's worth of wakes across distinct apps (dodge debounce),
        // one per minute so they're all inside the trailing hour.
        for i in 0..WAKES_PER_HOUR_CEILING {
            let now = i as i64 * 60_000;
            let d = r.should_wake_vlm(
                &ev(&format!("app{i}")),
                RICH_WEAK,
                now,
                true,
                true,
                false,
                false,
                true,
            );
            assert!(matches!(d, WakeDecision::Wake(_)), "wake {i} should pass");
        }
        // The next wake, still within the hour, hits the ceiling — voice wins.
        let d = r.should_wake_vlm(
            &ev("appX"),
            RICH_WEAK,
            (WAKES_PER_HOUR_CEILING as i64) * 60_000,
            true,
            true,
            false,
            false,
            true,
        );
        assert_eq!(d, WakeDecision::Skip(SkipReason::RateCeiling));
        // An hour after the first wake, the window has slid — wakes resume.
        let later = RATE_WINDOW_MS + 1;
        let d = r.should_wake_vlm(&ev("appY"), RICH_WEAK, later, true, true, false, false, true);
        assert!(matches!(d, WakeDecision::Wake(_)), "window slid, wakes resume");
    }

    #[test]
    fn pattern_and_user_triggers_fire_without_weak_ocr() {
        let mut r = TierRouter::new();
        let strong = OcrSignal { confidence: 0.95, text_density: 50.0 };
        assert_eq!(
            r.should_wake_vlm(&ev("a"), strong, 0, true, true, true, false, true),
            WakeDecision::Wake(WakeReason::PatternDisambiguation)
        );
        assert_eq!(
            r.should_wake_vlm(&ev("b"), strong, 0, true, true, false, true, true),
            WakeDecision::Wake(WakeReason::UserExplicit)
        );
        // No trigger at all -> NoTrigger.
        assert_eq!(
            r.should_wake_vlm(&ev("c"), strong, 0, true, true, false, false, true),
            WakeDecision::Skip(SkipReason::NoTrigger)
        );
    }

    #[test]
    fn explicit_reasoning_always_routes_to_the_gateway() {
        assert_eq!(TierRouter::new().route_explicit_reasoning(), ReasoningRoute::Gateway);
    }
}
