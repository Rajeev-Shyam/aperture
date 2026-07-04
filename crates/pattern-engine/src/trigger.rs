//! Proactive trigger gate — the 7 rules, all of which must hold (doc 08 §6, R2).
//!
//! 1. `score ≥ τ_conf = 0.7` ([`config::TAU_CONF`], ADR-033) `[VERIFY — SC7 at M3]`
//! 2. Weighted support ≥ 3 ([`config::COLD_START_SUPPORT_FLOOR`]) `[ASSUMPTION]`
//! 3. A **fresh, resumable** `connector_state` exists for the consequent (doc 10 TTLs)
//! 4. Cooldown: same signature not shown within its current cooldown — base 30 min
//!    ([`config::COOLDOWN_MIN`]), multiplied by the dismissal ladder (×2 / ×4, ADR-033)
//! 5. Global cap: **adaptive 2→8/hr, click-through-driven** (ADR-032; cold-start
//!    default [`config::CAP_PER_HOUR_DEFAULT`]); overflow drops lowest score
//! 6. Novelty: the consequent's resource is not foreground **and** was not focused
//!    in the last ~10 min ([`config::NOVELTY_RECENT_FOCUS_MIN`], ADR-033)
//! 7. **Capture is ON**
//!
//! Rule 7 is the capture-toggle invariant (invariant 3): when capture is OFF the
//! engine emits nothing — and the orchestrator has released capture + killed the
//! sidecars (VRAM → ~0 in < 3 s). This is also a transparency boundary: the
//! pattern engine **never** makes a cloud call (doc 08 §1, locked answer A); only
//! the reasoning-gateway crate may open sockets / spawn the Claude CLI (invariant 2).

use aperture_contracts::connector::ConnectorState;

use crate::config;
use crate::scorer;

/// Why a candidate was suppressed (doc 08 §6); useful for SC7 telemetry and
/// settings-tuning diagnostics (doc 08 §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerReject {
    /// Rule 1: `score < τ_conf`.
    BelowScore,
    /// Rule 2: weighted support below the cold-start floor.
    BelowSupport,
    /// Rule 3: no fresh, resumable `connector_state` for the consequent.
    NoFreshState,
    /// Rule 4: signature shown within its (ladder-multiplied) cooldown window.
    Cooldown,
    /// Rule 5: hourly cap reached (and this score did not displace a queued one).
    HourlyCapReached,
    /// Rule 6: the consequent resource is foreground / focused in the last ~10 min.
    NotNovel,
    /// Rule 7: capture is OFF.
    CaptureOff,
}

/// Everything the gate needs about one would-be candidate (doc 08 §6).
pub struct TriggerInput<'a> {
    /// `score` from [`crate::scorer::score`] (rule 1) — novelty already folded in.
    pub score: f64,
    /// `W(antecedent ⇒ consequent)` (rule 2).
    pub weighted_support: f64,
    /// Fresh, resumable state for the consequent, if any (rule 3).
    pub connector_state: Option<&'a ConnectorState>,
    /// Stable n-gram signature, for cooldown bookkeeping (rule 4).
    pub signature: &'a str,
    /// The signature's current dismissal-ladder step (0 = none, 1 = one recent
    /// dismissal, 2 = two) — multiplies the cooldown ×1/×2/×4 (ADR-033).
    pub dismissal_step: u32,
    /// Whether the consequent resource is foreground right now (rule 6).
    pub consequent_is_foreground: bool,
    /// When the consequent's resource was last focused (rule 6, ADR-033).
    pub consequent_last_focused_ms: Option<i64>,
    /// Now (epoch ms), for cooldown / cap windows.
    pub now_ms: i64,
}

/// Per-engine trigger bookkeeping: cooldown timestamps + the rolling-hour cap
/// (doc 08 §6.4-§6.5, ADR-032). Held inside [`crate::PatternEngine`].
#[derive(Debug)]
pub struct TriggerGate {
    /// Last-shown `ts` (epoch ms) per signature, for the cooldown.
    last_shown: std::collections::HashMap<String, i64>,
    /// `ts` of suggestions emitted in the trailing hour, for the adaptive
    /// 2→8/hr cap (ADR-032).
    recent_emissions: Vec<i64>,
    /// The current adaptive cap, bounded to
    /// `[CAP_PER_HOUR_FLOOR, CAP_PER_HOUR_CEILING]` (ADR-032); starts at the
    /// cold-start default and moves on click-through evidence.
    cap_per_hour: u32,
}

impl Default for TriggerGate {
    fn default() -> Self {
        Self {
            last_shown: Default::default(),
            recent_emissions: Vec::new(),
            cap_per_hour: config::CAP_PER_HOUR_DEFAULT,
        }
    }
}

impl TriggerGate {
    /// Fresh gate at the cold-start cap.
    pub fn new() -> Self {
        Self::default()
    }

    /// The current adaptive hourly cap (ADR-032).
    pub fn cap_per_hour(&self) -> u32 {
        self.cap_per_hour
    }

    /// Nudge the adaptive cap on click-through evidence (ADR-032): sustained
    /// clicks earn presence (+1), sustained ignores lose it (−1); always clamped
    /// to the hard band. Call from the feedback loop at M3-tuning cadence.
    pub fn adapt_cap(&mut self, clicked: bool) {
        let next = if clicked {
            self.cap_per_hour.saturating_add(1)
        } else {
            self.cap_per_hour.saturating_sub(1)
        };
        self.cap_per_hour = next.clamp(config::CAP_PER_HOUR_FLOOR, config::CAP_PER_HOUR_CEILING);
    }

    /// Apply rules 1-7 (rule 7 supplied by `capture_on` from the orchestrator).
    ///
    /// `Ok(())` ⇒ emit; `Err(reason)` ⇒ suppressed. This is **read-only** — it
    /// does not record the emission; call [`note_emitted`](Self::note_emitted)
    /// once the candidate is actually shown so cooldown/cap stay accurate.
    pub fn admit(&self, input: &TriggerInput<'_>, capture_on: bool) -> Result<(), TriggerReject> {
        // Rule 7 first — the invariant, and the cheapest check (doc 08 §6.7).
        if !capture_on {
            return Err(TriggerReject::CaptureOff);
        }
        // Rule 1: score threshold (novelty already folded into score upstream,
        // but rule 6 is also asserted independently below for defense in depth).
        if input.score < config::TAU_CONF {
            return Err(TriggerReject::BelowScore);
        }
        // Rule 2: cold-start support floor. The small epsilon absorbs read-time
        // decay: "3 observed returns" minutes ago weigh 2.999…, which must count
        // as 3 (US1 acceptance (a)); 2 returns (≈2.0) never pass.
        if input.weighted_support + 0.01 < config::COLD_START_SUPPORT_FLOOR {
            return Err(TriggerReject::BelowSupport);
        }
        // Rule 3: fresh, resumable connector state.
        match input.connector_state {
            Some(st) if scorer::freshness(st, input.now_ms) > 0.0 => {}
            _ => return Err(TriggerReject::NoFreshState),
        }
        // Rule 4: per-signature cooldown, ladder-multiplied (ADR-033).
        let ladder_mult = match input.dismissal_step {
            0 => 1,
            1 => config::DISMISS_COOLDOWN_MULT_1ST,
            _ => config::DISMISS_COOLDOWN_MULT_2ND,
        };
        if let Some(&shown) = self.last_shown.get(input.signature) {
            if input.now_ms - shown < config::COOLDOWN_MIN * ladder_mult * 60_000 {
                return Err(TriggerReject::Cooldown);
            }
        }
        // Rule 5: adaptive rolling-hour cap (ADR-032). Overflow-displacement of
        // a lower-score queued candidate is coordinated by the caller's queue
        // (doc 08 §6.5); the gate itself just enforces the count.
        let hour_ago = input.now_ms - 3_600_000;
        let emitted_last_hour = self
            .recent_emissions
            .iter()
            .filter(|&&t| t > hour_ago)
            .count() as u32;
        if emitted_last_hour >= self.cap_per_hour {
            return Err(TriggerReject::HourlyCapReached);
        }
        // Rule 6: novelty — foreground + ~10 min recent-focus window (ADR-033).
        if input.consequent_is_foreground {
            return Err(TriggerReject::NotNovel);
        }
        if let Some(t) = input.consequent_last_focused_ms {
            if input.now_ms.saturating_sub(t) < config::NOVELTY_RECENT_FOCUS_MIN * 60_000 {
                return Err(TriggerReject::NotNovel);
            }
        }
        Ok(())
    }

    /// Record that `signature` was shown at `now_ms`, updating the cooldown map
    /// and the rolling-hour cap window (doc 08 §6.4-§6.5).
    pub fn note_emitted(&mut self, signature: &str, now_ms: i64) {
        self.last_shown.insert(signature.to_string(), now_ms);
        self.recent_emissions.push(now_ms);
        let hour_ago = now_ms - 3_600_000;
        self.recent_emissions.retain(|&t| t > hour_ago);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_state() -> ConnectorState {
        ConnectorState {
            id: "c1".into(),
            connector_type: "youtube".into(),
            reconstruct_payload: serde_json::json!({}),
            payload_version: 1,
            captured_ts: 0,
            stale_after_ts: None,
        }
    }

    fn ok_input<'a>(state: &'a ConnectorState, now: i64) -> TriggerInput<'a> {
        TriggerInput {
            score: 0.9,
            weighted_support: 5.0,
            connector_state: Some(state),
            signature: "sig",
            dismissal_step: 0,
            consequent_is_foreground: false,
            consequent_last_focused_ms: None,
            now_ms: now,
        }
    }

    #[test]
    fn all_rules_pass_then_emit() {
        let gate = TriggerGate::new();
        let st = fresh_state();
        assert!(gate.admit(&ok_input(&st, 0), true).is_ok());
    }

    #[test]
    fn capture_off_blocks_everything() {
        let gate = TriggerGate::new();
        let st = fresh_state();
        assert_eq!(
            gate.admit(&ok_input(&st, 0), false),
            Err(TriggerReject::CaptureOff),
            "invariant 3: OFF ⇒ nothing"
        );
    }

    #[test]
    fn tau_conf_is_the_r2_070() {
        let gate = TriggerGate::new();
        let st = fresh_state();
        let mut input = ok_input(&st, 0);
        input.score = 0.65; // passed R1's 0.6, must FAIL R2's 0.7 (ADR-033)
        assert_eq!(gate.admit(&input, true), Err(TriggerReject::BelowScore));
    }

    #[test]
    fn cooldown_is_ladder_multiplied() {
        let mut gate = TriggerGate::new();
        let st = fresh_state();
        gate.note_emitted("sig", 0);

        // 45 min later: past the base 30 min cooldown…
        let mut input = ok_input(&st, 45 * 60_000);
        assert!(gate.admit(&input, true).is_ok(), "base cooldown expired");
        // …but NOT past a ×2 (one-dismissal) 60 min cooldown (ADR-033).
        input.dismissal_step = 1;
        assert_eq!(gate.admit(&input, true), Err(TriggerReject::Cooldown));
    }

    #[test]
    fn hourly_cap_enforced_and_adaptive_band_clamped() {
        let mut gate = TriggerGate::new();
        let st = fresh_state();
        assert_eq!(gate.cap_per_hour(), config::CAP_PER_HOUR_DEFAULT);

        for i in 0..gate.cap_per_hour() {
            gate.note_emitted(&format!("s{i}"), 0);
        }
        assert_eq!(
            gate.admit(&ok_input(&st, 60_000), true),
            Err(TriggerReject::HourlyCapReached)
        );

        // The band is hard-clamped (ADR-032).
        for _ in 0..20 {
            gate.adapt_cap(true);
        }
        assert_eq!(gate.cap_per_hour(), config::CAP_PER_HOUR_CEILING);
        for _ in 0..20 {
            gate.adapt_cap(false);
        }
        assert_eq!(gate.cap_per_hour(), config::CAP_PER_HOUR_FLOOR);
    }

    #[test]
    fn stale_connector_and_recent_focus_are_rejected() {
        let gate = TriggerGate::new();
        let mut st = fresh_state();
        st.stale_after_ts = Some(10);
        let input = ok_input(&st, 100);
        assert_eq!(gate.admit(&input, true), Err(TriggerReject::NoFreshState));

        let st2 = fresh_state();
        let mut input2 = ok_input(&st2, 100 * 60_000);
        input2.consequent_last_focused_ms = Some(95 * 60_000); // 5 min ago
        assert_eq!(
            gate.admit(&input2, true),
            Err(TriggerReject::NotNovel),
            "focused 5 min ago → not novel (ADR-033)"
        );
    }
}
