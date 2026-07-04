//! VLM wake-up gating — the heuristics that protect the GPU (doc 06 §4, M5).
//!
//! The gate decides whether a frame is worth a `prio:50` VLM job. The whole
//! point is restraint: the wake budget is **adaptive ~3–10/hour, value-driven**
//! (ADR-032: raised when VLM-enriched suggestions out-click un-enriched ones;
//! the **hard ceiling is non-negotiable** so a "valuable" VLM never starves
//! voice; tuned at the M5 gate). Three things gate every wake:
//! 1. capture must be on **and** the mutex likely free (doc 12);
//! 2. no per-app debounce active (30 s, anti-thrash);
//! 3. a real trigger fired: (a) the pattern engine asked to disambiguate (doc
//!    08), (b) a rich frame with weak OCR, or (c) an explicit user request;
//! and finally the budget projection must pass (doc 04 R1, via orchestration).
//!
//! Reference (doc 06 §4):
//! ```text
//! fn should_wake_vlm(ev, ocr) -> bool {
//!   if !capture_on() || !mutex_likely_free() { return false }
//!   if debounce_active(30s per app) { return false }
//!   let trigger =
//!        pattern_engine.requested_disambiguation(ev)
//!     || (ocr.confidence < 0.55 && ocr.text_density > LOW)
//!     || user_explicit_request();
//!   trigger && budget_projection_ok()
//! }
//! ```

use aperture_contracts::event::Event;

use crate::ocr_engine::OcrOutput;

/// Per-app debounce window — at most one wake per app per this interval
/// (doc 06 §4 anti-thrash). Seconds.
pub const PER_APP_DEBOUNCE_SECS: u64 = 30;

/// OCR-confidence ceiling for the "rich frame, weak OCR" trigger (doc 06 §4,
/// branch (b)). Below this *and* dense enough ⇒ the VLM may help.
pub const WEAK_OCR_CONFIDENCE: f32 = 0.55;

/// `LOW` text-density floor (doc 06 §4, branch (b)) [ASSUMPTION; tuned at M5].
/// Density is [`OcrOutput::text_density`]; a frame must exceed this to count as
/// "rich" (an empty frame is not worth a VLM wake).
pub const LOW_TEXT_DENSITY: usize = 8;

/// Adaptive proactive wake budget — floor (ADR-032: cold-start conservative).
pub const WAKES_PER_HOUR_FLOOR: u32 = 3;
/// Adaptive proactive wake budget — **hard ceiling** (ADR-032: non-negotiable so
/// the VLM never starves voice). Not enforced here — the orchestration telemetry
/// asserts it at the M5 gate — but documented so the thresholds have a north star.
pub const WAKES_PER_HOUR_CEILING: u32 = 10;

/// Why a wake fired — logged for tuning (doc 06 §4: "Wake reasons are logged").
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeReason {
    /// (a) The pattern engine asked to disambiguate this event (doc 08).
    PatternDisambiguation,
    /// (b) Rich frame but the cheap OCR was weak (doc 06 §4).
    WeakOcrRichFrame,
    /// (c) The user explicitly asked (e.g. enrichment "add scene summary").
    UserRequest,
}

/// The runtime signals the gate needs but cannot observe from inside this crate
/// — capture/mutex/budget state is owned by orchestration (doc 12), and the
/// pattern/user triggers come from docs 08/11. The orchestrator fills this in
/// and calls [`should_wake_vlm`]; keeping it a plain struct preserves the
/// transparency gate (no scheduler/network handle leaks in here).
#[derive(Debug, Clone, Copy)]
pub struct GateInputs {
    /// Capture toggle is ON (doc 05 §5 / doc 12).
    pub capture_on: bool,
    /// The GPU mutex is *likely* free right now (doc 12; advisory, the real
    /// admission check is the scheduler's projection).
    pub mutex_likely_free: bool,
    /// A wake for this app fired within [`PER_APP_DEBOUNCE_SECS`] (doc 06 §4).
    pub debounce_active: bool,
    /// Trigger (a): the pattern engine requested disambiguation for this event.
    pub pattern_requested_disambiguation: bool,
    /// Trigger (c): an explicit user enrichment request.
    pub user_explicit_request: bool,
    /// `budget_projection_ok()` — doc 04 R1, computed by the orchestration
    /// BudgetEnforcer (doc 12 §4). The scheduler may still refuse at enqueue.
    pub budget_projection_ok: bool,
}

/// The gate (doc 06 §4). Returns `Some(reason)` when the frame should wake the
/// VLM (the reason is logged for tuning), `None` otherwise. Mirrors the doc's
/// pseudocode exactly, including short-circuit order.
///
/// Note this returns *intent only* — the orchestrator still goes through the
/// scheduler, whose projection check (doc 04 R1 / doc 12 §4) is the final word;
/// a `Some` here can still be refused at enqueue and degrade to OCR-only
/// (doc 06 §6).
pub fn should_wake_vlm(_ev: &Event, ocr: &OcrOutput, g: &GateInputs) -> Option<WakeReason> {
    // TODO(M5): tune thresholds against the adaptive 3–10 wakes/h band (ADR-032)
    // on real usage; requires the click-attribution proxy for raising the budget.
    if !g.capture_on || !g.mutex_likely_free {
        return None;
    }
    if g.debounce_active {
        return None; // anti-thrash, 30 s per app
    }

    let reason = if g.pattern_requested_disambiguation {
        Some(WakeReason::PatternDisambiguation) // (a) doc 08 asks
    } else if ocr.mean_confidence < WEAK_OCR_CONFIDENCE
        && ocr.text_density() > LOW_TEXT_DENSITY
    {
        Some(WakeReason::WeakOcrRichFrame) // (b) rich frame, weak OCR
    } else if g.user_explicit_request {
        Some(WakeReason::UserRequest) // (c) explicit
    } else {
        None
    };

    // trigger && budget_projection_ok() (doc 04 R1 via doc 12)
    match reason {
        Some(r) if g.budget_projection_ok => Some(r),
        _ => None,
    }
}

/// Boolean convenience matching the doc's `-> bool` signature (doc 06 §4) for
/// callers that don't need the reason.
pub fn should_wake_vlm_bool(ev: &Event, ocr: &OcrOutput, g: &GateInputs) -> bool {
    should_wake_vlm(ev, ocr, g).is_some()
}
