//! Proactive trigger gate — the 7 rules, all of which must hold (doc 08 §6, R2).
//!
//! 1. `score ≥ τ_conf = 0.4` ([`config::TAU_CONF`], owner-lowered 2026-08-26 from ADR-033's 0.7) `[VERIFY — SC7 at M3]`
//! 2. Weighted support ≥ 3 ([`config::COLD_START_SUPPORT_FLOOR`]) `[ASSUMPTION]`
//! 3. A **fresh, resumable** `connector_state` exists for the consequent (doc 10
//!    TTLs). Amended by owner decision #15 (2026-08-16): pure app-focus
//!    consequents ("switch to X") are exempt — they carry no resumable state by
//!    design ([`TriggerInput::requires_fresh_state`]); every other rule still
//!    applies to them.
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
/// settings-tuning diagnostics (doc 08 §9) — counted per reason in
/// [`GateStats`] and shown in the Dashboard's diagnostics block (doc 11 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
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

impl TriggerReject {
    /// Every reason, in rule order — the index into [`GateStats::rejected`].
    pub const ALL: [TriggerReject; 7] = [
        TriggerReject::BelowScore,
        TriggerReject::BelowSupport,
        TriggerReject::NoFreshState,
        TriggerReject::Cooldown,
        TriggerReject::HourlyCapReached,
        TriggerReject::NotNovel,
        TriggerReject::CaptureOff,
    ];

    /// Position in [`Self::ALL`].
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|r| *r == self).expect("every reason is listed")
    }

    /// The rule in the words the Dashboard shows (doc 11 §6 diagnostics).
    pub fn label(self) -> &'static str {
        match self {
            TriggerReject::BelowScore => "not confident enough",
            TriggerReject::BelowSupport => "not repeated enough yet",
            TriggerReject::NoFreshState => "nothing fresh to resume",
            TriggerReject::Cooldown => "shown too recently",
            TriggerReject::HourlyCapReached => "hourly budget spent",
            TriggerReject::NotNovel => "you were just there",
            TriggerReject::CaptureOff => "capture was off",
        }
    }

    /// One sentence explaining what would make the rule pass.
    pub fn hint(self) -> &'static str {
        match self {
            TriggerReject::BelowScore => {
                "The habit's confidence × freshness × novelty fell under the certainty knob."
            }
            TriggerReject::BelowSupport => {
                "The sequence has been seen fewer times than the repeats knob."
            }
            TriggerReject::NoFreshState => {
                "No captured page, document or file to resume — the browser extension and Office titles feed this."
            }
            TriggerReject::Cooldown => "The same habit surfaced inside its quiet window.",
            TriggerReject::HourlyCapReached => "The rolling-hour cap was already reached.",
            TriggerReject::NotNovel => {
                "The thing it would suggest is on screen or was focused in the last ten minutes."
            }
            TriggerReject::CaptureOff => "Nothing fires while capture is off.",
        }
    }
}

/// One gate decision, kept as the "last admitted" / "closest miss" exhibits
/// for the diagnostics block. Signatures are coarse class tokens (`browser:focus:∅
/// ⇒ ide:focus:∅`) — never titles or URLs beyond a host — so this is safe to show.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GateDecision {
    pub signature: String,
    pub score: f64,
    /// `None` = admitted.
    pub reject: Option<TriggerReject>,
    pub at_ms: i64,
}

/// Counters over every gate decision since startup (doc 08 §9 diagnostics;
/// doc 11 §6's Advanced-tab diagnostics, built 2026-09-06 after three weeks of
/// "zero recommendations" with no way to see why). Pure bookkeeping — the gate
/// itself never reads it.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct GateStats {
    /// Candidates the gate looked at.
    pub evaluated: u64,
    /// Candidates that passed all seven rules.
    pub admitted: u64,
    /// Rejections by rule, indexed by [`TriggerReject::index`].
    pub rejected: [u64; 7],
    pub last_admitted: Option<GateDecision>,
    pub last_rejected: Option<GateDecision>,
    /// The highest-scoring rejection seen — "the closest miss".
    pub closest_miss: Option<GateDecision>,
}

impl GateStats {
    /// Record one decision.
    pub fn record(&mut self, signature: &str, score: f64, at_ms: i64, outcome: Result<(), TriggerReject>) {
        self.evaluated += 1;
        let decision = GateDecision {
            signature: signature.to_string(),
            score,
            reject: outcome.err(),
            at_ms,
        };
        match outcome {
            Ok(()) => {
                self.admitted += 1;
                self.last_admitted = Some(decision);
            }
            Err(reason) => {
                self.rejected[reason.index()] += 1;
                if self
                    .closest_miss
                    .as_ref()
                    .map_or(true, |best| score > best.score)
                {
                    self.closest_miss = Some(decision.clone());
                }
                self.last_rejected = Some(decision);
            }
        }
    }

    /// `(label, hint, count)` per rule, in rule order — the Dashboard's table.
    pub fn rejected_by_reason(&self) -> Vec<(&'static str, &'static str, u64)> {
        TriggerReject::ALL
            .iter()
            .map(|r| (r.label(), r.hint(), self.rejected[r.index()]))
            .collect()
    }
}

/// Everything the gate needs about one would-be candidate (doc 08 §6).
pub struct TriggerInput<'a> {
    /// `score` from [`crate::scorer::score`] (rule 1) — novelty already folded in.
    pub score: f64,
    /// `W(antecedent ⇒ consequent)` (rule 2).
    pub weighted_support: f64,
    /// Fresh, resumable state for the consequent, if any (rule 3).
    pub connector_state: Option<&'a ConnectorState>,
    /// Whether rule 3 applies to this candidate. `true` for every resume-style
    /// candidate; `false` only for the "switch to X" app-focus candidates
    /// (owner decision #15, 2026-08-16), whose consequent has no resumable
    /// state by design. Rules 1-2 and 4-7 apply regardless.
    pub requires_fresh_state: bool,
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
    /// The current adaptive cap, bounded to `[cap_floor, cap_ceiling]`
    /// (ADR-032); starts at the cold-start default and moves on click-through
    /// evidence.
    cap_per_hour: u32,
    /// Whether [`adapt_cap`](Self::adapt_cap) has ever moved the cap — a
    /// settings reload ([`configure`](Self::configure)) re-baselines an
    /// unadapted cap to the new default but never discards earned adaptation.
    cap_adapted: bool,
    /// Rule 1 threshold (decision #17; default [`config::TAU_CONF`]).
    tau_conf: f64,
    /// Rule 2 floor (default [`config::COLD_START_SUPPORT_FLOOR`]).
    support_floor: f64,
    /// Rule 4 base cooldown, minutes (default [`config::COOLDOWN_MIN`]).
    cooldown_min: i64,
    /// Rule 5 hard band (defaults [`config::CAP_PER_HOUR_FLOOR`] /
    /// [`config::CAP_PER_HOUR_CEILING`]).
    cap_floor: u32,
    cap_ceiling: u32,
}

impl Default for TriggerGate {
    fn default() -> Self {
        Self {
            last_shown: Default::default(),
            recent_emissions: Vec::new(),
            cap_per_hour: config::CAP_PER_HOUR_DEFAULT,
            cap_adapted: false,
            tau_conf: config::TAU_CONF,
            support_floor: config::COLD_START_SUPPORT_FLOOR,
            cooldown_min: config::COOLDOWN_MIN,
            cap_floor: config::CAP_PER_HOUR_FLOOR,
            cap_ceiling: config::CAP_PER_HOUR_CEILING,
        }
    }
}

impl TriggerGate {
    /// Fresh gate at the cold-start cap.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply the runtime tunables (decision #17): threshold, support floor,
    /// cooldown, and the cap band. An unadapted cap re-baselines to the new
    /// default; an adapted cap keeps its earned value, re-clamped into the new
    /// band — a daily settings re-read must not trash learned presence.
    pub fn configure(&mut self, cfg: &config::EngineConfig) {
        self.tau_conf = cfg.tau_conf;
        self.support_floor = cfg.cold_start_support_floor;
        self.cooldown_min = cfg.cooldown_min;
        self.cap_floor = cfg.cap_per_hour_floor;
        self.cap_ceiling = cfg.cap_per_hour_ceiling;
        if !self.cap_adapted {
            self.cap_per_hour = cfg.cap_per_hour_default;
        }
        self.cap_per_hour = self.cap_per_hour.clamp(self.cap_floor, self.cap_ceiling);
    }

    /// The current adaptive hourly cap (ADR-032).
    pub fn cap_per_hour(&self) -> u32 {
        self.cap_per_hour
    }

    /// The configured rule-1 threshold (diagnostics).
    pub fn tau_conf(&self) -> f64 {
        self.tau_conf
    }

    /// The configured rule-2 floor (diagnostics).
    pub fn support_floor(&self) -> f64 {
        self.support_floor
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
        self.cap_per_hour = next.clamp(self.cap_floor, self.cap_ceiling);
        self.cap_adapted = true;
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
        if input.score < self.tau_conf {
            return Err(TriggerReject::BelowScore);
        }
        // Rule 2: cold-start support floor. The small epsilon absorbs read-time
        // decay: "3 observed returns" minutes ago weigh 2.999…, which must count
        // as 3 (US1 acceptance (a)); 2 returns (≈2.0) never pass.
        if input.weighted_support + 0.01 < self.support_floor {
            return Err(TriggerReject::BelowSupport);
        }
        // Rule 3: fresh, resumable connector state — except for app-focus
        // ("switch to X") candidates, which have none by design (decision #15).
        if input.requires_fresh_state {
            match input.connector_state {
                Some(st) if scorer::freshness(st, input.now_ms) > 0.0 => {}
                _ => return Err(TriggerReject::NoFreshState),
            }
        }
        // Rule 4: per-signature cooldown, ladder-multiplied (ADR-033).
        let ladder_mult = match input.dismissal_step {
            0 => 1,
            1 => config::DISMISS_COOLDOWN_MULT_1ST,
            _ => config::DISMISS_COOLDOWN_MULT_2ND,
        };
        if let Some(&shown) = self.last_shown.get(input.signature) {
            if input.now_ms - shown < self.cooldown_min * ladder_mult * 60_000 {
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
            requires_fresh_state: true,
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
    fn tau_conf_is_the_owner_lowered_040() {
        let gate = TriggerGate::new();
        let st = fresh_state();
        let mut input = ok_input(&st, 0);
        input.score = 0.35; // must FAIL the 0.4 floor (owner-lowered 2026-08-26)
        assert_eq!(gate.admit(&input, true), Err(TriggerReject::BelowScore));
        input.score = 0.65; // failed the old 0.7; must PASS the 0.4 floor
        assert!(gate.admit(&input, true).is_ok());
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
    fn app_focus_candidates_skip_rule_3_but_nothing_else() {
        // Decision #15: no connector state + requires_fresh_state=false admits…
        let gate = TriggerGate::new();
        let st = fresh_state();
        let mut input = ok_input(&st, 0);
        input.connector_state = None;
        input.requires_fresh_state = false;
        assert!(gate.admit(&input, true).is_ok(), "rule 3 exempt for app-focus (#15)");

        // …but every other rule still applies: score, support, novelty, capture.
        input.score = 0.3;
        assert_eq!(gate.admit(&input, true), Err(TriggerReject::BelowScore));
        input.score = 0.9;
        input.weighted_support = 2.0;
        assert_eq!(gate.admit(&input, true), Err(TriggerReject::BelowSupport));
        input.weighted_support = 5.0;
        input.consequent_is_foreground = true;
        assert_eq!(gate.admit(&input, true), Err(TriggerReject::NotNovel));
        input.consequent_is_foreground = false;
        assert_eq!(gate.admit(&input, false), Err(TriggerReject::CaptureOff));
    }

    #[test]
    fn configure_applies_runtime_tunables_and_keeps_earned_cap() {
        // Decision #17: the settings block moves the gate's thresholds.
        let mut gate = TriggerGate::new();
        let st = fresh_state();
        let mut cfg = config::EngineConfig {
            tau_conf: 0.95,
            ..config::EngineConfig::default()
        };
        gate.configure(&cfg);
        let input = ok_input(&st, 0); // score 0.9 passes the default 0.4…
        assert_eq!(
            gate.admit(&input, true),
            Err(TriggerReject::BelowScore),
            "…but not a configured 0.95 (#17)"
        );

        // An unadapted cap re-baselines to the configured default…
        cfg.tau_conf = 0.7;
        cfg.cap_per_hour_default = 6;
        gate.configure(&cfg);
        assert_eq!(gate.cap_per_hour(), 6);
        // …while an adapted cap survives a reload (re-clamped only).
        gate.adapt_cap(false); // 6 → 5, adapted
        gate.configure(&cfg);
        assert_eq!(gate.cap_per_hour(), 5, "earned adaptation is not reset by a re-read");
    }

    /// 2026-09-06 diagnostics: every decision is counted once, per rule, and
    /// the closest miss is the highest-scoring rejection.
    #[test]
    fn gate_stats_count_each_decision_and_keep_the_closest_miss() {
        let mut stats = GateStats::default();
        stats.record("a ⇒ b", 0.9, 1, Ok(()));
        stats.record("a ⇒ c", 0.3, 2, Err(TriggerReject::BelowScore));
        stats.record("a ⇒ d", 0.8, 3, Err(TriggerReject::NotNovel));
        stats.record("a ⇒ e", 0.5, 4, Err(TriggerReject::NotNovel));
        assert_eq!((stats.evaluated, stats.admitted), (4, 1));
        assert_eq!(stats.rejected[TriggerReject::BelowScore.index()], 1);
        assert_eq!(stats.rejected[TriggerReject::NotNovel.index()], 2);
        assert_eq!(stats.rejected.iter().sum::<u64>(), 3);
        assert_eq!(stats.last_admitted.as_ref().map(|d| d.signature.as_str()), Some("a ⇒ b"));
        assert_eq!(stats.last_rejected.as_ref().map(|d| d.at_ms), Some(4));
        let miss = stats.closest_miss.as_ref().expect("a miss was recorded");
        assert_eq!((miss.signature.as_str(), miss.reject), ("a ⇒ d", Some(TriggerReject::NotNovel)));
        let by_reason = stats.rejected_by_reason();
        assert_eq!(by_reason.len(), TriggerReject::ALL.len());
        assert_eq!(by_reason[TriggerReject::NotNovel.index()].2, 2);
        assert!(by_reason.iter().all(|(label, hint, _)| !label.is_empty() && !hint.is_empty()));
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
