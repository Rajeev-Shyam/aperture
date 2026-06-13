//! Feedback loop (doc 08 §7) + weekly prune (doc 08 §9).
//!
//! User reactions adjust each signature's `dismiss_decay` and may mute it:
//! | Signal                | Effect (doc 08 §7) |
//! |-----------------------|--------------------|
//! | `suggestion_clicked`  | `decay ← min(1.0, decay × 1.25)`; support reinforced |
//! | `suggestion_dismissed`| `decay ← decay × 0.5`; **two** dismissals in 24 h ⇒ muted 7 d |
//! | `suggestion_expired`  | mild: `decay × 0.9` |
//!
//! These write back to the `patterns` table (doc 03) and are the lever that
//! meets SC7 without cloud help (doc 08 §7). All CPU-only, zero-cloud.

use crate::config;
use crate::scorer::PatternStats;

/// A user reaction to a shown suggestion (doc 08 §7). Mirrors the facade's
/// [`crate::FeedbackEvent`]; kept here so the math lives next to the constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// `suggestion_clicked` — reinforce.
    Clicked,
    /// `suggestion_dismissed` — penalize, and arm the mute trip-wire.
    Dismissed,
    /// `suggestion_expired` (ignored) — mild penalty.
    Expired,
}

/// Mute bookkeeping per signature for the "two dismissals in 24 h" rule (doc 08 §7).
#[derive(Debug, Clone, Default)]
pub struct MuteState {
    /// Timestamps (epoch ms) of dismissals within the trailing trip-wire window.
    pub recent_dismissals: Vec<i64>,
    /// If set, the signature is muted until this `ts` (epoch ms).
    pub muted_until: Option<i64>,
}

impl MuteState {
    /// Whether this signature is currently muted (doc 08 §7).
    pub fn is_muted(&self, now_ms: i64) -> bool {
        matches!(self.muted_until, Some(until) if now_ms < until)
    }
}

/// Apply a feedback `Signal` to a signature's stats + mute state (doc 08 §7).
///
/// Mutates `dismiss_decay` per the table above (clicks clamp at `1.0`) and, on a
/// second dismissal inside [`config::MUTE_TRIGGER_WINDOW_HOURS`], sets
/// `muted_until = now + MUTE_DURATION_DAYS`.
pub fn apply(_stats: &mut PatternStats, _mute: &mut MuteState, _signal: Signal, _now_ms: i64) {
    // TODO(M3): match signal:
    //   Clicked   => decay = (decay * CLICK_DECAY_MULT).min(1.0); reinforce support
    //   Dismissed => decay *= DISMISS_DECAY_MULT; push now into recent_dismissals,
    //                prune older than MUTE_TRIGGER_WINDOW_HOURS; if count >=
    //                MUTE_DISMISS_COUNT, muted_until = now + MUTE_DURATION_DAYS
    //   Expired   => decay *= EXPIRE_DECAY_MULT
    // Then persist back to the patterns table (doc 03 / aperture_db).
    let _ = (
        config::CLICK_DECAY_MULT,
        config::DISMISS_DECAY_MULT,
        config::EXPIRE_DECAY_MULT,
        config::MUTE_DISMISS_COUNT,
        config::MUTE_TRIGGER_WINDOW_HOURS,
        config::MUTE_DURATION_DAYS,
    );
    todo!("M3: apply feedback decay + mute trip-wire (doc 08 §7)")
}

/// Weekly maintenance (doc 08 §9): prune signatures whose weighted support has
/// decayed below [`config::PRUNE_SUPPORT_FLOOR`], preventing pattern-table bloat.
///
/// Runs against the `patterns` table; returns the number of rows pruned.
pub fn prune_stale_patterns() -> usize {
    // TODO(M3): DELETE FROM patterns WHERE weighted_support < PRUNE_SUPPORT_FLOOR
    // (recompute support with current recency weights first). Scheduled weekly by
    // the orchestrator (doc 12); CPU-only, zero-cloud.
    let _ = config::PRUNE_SUPPORT_FLOOR;
    todo!("M3: weekly prune of weighted_support < 0.5 (doc 08 §9)")
}
