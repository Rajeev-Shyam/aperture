//! Feedback loop (doc 08 §7) + weekly prune (doc 08 §9).
//!
//! User reactions adjust each signature's `dismiss_decay` and may mute it
//! (R2 ladder — ADR-033: fire rarely, suppress gently):
//! | Signal                | Effect (doc 08 §7, ADR-033) |
//! |-----------------------|------------------------------|
//! | `suggestion_clicked`  | `decay ← min(1.0, decay × 1.25)`; support reinforced |
//! | 1st dismissal         | cooldown ×2, `decay × 0.8` |
//! | 2nd dismissal         | cooldown ×4, `decay × 0.6` |
//! | 3rd dismissal         | **mute** (7 d) |
//! | `suggestion_expired`  | mild: `decay × 0.9` |
//! | "useful?" 👍 (Q81)    | strong reinforce: `decay ← min(1.0, decay × 1.5)` |
//! | "useful?" 👎 (Q81)    | strong penalty: `decay × 0.33` + advances the ladder |
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
    /// `suggestion_dismissed` — penalize per the ladder (ADR-033), mute at the 3rd.
    Dismissed,
    /// `suggestion_expired` (ignored) — mild penalty.
    Expired,
    /// Explicit "useful?" 👍 (Q81) — strong reinforce; feeds SC7 directly.
    ThumbsUp,
    /// Explicit "useful?" 👎 (Q81) — strong penalty; advances the dismissal ladder.
    ThumbsDown,
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

/// Apply a feedback `Signal` to a signature's stats + mute state (doc 08 §7,
/// ADR-033 ladder).
///
/// Mutates `dismiss_decay` per the table above (reinforcements clamp at `1.0`).
/// Dismissals escalate: 1st → cooldown ×2 + decay ×0.8; 2nd → cooldown ×4 +
/// decay ×0.6; the [`config::MUTE_DISMISS_COUNT`]-th (3rd) inside
/// [`config::MUTE_TRIGGER_WINDOW_HOURS`] sets `muted_until = now +
/// MUTE_DURATION_DAYS`. A 👎 applies its own decay *and* advances the ladder.
pub fn apply(_stats: &mut PatternStats, _mute: &mut MuteState, _signal: Signal, _now_ms: i64) {
    // TODO(M3): match signal:
    //   Clicked    => decay = (decay * CLICK_DECAY_MULT).min(1.0); reinforce support
    //   Dismissed  => push now into recent_dismissals, prune older than
    //                 MUTE_TRIGGER_WINDOW_HOURS; ladder step n = count:
    //                   1 => decay *= DISMISS_DECAY_MULT_1ST (cooldown ×2 read at trigger)
    //                   2 => decay *= DISMISS_DECAY_MULT_2ND (cooldown ×4)
    //                   >= MUTE_DISMISS_COUNT => muted_until = now + MUTE_DURATION_DAYS
    //   Expired    => decay *= EXPIRE_DECAY_MULT
    //   ThumbsUp   => decay = (decay * THUMBS_UP_DECAY_MULT).min(1.0)
    //   ThumbsDown => decay *= THUMBS_DOWN_DECAY_MULT; also counts as a Dismissed
    //                 ladder step
    // Then persist back to the patterns table (doc 03 / aperture_db), including
    // suggestions.useful_rating for the thumbs (Q81).
    let _ = (
        config::CLICK_DECAY_MULT,
        config::DISMISS_DECAY_MULT_1ST,
        config::DISMISS_DECAY_MULT_2ND,
        config::EXPIRE_DECAY_MULT,
        config::THUMBS_UP_DECAY_MULT,
        config::THUMBS_DOWN_DECAY_MULT,
        config::MUTE_DISMISS_COUNT,
        config::MUTE_TRIGGER_WINDOW_HOURS,
        config::MUTE_DURATION_DAYS,
    );
    todo!("M3: apply feedback ladder + mute at 3rd dismiss (doc 08 §7, ADR-033)")
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
