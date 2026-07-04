//! Tier routing: the VLM wake gate + the explicit-reasoning hand-off (doc 12 §2,
//! doc 06 §4, doc 02 §8).
//!
//! Two jobs:
//! 1. Apply doc 06 §4's **wake gate** — decide whether an event/OCR pair earns a
//!    Tier-1 VLM job. VLM output **never gates a bubble** (doc 02 Path A
//!    invariant, doc 06 §4): it enriches the *next* cycle, not this one.
//! 2. Route **explicit** reasoning (Path D, doc 02 §6) to the reasoning gateway —
//!    **never** the proactive loop. Anything -> Tier 2 passes only through the
//!    gateway after the transparency gate (invariant 2, doc 02 §8). This crate
//!    holds **no** gateway/network handle; it returns a routing *decision* the
//!    shell forwards.

use aperture_contracts::Event;

/// Adaptive wake-rate band the M5 gate asserts (ADR-032): floor ~3/hr
/// (cold-start), **hard ceiling ~10/hr** (non-negotiable — protects voice).
/// Value-driven: raised only when VLM-enriched suggestions out-click un-enriched.
pub const WAKES_PER_HOUR_FLOOR: u32 = 3;
/// See [`WAKES_PER_HOUR_FLOOR`]. `[ASSUMPTION; tune at M5]`.
pub const WAKES_PER_HOUR_CEILING: u32 = 10;
/// Per-app anti-thrash debounce on VLM wakes (doc 06 §4).
pub const WAKE_DEBOUNCE_PER_APP_SECS: u64 = 30;

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

/// The gate's verdict (doc 06 §4). `Wake` carries the reason for telemetry +
/// logging; every `Skip` reason is also logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeDecision {
    Wake(WakeReason),
    /// Capture off, mutex unlikely free, debounced, no trigger, or the R1
    /// projection would fail (doc 06 §4 / doc 04 R1). OCR-only is the contract.
    Skip,
}

/// Minimal OCR signal the wake gate reads (doc 06 §4). Full OCR output lives in
/// `aperture-vision-ocr`; the router needs only the gate inputs.
#[derive(Debug, Clone, Copy)]
pub struct OcrSignal {
    pub confidence: f32,
    /// Text-density bucket; the gate compares against the doc 06 §4 `LOW` floor.
    pub text_density: f32,
}

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

/// Applies the doc 06 §4 wake gate and the Path-D routing rule (doc 12 §2).
/// Stateless except for the per-app wake debounce timers.
pub struct TierRouter {
    // wake_debounce: HashMap<String, Instant>, // per-app, WAKE_DEBOUNCE_PER_APP_SECS
}

impl TierRouter {
    pub fn new() -> Self {
        Self {}
    }

    /// The doc 06 §4 `should_wake_vlm` gate, operationalized:
    /// ```text
    /// if !capture_on || !mutex_likely_free { Skip }
    /// if debounce_active(30 s per app)       { Skip }   // anti-thrash
    /// trigger = pattern_requested_disambiguation
    ///        || (ocr.confidence < 0.55 && ocr.text_density > LOW)
    ///        || user_explicit_request
    /// Wake iff trigger && budget_projection_ok (doc 04 R1 via doc 12)
    /// ```
    pub fn should_wake_vlm(
        &mut self,
        _ev: &Event,
        _ocr: OcrSignal,
        _capture_on: bool,
        _mutex_likely_free: bool,
        _pattern_requested: bool,
        _user_explicit: bool,
    ) -> WakeDecision {
        // TODO(M5:) implement the gate exactly per doc 06 §4; on Wake, stamp the
        //   per-app debounce and telemetry.record_vlm_wake(reason). The R1 check
        //   runs through the BudgetEnforcer (doc 04 R1). Wakes are logged for the
        //   < 6/h M5 gate (doc 06 §4, doc 16 M5).
        todo!("M5: doc 06 §4 wake gate; VLM never gates a bubble (doc 02 Path A)")
    }

    /// Route an explicit reasoning request (Path D, doc 02 §6): always the
    /// gateway, never the proactive loop (doc 12 §2). The shell takes this
    /// decision and invokes the gateway after preview + the transparency gate.
    pub fn route_explicit_reasoning(&self) -> ReasoningRoute {
        // The proactive loop has NO cloud route by construction (invariant 2,
        // doc 12 §2). Explicit requests go only through the gateway (doc 02 §8).
        ReasoningRoute::Gateway
    }
}

impl Default for TierRouter {
    fn default() -> Self {
        Self::new()
    }
}
