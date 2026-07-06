//! M5 gate — the VLM wake-rate band (doc 06 §4, doc 12 §2, doc 16 M5, ADR-032).
//!
//! The promise: the proactive VLM wake gate holds within an **adaptive ~3–10/h
//! band**, and its **hard ceiling is non-negotiable** — a "valuable" VLM must
//! never starve voice by monopolizing the GPU mutex (ADR-032). The floor (~3/h)
//! is a value-driven target the tuner raises only when VLM-enriched suggestions
//! out-click un-enriched ones; the **ceiling (~10/h) is enforced in code** by the
//! [`TierRouter`]'s trailing-hour ledger, and that is what this gate guards.
//!
//! Runs everywhere (pure logic, no GPU/network). A regression that lets the wake
//! rate cross the hard ceiling — i.e. lets enrichment starve voice — fails M5.

use aperture_contracts::event::{Event, EventType};
use aperture_orchestration::tier_router::{
    OcrSignal, SkipReason, TierRouter, WakeDecision, WAKES_PER_HOUR_CEILING, WAKES_PER_HOUR_FLOOR,
};

/// A rich-frame-but-weak-OCR signal — trigger (b) (doc 06 §4): confidence below
/// 0.55 and density above the LOW floor.
const RICH_WEAK: OcrSignal = OcrSignal { confidence: 0.4, text_density: 20.0 };

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

#[test]
fn the_hard_ceiling_caps_wakes_and_protects_voice() {
    let mut r = TierRouter::new();
    // Fire far more candidate wakes than the ceiling — one per minute across
    // distinct apps (dodging the 30 s per-app debounce), all inside one trailing
    // hour. Every gate input passes; only the ceiling should hold them back.
    let attempts = (WAKES_PER_HOUR_CEILING as i64) * 6; // 6x the ceiling
    let mut wakes = 0u32;
    for i in 0..attempts {
        let now = i * 60_000; // one minute apart
        let d = r.should_wake_vlm(
            &ev(&format!("app{i}")),
            RICH_WEAK,
            now,
            true,  // capture_on
            true,  // mutex_likely_free
            false, // pattern_requested
            false, // user_explicit
            true,  // budget_projection_ok
        );
        if matches!(d, WakeDecision::Wake(_)) {
            wakes += 1;
        }
        // The invariant, checked at *every* step: the trailing-hour count never
        // crosses the hard ceiling — no matter how much the frame stream pushes.
        assert!(
            r.wakes_last_hour(now) <= WAKES_PER_HOUR_CEILING,
            "M5 WAKE-BAND VIOLATION: {} wakes in the trailing hour at t={now}ms > ceiling {}",
            r.wakes_last_hour(now),
            WAKES_PER_HOUR_CEILING,
        );
    }
    // Exactly the ceiling's worth were allowed (all attempts fall in one hour).
    assert_eq!(
        wakes, WAKES_PER_HOUR_CEILING,
        "the ceiling admits exactly {WAKES_PER_HOUR_CEILING} wakes/hour, got {wakes}"
    );
}

#[test]
fn over_ceiling_wakes_skip_with_rate_ceiling_the_voice_guard() {
    let mut r = TierRouter::new();
    // Burn the ceiling across distinct apps within the hour.
    for i in 0..WAKES_PER_HOUR_CEILING {
        let now = i as i64 * 60_000;
        assert!(matches!(
            r.should_wake_vlm(&ev(&format!("a{i}")), RICH_WEAK, now, true, true, false, false, true),
            WakeDecision::Wake(_)
        ));
    }
    // The next, still inside the hour, is refused *specifically* because voice is
    // protected — not for any other reason.
    let now = WAKES_PER_HOUR_CEILING as i64 * 60_000;
    assert_eq!(
        r.should_wake_vlm(&ev("later"), RICH_WEAK, now, true, true, false, false, true),
        WakeDecision::Skip(SkipReason::RateCeiling),
        "over-ceiling wakes must Skip(RateCeiling), the voice guard (ADR-032)"
    );
}

#[test]
fn the_window_slides_so_wakes_resume_after_an_hour() {
    let mut r = TierRouter::new();
    for i in 0..WAKES_PER_HOUR_CEILING {
        let now = i as i64 * 60_000;
        let _ = r.should_wake_vlm(&ev(&format!("a{i}")), RICH_WEAK, now, true, true, false, false, true);
    }
    // Just over an hour after the first wake, that wake has aged out of the
    // trailing window — the band is a sliding hour, not a hard lifetime cap.
    let later = 60 * 60 * 1000 + 1;
    assert!(
        matches!(
            r.should_wake_vlm(&ev("fresh"), RICH_WEAK, later, true, true, false, false, true),
            WakeDecision::Wake(_)
        ),
        "the trailing-hour window must slide so wakes resume (ADR-032 adaptive band)"
    );
}

#[test]
fn the_band_constants_are_the_adr032_values() {
    // The floor is the adaptive/value-driven target (not code-enforced); the
    // ceiling is the enforced hard cap. Both are documented here so a drift in
    // either is a visible M5 gate failure.
    assert_eq!(WAKES_PER_HOUR_FLOOR, 3, "ADR-032: ~3/h cold-start floor");
    assert_eq!(WAKES_PER_HOUR_CEILING, 10, "ADR-032: ~10/h hard ceiling");
    assert!(WAKES_PER_HOUR_FLOOR < WAKES_PER_HOUR_CEILING, "the band must be non-empty");
}
