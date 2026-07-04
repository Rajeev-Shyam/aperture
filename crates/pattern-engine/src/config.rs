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

// TODO(M3): expose these via the settings store so the UI can adjust them live
// (doc 08 §9); for now they are compile-time constants matching the doc.

/// Trigger rule 1 — score threshold `τ_conf` (doc 08 §6.1, ADR-033: 0.6 ⟶ 0.7 —
/// fewer, higher-confidence bubbles). `[VERIFY — tuned against SC7 at M3]`.
pub const TAU_CONF: f64 = 0.7;

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
