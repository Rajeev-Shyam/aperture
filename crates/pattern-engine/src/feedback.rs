//! Feedback loop (doc 08 §7, ADR-033 ladder) + weekly prune (doc 08 §9).
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

/// Mute bookkeeping per signature for the ladder (doc 08 §7, ADR-033: mute only
/// at the 3rd dismissal inside the trailing window).
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

    /// The current dismissal-ladder step in `0..=2` (feeds the ×1/×2/×4 cooldown
    /// multiplier — [`crate::trigger::TriggerInput::dismissal_step`]).
    pub fn dismissal_step(&self, now_ms: i64) -> u32 {
        let window_floor = now_ms - config::MUTE_TRIGGER_WINDOW_HOURS * 3_600_000;
        (self
            .recent_dismissals
            .iter()
            .filter(|&&t| t > window_floor)
            .count() as u32)
            .min(2)
    }

    /// Register a dismissal at `now_ms`; returns the ladder step it landed on
    /// (1-based) and sets the mute when the count reaches
    /// [`config::MUTE_DISMISS_COUNT`].
    fn register_dismissal(&mut self, now_ms: i64) -> u32 {
        let window_floor = now_ms - config::MUTE_TRIGGER_WINDOW_HOURS * 3_600_000;
        self.recent_dismissals.retain(|&t| t > window_floor);
        self.recent_dismissals.push(now_ms);
        let count = self.recent_dismissals.len() as u32;
        if count >= config::MUTE_DISMISS_COUNT {
            self.muted_until = Some(now_ms + config::MUTE_DURATION_DAYS * 86_400_000);
        }
        count
    }
}

/// Apply a feedback `Signal` to a signature's stats + mute state (doc 08 §7,
/// ADR-033 ladder). Mutates `dismiss_decay` per the table above
/// (reinforcements clamp at `1.0`); the caller persists to the `patterns`
/// table (doc 03) and to `suggestions.useful_rating` for thumbs (Q81).
pub fn apply(stats: &mut PatternStats, mute: &mut MuteState, signal: Signal, now_ms: i64) {
    match signal {
        Signal::Clicked => {
            stats.dismiss_decay = (stats.dismiss_decay * config::CLICK_DECAY_MULT).min(1.0);
        }
        Signal::Dismissed => {
            let step = mute.register_dismissal(now_ms);
            stats.dismiss_decay *= match step {
                1 => config::DISMISS_DECAY_MULT_1ST,
                2 => config::DISMISS_DECAY_MULT_2ND,
                // 3rd+: the mute is the penalty; keep the 2nd-step decay factor.
                _ => config::DISMISS_DECAY_MULT_2ND,
            };
        }
        Signal::Expired => {
            stats.dismiss_decay *= config::EXPIRE_DECAY_MULT;
        }
        Signal::ThumbsUp => {
            stats.dismiss_decay = (stats.dismiss_decay * config::THUMBS_UP_DECAY_MULT).min(1.0);
        }
        Signal::ThumbsDown => {
            stats.dismiss_decay *= config::THUMBS_DOWN_DECAY_MULT;
            // A 👎 also advances the ladder — it is a dismissal *with signal* (Q81).
            mute.register_dismissal(now_ms);
        }
    }
}

/// Weekly maintenance (doc 08 §9): prune signatures whose recency-weighted
/// support has decayed below [`config::PRUNE_SUPPORT_FLOOR`], preventing
/// pattern-table bloat. Operates on the engine's in-memory cache; the caller
/// mirrors the deletions to the `patterns` table (doc 03).
///
/// Returns the pruned signatures.
pub fn prune_stale_patterns(
    cache: &mut std::collections::HashMap<String, (PatternStats, MuteState)>,
    now_ms: i64,
) -> Vec<String> {
    let doomed: Vec<String> = cache
        .iter()
        .filter(|(_, (stats, _))| {
            stats.decayed_to(now_ms).weighted_support < config::PRUNE_SUPPORT_FLOOR
        })
        .map(|(sig, _)| sig.clone())
        .collect();
    for sig in &doomed {
        cache.remove(sig);
    }
    doomed
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 86_400_000;

    #[test]
    fn dismissal_ladder_escalates_and_mutes_at_third() {
        let mut stats = PatternStats::new(0);
        stats.dismiss_decay = 1.0;
        let mut mute = MuteState::default();

        apply(&mut stats, &mut mute, Signal::Dismissed, 0);
        assert!((stats.dismiss_decay - 0.8).abs() < 1e-9, "1st: ×0.8 (ADR-033)");
        assert!(!mute.is_muted(HOUR), "no mute at 1st");
        assert_eq!(mute.dismissal_step(HOUR), 1);

        apply(&mut stats, &mut mute, Signal::Dismissed, HOUR);
        assert!((stats.dismiss_decay - 0.48).abs() < 1e-9, "2nd: ×0.6");
        assert!(!mute.is_muted(2 * HOUR), "no mute at 2nd (R1's 2-strike is superseded)");

        apply(&mut stats, &mut mute, Signal::Dismissed, 2 * HOUR);
        assert!(mute.is_muted(3 * HOUR), "3rd dismisses → muted (ADR-033)");
        assert!(
            !mute.is_muted(2 * HOUR + config::MUTE_DURATION_DAYS * DAY + 1),
            "mute expires after 7 d"
        );
    }

    #[test]
    fn window_resets_the_ladder() {
        let mut stats = PatternStats::new(0);
        let mut mute = MuteState::default();
        apply(&mut stats, &mut mute, Signal::Dismissed, 0);
        apply(&mut stats, &mut mute, Signal::Dismissed, HOUR);
        // Third dismissal lands past the 24 h window → old ones fall out; no mute.
        apply(&mut stats, &mut mute, Signal::Dismissed, 26 * HOUR);
        assert!(!mute.is_muted(27 * HOUR), "windowed count, not lifetime count");
    }

    #[test]
    fn clicks_and_thumbs_reinforce_clamped_at_one() {
        let mut stats = PatternStats::new(0);
        stats.dismiss_decay = 0.9;
        let mut mute = MuteState::default();
        apply(&mut stats, &mut mute, Signal::Clicked, 0);
        assert!((stats.dismiss_decay - 1.0).abs() < 1e-9, "0.9×1.25 clamps at 1.0");

        stats.dismiss_decay = 0.5;
        apply(&mut stats, &mut mute, Signal::ThumbsUp, 0);
        assert!((stats.dismiss_decay - 0.75).abs() < 1e-9, "👍 ×1.5");
    }

    #[test]
    fn thumbs_down_penalizes_and_advances_ladder() {
        let mut stats = PatternStats::new(0);
        stats.dismiss_decay = 1.0;
        let mut mute = MuteState::default();
        apply(&mut stats, &mut mute, Signal::ThumbsDown, 0);
        assert!((stats.dismiss_decay - 0.33).abs() < 1e-9);
        assert_eq!(mute.dismissal_step(HOUR), 1, "👎 counts on the ladder (Q81)");
    }

    #[test]
    fn prune_removes_decayed_support() {
        let mut cache = std::collections::HashMap::new();
        let mut strong = PatternStats::new(0);
        strong.credit_occurrence(0);
        strong.credit_occurrence(0);
        strong.credit_occurrence(0); // support 3 at t=0
        let mut weak = PatternStats::new(0);
        weak.credit_occurrence(0); // support 1 at t=0

        cache.insert("strong".to_string(), (strong, MuteState::default()));
        cache.insert("weak".to_string(), (weak, MuteState::default()));

        // 28 days later (H=14): strong → 3×0.25 = 0.75 (kept); weak → 0.25 (pruned).
        let pruned = prune_stale_patterns(&mut cache, 28 * DAY);
        assert_eq!(pruned, vec!["weak".to_string()]);
        assert!(cache.contains_key("strong"));
    }
}
