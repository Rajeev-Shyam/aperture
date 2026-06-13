//! Proactive trigger gate — the 7 rules, all of which must hold (doc 08 §6).
//!
//! 1. `score ≥ τ_conf = 0.6` ([`config::TAU_CONF`]) `[VERIFY — SC7 at M3]`
//! 2. Weighted support ≥ 3 ([`config::COLD_START_SUPPORT_FLOOR`]) `[ASSUMPTION]`
//! 3. A **fresh, resumable** `connector_state` exists for the consequent (doc 10 TTLs)
//! 4. Cooldown: same signature not shown in the last 30 min ([`config::COOLDOWN_MIN`])
//! 5. Global cap: ≤ 4 suggestions/hour ([`config::CAP_PER_HOUR`]); overflow drops lowest score
//! 6. Novelty: the consequent's resource is not currently foreground
//! 7. **Capture is ON**
//!
//! Rule 7 is the capture-toggle invariant (invariant 3): when capture is OFF the
//! engine emits nothing — and the orchestrator has released capture + killed the
//! sidecars (VRAM → ~0 in < 3 s). This is also a transparency boundary: the
//! pattern engine **never** makes a cloud call (doc 08 §1, locked answer A); only
//! the reasoning-gateway crate may open sockets / spawn the Claude CLI (invariant 2).

use aperture_contracts::connector::ConnectorState;

use crate::config;

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
    /// Rule 4: signature shown within the cooldown window.
    Cooldown,
    /// Rule 5: hourly cap reached (and this score did not displace a queued one).
    HourlyCapReached,
    /// Rule 6: the consequent resource is already foreground.
    NotNovel,
    /// Rule 7: capture is OFF.
    CaptureOff,
}

/// Everything the gate needs about one would-be candidate (doc 08 §6).
pub struct TriggerInput<'a> {
    /// `score` from [`crate::scorer::score`] (rule 1).
    pub score: f64,
    /// `W(antecedent ⇒ consequent)` (rule 2).
    pub weighted_support: f64,
    /// Fresh, resumable state for the consequent, if any (rule 3).
    pub connector_state: Option<&'a ConnectorState>,
    /// Stable n-gram signature, for cooldown bookkeeping (rule 4).
    pub signature: &'a str,
    /// Whether the consequent resource is foreground right now (rule 6).
    pub consequent_is_foreground: bool,
    /// Now (epoch ms), for cooldown / cap windows.
    pub now_ms: i64,
}

/// Per-engine trigger bookkeeping: cooldown timestamps + the rolling-hour cap
/// (doc 08 §6.4-§6.5). Held inside [`crate::PatternEngine`].
#[derive(Debug, Default)]
pub struct TriggerGate {
    /// Last-shown `ts` (epoch ms) per signature, for the 30-min cooldown.
    last_shown: std::collections::HashMap<String, i64>,
    /// `ts` of suggestions emitted in the trailing hour, for the ≤4/hr cap.
    recent_emissions: Vec<i64>,
}

impl TriggerGate {
    /// Fresh gate.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply rules 1-7 (rule 7 supplied by `capture_on` from the orchestrator).
    ///
    /// `Ok(())` ⇒ emit; `Err(reason)` ⇒ suppressed. This is **read-only** — it
    /// does not record the emission; call [`note_emitted`](Self::note_emitted)
    /// once the candidate is actually shown so cooldown/cap stay accurate.
    pub fn admit(&self, _input: &TriggerInput<'_>, _capture_on: bool) -> Result<(), TriggerReject> {
        // TODO(M3): evaluate in order, cheapest first; return the first failing
        // rule. Notes on individual rules:
        //   1. input.score >= config::TAU_CONF
        //   2. input.weighted_support >= config::COLD_START_SUPPORT_FLOOR
        //   3. input.connector_state is Some AND fresh (scorer::freshness > 0)
        //   4. now - last_shown[signature] >= COOLDOWN_MIN * 60_000
        //   5. emissions in trailing hour < CAP_PER_HOUR (overflow drops lowest
        //      score — coordinated by the caller's candidate queue, doc 08 §6.5)
        //   6. !input.consequent_is_foreground
        //   7. capture_on  (invariant 3 — capture toggle)
        let _ = (config::TAU_CONF, config::COLD_START_SUPPORT_FLOOR, config::COOLDOWN_MIN);
        todo!("M3: evaluate the 7 trigger rules in order (doc 08 §6)")
    }

    /// Record that `signature` was shown at `now_ms`, updating the cooldown map
    /// and the rolling-hour cap window (doc 08 §6.4-§6.5).
    pub fn note_emitted(&mut self, _signature: &str, _now_ms: i64) {
        // TODO(M3): last_shown.insert(signature, now_ms); push now_ms into
        // recent_emissions; prune entries older than 1 h.
        todo!("M3: record emission for cooldown + hourly cap (doc 08 §6.4-§6.5)")
    }
}
