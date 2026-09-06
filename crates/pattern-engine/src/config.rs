//! Tunable constants for the pattern engine (doc 08 §3-§7, R2 values).
//!
//! These are the levers SC7 is tuned against (doc 16 M3); doc 08 marks several
//! as `[ASSUMPTION]` / `[VERIFY]`. They are surfaced in settings (doc 08 §9
//! "tunables exposed in settings") so over-/under-triggering can be corrected
//! without a rebuild — keep this module the single source of truth.
//!
//! R2 posture (ADR-033): **fire rarely, suppress gently** — conservative-to-fire
//! (τ_conf 0.7), patient-to-suppress (escalating dismissal ladder, mute only at
//! the 3rd dismiss). ADR-032 makes the cap/sessionization adaptive with bounded
//! ranges and conservative cold-start defaults.

/// Trigger rule 1 — score threshold `τ_conf` (doc 08 §6.1, ADR-033: 0.6 ⟶ 0.7,
/// owner-lowered 2026-08-26: 0.7 ⟶ 0.4 — the engine never cleared the bar in
/// real use, so trade precision for actually firing). `[VERIFY — tuned against
/// SC7 at M3]`.
pub const TAU_CONF: f64 = 0.4;

/// Trigger rule 2 — cold-start weighted-support floor (doc 08 §6.2, Q23:
/// unchanged at 3). `[ASSUMPTION]`. Also the n-gram support floor; below this we
/// stay silent.
pub const COLD_START_SUPPORT_FLOOR: f64 = 3.0;

/// Semantic-assist threshold (doc 08 §5, Q30: unchanged): cosine similarity of
/// the current context embedding to a pattern's stored centroid ≥ this may
/// substitute for one token in antecedent matching. `[ASSUMPTION — evaluate at M3]`.
pub const SEMANTIC_SIMILARITY_THRESHOLD: f64 = 0.75;

/// Trigger rule 4 — per-signature cooldown, minutes (doc 08 §6.4, Q26: unchanged
/// at 30). `[ASSUMPTION]`. The dismissal ladder multiplies this per signature
/// (×2 after the 1st dismiss, ×4 after the 2nd — ADR-033).
pub const COOLDOWN_MIN: i64 = 30;

// --- Trigger rule 5 — the global cap is ADAPTIVE (ADR-032/Q25) ---
// fixed 4/hr ⟶ adaptive, click-through-driven, bounded [2, 8]/hr.

/// Adaptive-cap floor, suggestions per rolling hour (ADR-032).
pub const CAP_PER_HOUR_FLOOR: u32 = 2;
/// Adaptive-cap ceiling, suggestions per rolling hour (ADR-032). Hard bound —
/// the adaptation may never exceed it.
pub const CAP_PER_HOUR_CEILING: u32 = 8;
/// Cold-start cap default (start conservative, earn presence — ADR-032/033;
/// build-prompt default 4/hr). Adaptation raises/lowers it inside
/// `[CAP_PER_HOUR_FLOOR, CAP_PER_HOUR_CEILING]` on click-through evidence at M3+.
pub const CAP_PER_HOUR_DEFAULT: u32 = 4;

/// Sessionization (doc 08 §3, ADR-032/Q28): a **rolling idle-gap distribution**
/// decides the boundary (applied forward, never retro-sessionizing); this is the
/// **cold-start default** gap in minutes until enough of the user's own gap
/// distribution has accrued. `[ASSUMPTION]`.
pub const SESSION_GAP_COLD_START_MIN: i64 = 15;

// --- Recency half-lives — SPLIT by pattern type (ADR-033/Q77) ---
// single 7 d ⟶ temporal ~5 d (time-of-day habits shift fast) vs sequence ~14 d
// (workflows are stable). An occurrence's weight is `w = 0.5^(age_days / H)`.

/// Half-life for **sequence** (n-gram A→B→C) patterns, days (ADR-033). `[ASSUMPTION]`.
pub const HALF_LIFE_SEQUENCE_DAYS: f64 = 14.0;
/// Half-life for **temporal** (time-of-day) patterns, days (ADR-033). `[ASSUMPTION]`.
pub const HALF_LIFE_TEMPORAL_DAYS: f64 = 5.0;

// --- derived feedback / temporal constants (doc 08 §4, §7 — R2 ladder) ---

/// Temporal bucketing width, hours (doc 08 §4, Q76: unchanged): return-visit
/// periodicity is histogrammed into 2-hour, local-wall-clock buckets.
pub const TEMPORAL_BUCKET_HOURS: i64 = 2;

/// Temporal pattern floor (doc 08 §4): ≥ this many weighted returns in one
/// time-of-day bucket forms a `temporal` pattern.
pub const TEMPORAL_RETURN_FLOOR: f64 = 3.0;

/// Feedback multiplier on `suggestion_clicked` (doc 08 §7: ×1.25, clamped at 1.0).
pub const CLICK_DECAY_MULT: f64 = 1.25;

// --- Dismissal ladder (ADR-033): softened, escalating; mute only at the 3rd ---
// 1st dismiss → cooldown ×2 + decay ×0.8; 2nd → cooldown ×4 + decay ×0.6;
// 3rd → mute. (R1's single ×0.5 + two-in-24h mute is superseded.)

/// Decay multiplier applied on the 1st dismissal in the ladder window (ADR-033).
pub const DISMISS_DECAY_MULT_1ST: f64 = 0.8;
/// Decay multiplier applied on the 2nd dismissal (ADR-033).
pub const DISMISS_DECAY_MULT_2ND: f64 = 0.6;
/// Cooldown multiplier after the 1st dismissal (ADR-033): 30 min → 60 min.
pub const DISMISS_COOLDOWN_MULT_1ST: i64 = 2;
/// Cooldown multiplier after the 2nd dismissal (ADR-033): 30 min → 120 min.
pub const DISMISS_COOLDOWN_MULT_2ND: i64 = 4;
/// The dismissal count that mutes the signature (ADR-033: mute only at the 3rd).
pub const MUTE_DISMISS_COUNT: u32 = 3;
/// The trailing window the ladder counts dismissals within, hours. `[ASSUMPTION —
/// ADR-033 softened the trip-wire but kept the windowed count]`.
pub const MUTE_TRIGGER_WINDOW_HOURS: i64 = 24;
/// Mute duration once tripped, days (doc 08 §7). `[ASSUMPTION]`.
pub const MUTE_DURATION_DAYS: i64 = 7;

/// Feedback multiplier on `suggestion_expired` / ignored (doc 08 §7: ×0.9 unchanged).
pub const EXPIRE_DECAY_MULT: f64 = 0.9;

// --- Explicit "useful?" thumbs (Q81/ADR-040): a cleaner SC7 signal ---
// up ≈ strong click, down ≈ dismiss-with-signal (doc 08 §7 amendment).

/// Thumbs-up decay multiplier (stronger than a click; clamped at 1.0). `[ASSUMPTION]`.
pub const THUMBS_UP_DECAY_MULT: f64 = 1.5;
/// Thumbs-down decay multiplier (stronger than a dismissal; also advances the
/// dismissal ladder by one step). `[ASSUMPTION]`.
pub const THUMBS_DOWN_DECAY_MULT: f64 = 0.33;

/// Novelty suppression window, minutes (ADR-033): never suggest the foreground
/// resource *and* suppress any resource focused within the last ~10 min
/// ("I just closed that"). `[ASSUMPTION]`.
pub const NOVELTY_RECENT_FOCUS_MIN: i64 = 10;

/// Weekly-prune support threshold (doc 08 §9, Q76: unchanged): signatures with
/// weighted support below this are pruned to prevent pattern-table bloat.
pub const PRUNE_SUPPORT_FLOOR: f64 = 0.5;

/// The runtime-tunable subset of the engine's knobs — the `pattern_engine`
/// settings block (doc 08 §9 "tunables exposed in settings", owner decision #17
/// 2026-08-16). The compile-time constants above are the DEFAULTS: a missing or
/// invalid key falls back to its constant, so a partial (or absent) settings
/// block can never weaken the engine below its shipped posture. The dismissal
/// ladder / mute / novelty constants stay compile-time on purpose — the settings
/// block never named them.
///
/// `semantic_similarity_threshold` is parsed so the settings shape is honored
/// in full, but the semantic assist itself (doc 08 §5) is not wired into the
/// trigger path yet — the value is carried, not consulted.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineConfig {
    /// Trigger rule 1 (default [`TAU_CONF`]).
    pub tau_conf: f64,
    /// Trigger rule 2 (default [`COLD_START_SUPPORT_FLOOR`]).
    pub cold_start_support_floor: f64,
    /// Semantic-assist threshold (default [`SEMANTIC_SIMILARITY_THRESHOLD`]).
    pub semantic_similarity_threshold: f64,
    /// Trigger rule 4 base cooldown, minutes (default [`COOLDOWN_MIN`]).
    pub cooldown_min: i64,
    /// Adaptive-cap floor (default [`CAP_PER_HOUR_FLOOR`]).
    pub cap_per_hour_floor: u32,
    /// Adaptive-cap ceiling (default [`CAP_PER_HOUR_CEILING`]).
    pub cap_per_hour_ceiling: u32,
    /// Cold-start cap (default [`CAP_PER_HOUR_DEFAULT`]).
    pub cap_per_hour_default: u32,
    /// Sessionizer cold-start gap, minutes (default [`SESSION_GAP_COLD_START_MIN`]).
    pub session_gap_cold_start_min: i64,
    /// Sequence-pattern half-life, days (default [`HALF_LIFE_SEQUENCE_DAYS`]).
    pub half_life_sequence_days: f64,
    /// Temporal-pattern half-life, days (default [`HALF_LIFE_TEMPORAL_DAYS`]).
    pub half_life_temporal_days: f64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            tau_conf: TAU_CONF,
            cold_start_support_floor: COLD_START_SUPPORT_FLOOR,
            semantic_similarity_threshold: SEMANTIC_SIMILARITY_THRESHOLD,
            cooldown_min: COOLDOWN_MIN,
            cap_per_hour_floor: CAP_PER_HOUR_FLOOR,
            cap_per_hour_ceiling: CAP_PER_HOUR_CEILING,
            cap_per_hour_default: CAP_PER_HOUR_DEFAULT,
            session_gap_cold_start_min: SESSION_GAP_COLD_START_MIN,
            half_life_sequence_days: HALF_LIFE_SEQUENCE_DAYS,
            half_life_temporal_days: HALF_LIFE_TEMPORAL_DAYS,
        }
    }
}

impl EngineConfig {
    /// Parse the `pattern_engine` settings section (the shape seeded from
    /// `config/settings.default.json`). Every key is optional; a value outside
    /// its sane range is REJECTED in favor of the constant default (a typo must
    /// never mean "bubble on everything" — same posture as
    /// `retention_policy_from_settings`). The cap band is repaired to stay
    /// ordered: `floor ≤ default ≤ ceiling`, all ≥ 1.
    pub fn from_settings(section: &serde_json::Value) -> Self {
        let mut cfg = Self::default();
        let f64_in = |key: &str, lo: f64, hi: f64, dst: &mut f64| {
            if let Some(v) = section.get(key).and_then(serde_json::Value::as_f64) {
                if v > lo && v <= hi {
                    *dst = v;
                }
            }
        };
        let i64_pos = |key: &str, dst: &mut i64| {
            if let Some(v) = section.get(key).and_then(serde_json::Value::as_i64) {
                if v >= 1 {
                    *dst = v;
                }
            }
        };
        f64_in("tau_conf", 0.0, 1.0, &mut cfg.tau_conf);
        f64_in("cold_start_support_floor", 0.0, 1e6, &mut cfg.cold_start_support_floor);
        f64_in(
            "semantic_similarity_threshold",
            0.0,
            1.0,
            &mut cfg.semantic_similarity_threshold,
        );
        i64_pos("cooldown_min", &mut cfg.cooldown_min);
        i64_pos("session_gap_cold_start_min", &mut cfg.session_gap_cold_start_min);
        f64_in("half_life_sequence_days", 0.0, 3650.0, &mut cfg.half_life_sequence_days);
        f64_in("half_life_temporal_days", 0.0, 3650.0, &mut cfg.half_life_temporal_days);

        let u32_pos = |key: &str, dst: &mut u32| {
            if let Some(v) = section.get(key).and_then(serde_json::Value::as_u64) {
                if v >= 1 {
                    *dst = v.min(u32::MAX as u64) as u32;
                }
            }
        };
        u32_pos("cap_per_hour_floor", &mut cfg.cap_per_hour_floor);
        u32_pos("cap_per_hour_ceiling", &mut cfg.cap_per_hour_ceiling);
        u32_pos("cap_per_hour_default", &mut cfg.cap_per_hour_default);
        // Repair the band instead of silently mis-clamping later.
        cfg.cap_per_hour_ceiling = cfg.cap_per_hour_ceiling.max(cfg.cap_per_hour_floor);
        cfg.cap_per_hour_default = cfg
            .cap_per_hour_default
            .clamp(cfg.cap_per_hour_floor, cfg.cap_per_hour_ceiling);
        cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_or_empty_settings_yield_the_constant_defaults() {
        let cfg = EngineConfig::from_settings(&serde_json::json!({}));
        assert_eq!(cfg, EngineConfig::default());
        assert!((cfg.tau_conf - TAU_CONF).abs() < 1e-12);
        assert_eq!(cfg.cap_per_hour_default, CAP_PER_HOUR_DEFAULT);
    }

    #[test]
    fn present_keys_override_and_absent_keys_keep_defaults() {
        let cfg = EngineConfig::from_settings(&serde_json::json!({
            "tau_conf": 0.85,
            "cooldown_min": 45,
            "half_life_sequence_days": 21.0
        }));
        assert!((cfg.tau_conf - 0.85).abs() < 1e-12);
        assert_eq!(cfg.cooldown_min, 45);
        assert!((cfg.half_life_sequence_days - 21.0).abs() < 1e-12);
        // Untouched keys stay at their constants.
        assert!((cfg.cold_start_support_floor - COLD_START_SUPPORT_FLOOR).abs() < 1e-12);
        assert_eq!(cfg.session_gap_cold_start_min, SESSION_GAP_COLD_START_MIN);
    }

    #[test]
    fn out_of_range_values_fall_back_instead_of_weakening_the_engine() {
        let cfg = EngineConfig::from_settings(&serde_json::json!({
            "tau_conf": 0.0,              // "fire on everything" typo
            "cooldown_min": -5,           // negative cooldown
            "half_life_temporal_days": 0  // divide-by-zero bait
        }));
        assert_eq!(cfg, EngineConfig::default());
    }

    #[test]
    fn cap_band_is_repaired_to_stay_ordered() {
        let cfg = EngineConfig::from_settings(&serde_json::json!({
            "cap_per_hour_floor": 6,
            "cap_per_hour_ceiling": 3,   // crossed band
            "cap_per_hour_default": 100  // way outside
        }));
        assert_eq!(cfg.cap_per_hour_floor, 6);
        assert_eq!(cfg.cap_per_hour_ceiling, 6, "ceiling lifted to the floor");
        assert_eq!(cfg.cap_per_hour_default, 6, "default clamped into the band");
    }
}
