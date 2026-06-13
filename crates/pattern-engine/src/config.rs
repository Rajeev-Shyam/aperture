//! Tunable constants for the pattern engine (doc 08 §3-§7).
//!
//! These are the levers SC7 is tuned against (doc 16 M3); doc 08 marks several
//! as `[ASSUMPTION]` / `[VERIFY]`. They are surfaced in settings (doc 08 §9
//! "tunables exposed in settings") so over-/under-triggering can be corrected
//! without a rebuild — keep this module the single source of truth.

// TODO(M3): expose these via the settings store so the UI can adjust them live
// (doc 08 §9); for now they are compile-time constants matching the doc.

/// Trigger rule 1 — score threshold `τ_conf` (doc 08 §6.1).
/// `[VERIFY — tuned against SC7 at M3]`.
pub const TAU_CONF: f64 = 0.6;

/// Trigger rule 2 — cold-start weighted-support floor (doc 08 §6.2).
/// `[ASSUMPTION]`. Also the n-gram support floor; below this we stay silent.
pub const COLD_START_SUPPORT_FLOOR: f64 = 3.0;

/// Semantic-assist threshold (doc 08 §5): cosine similarity of the current
/// context embedding to a pattern's stored centroid ≥ this may substitute for
/// one token in antecedent matching. `[ASSUMPTION — evaluate at M3]`.
pub const SEMANTIC_SIMILARITY_THRESHOLD: f64 = 0.75;

/// Trigger rule 4 — per-signature cooldown, minutes (doc 08 §6.4). `[ASSUMPTION]`.
pub const COOLDOWN_MIN: i64 = 30;

/// Trigger rule 5 — global cap, suggestions per rolling hour (doc 08 §6.5).
/// `[ASSUMPTION]`. On overflow the lowest-score candidate is dropped.
pub const CAP_PER_HOUR: u32 = 4;

/// Sessionization gap, minutes (doc 08 §3): no input activity for this long
/// starts a new `session_id`. `[ASSUMPTION]`.
pub const SESSION_GAP_MIN: i64 = 15;

/// Recency half-life, days (doc 08 §4): an occurrence's weight is
/// `w = 0.5^(age_days / HALF_LIFE_DAYS)`. `[ASSUMPTION]`. Ages habits out in
/// ~3 weeks (doc 08 §9 concept-drift row).
pub const HALF_LIFE_DAYS: f64 = 7.0;

// --- derived feedback / temporal constants (doc 08 §4, §7) ---

/// Temporal bucketing width, hours (doc 08 §4): return-visit periodicity is
/// histogrammed into 2-hour, local-wall-clock buckets.
pub const TEMPORAL_BUCKET_HOURS: i64 = 2;

/// Temporal pattern floor (doc 08 §4): ≥ this many weighted returns in one
/// time-of-day bucket forms a `temporal` pattern.
pub const TEMPORAL_RETURN_FLOOR: f64 = 3.0;

/// Feedback multiplier on `suggestion_clicked` (doc 08 §7).
pub const CLICK_DECAY_MULT: f64 = 1.25;

/// Feedback multiplier on `suggestion_dismissed` (doc 08 §7).
pub const DISMISS_DECAY_MULT: f64 = 0.5;

/// Feedback multiplier on `suggestion_expired` / ignored (doc 08 §7).
pub const EXPIRE_DECAY_MULT: f64 = 0.9;

/// Dismissals within this window that trigger a mute (doc 08 §7): two in 24 h.
pub const MUTE_DISMISS_COUNT: u32 = 2;

/// Mute window for the dismissal trip-wire, hours (doc 08 §7): two dismissals
/// in this many hours ⇒ signature muted.
pub const MUTE_TRIGGER_WINDOW_HOURS: i64 = 24;

/// Mute duration once tripped, days (doc 08 §7). `[ASSUMPTION]`.
pub const MUTE_DURATION_DAYS: i64 = 7;

/// Weekly-prune support threshold (doc 08 §9): signatures with weighted support
/// below this are pruned to prevent pattern-table bloat.
pub const PRUNE_SUPPORT_FLOOR: f64 = 0.5;
