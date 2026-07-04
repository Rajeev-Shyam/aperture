//! Recency-weighted statistics & candidate scoring (doc 08 §4-§5).
//!
//! Support is recency-weighted: each occurrence contributes
//! `w = 0.5^(age_days / H)` with the half-life **split by pattern type**
//! (ADR-033): sequence patterns H ≈ 14 d, temporal patterns H ≈ 5 d (doc 08 §4).
//! Confidence is the recency-weighted conditional probability
//! `conf = W(antecedent ⇒ consequent) / W(antecedent ⇒ *)` (doc 08 §4).
//! The final candidate score multiplies confidence by the feedback decay,
//! connector freshness, and novelty (doc 08 §5):
//! ```text
//! score = conf × dismiss_decay × freshness(connector_state) × novelty
//! ```

use aperture_contracts::connector::ConnectorState;

use crate::config;

/// Recency weight of a single occurrence (doc 08 §4, ADR-033):
/// `w = 0.5^(age_days / half_life_days)`. `age_days` is `(now - ts) / 1 day`.
/// Callers pass [`config::HALF_LIFE_SEQUENCE_DAYS`] for n-gram patterns and
/// [`config::HALF_LIFE_TEMPORAL_DAYS`] for time-of-day patterns.
///
/// Pure and side-effect-free so [`crate::temporal`] can reuse it for weighted
/// return-visit mass.
pub fn recency_weight(now_ms: i64, occurrence_ms: i64, half_life_days: f64) -> f64 {
    let age_days = (now_ms - occurrence_ms).max(0) as f64 / 86_400_000.0;
    0.5_f64.powf(age_days / half_life_days)
}

/// Recency-weighted support and confidence for one antecedent ⇒ consequent
/// signature (doc 08 §4). Persisted in / loaded from the `patterns` table.
///
/// Incremental-decay bookkeeping: rather than re-summing every historical
/// occurrence on each read, the weighted sums are stored together with the
/// `last_updated_ms` they were valid at; [`decayed_to`](Self::decayed_to)
/// re-bases them to "now" by multiplying with the elapsed-time decay factor —
/// mathematically identical to per-occurrence weights because the exponential
/// factorizes: `0.5^((now-t)/H) = 0.5^((now-u)/H) * 0.5^((u-t)/H)`.
#[derive(Debug, Clone, Default)]
pub struct PatternStats {
    /// `W(antecedent ⇒ consequent)` — weighted occurrences of this exact n-gram,
    /// valid as of `last_updated_ms`.
    pub weighted_support: f64,
    /// `W(antecedent ⇒ *)` — weighted occurrences of the antecedent overall;
    /// the denominator for confidence (doc 08 §4), valid as of `last_updated_ms`.
    pub antecedent_total: f64,
    /// Feedback multiplier in `(0, 1]`-ish, clamped at 1.0 on reinforcement (doc 08 §7).
    pub dismiss_decay: f64,
    /// The instant the weighted sums were last re-based / credited (epoch ms).
    pub last_updated_ms: i64,
}

impl PatternStats {
    /// Fresh stats for a signature first seen now.
    pub fn new(now_ms: i64) -> Self {
        Self {
            weighted_support: 0.0,
            antecedent_total: 0.0,
            dismiss_decay: 1.0,
            last_updated_ms: now_ms,
        }
    }

    /// Re-base the weighted sums to `now_ms` (decay applied **at read time**,
    /// build prompt / doc 08 §4) with the sequence half-life.
    pub fn decayed_to(&self, now_ms: i64) -> PatternStats {
        let f = recency_weight(now_ms, self.last_updated_ms, config::HALF_LIFE_SEQUENCE_DAYS);
        PatternStats {
            weighted_support: self.weighted_support * f,
            antecedent_total: self.antecedent_total * f,
            dismiss_decay: self.dismiss_decay,
            last_updated_ms: now_ms,
        }
    }

    /// Credit one observed occurrence of this exact n-gram at `now_ms`
    /// (decays the running sums to now, then adds weight 1).
    pub fn credit_occurrence(&mut self, now_ms: i64) {
        *self = self.decayed_to(now_ms);
        self.weighted_support += 1.0;
        self.antecedent_total += 1.0;
    }

    /// Credit an occurrence of the antecedent that led to a *different*
    /// consequent (the `⇒ *` denominator grows, this row's support doesn't).
    pub fn credit_antecedent_only(&mut self, now_ms: i64) {
        *self = self.decayed_to(now_ms);
        self.antecedent_total += 1.0;
    }

    /// `conf = W(ant ⇒ cons) / W(ant ⇒ *)` (doc 08 §4); `0.0` when the
    /// antecedent has no weighted mass yet (cold start).
    pub fn confidence(&self) -> f64 {
        if self.antecedent_total <= 0.0 {
            0.0
        } else {
            (self.weighted_support / self.antecedent_total).min(1.0)
        }
    }
}

/// The connector-freshness factor (doc 08 §5): `1.0` while within the connector
/// TTL, else `0.0` so a stale state zeroes the candidate (no stale bubbles —
/// doc 10 / connector.rs `stale_after_ts`). Cross-checked again at trigger time
/// (doc 08 §6.3) before emitting.
pub fn freshness(state: &ConnectorState, now_ms: i64) -> f64 {
    match state.stale_after_ts {
        Some(stale_after) if now_ms >= stale_after => 0.0,
        _ => 1.0,
    }
}

/// The novelty factor (doc 08 §5, ADR-033): `0.0` if the consequent's resource
/// is already foreground **or was focused within the last ~10 min**
/// ([`config::NOVELTY_RECENT_FOCUS_MIN`] — "I just closed that"), else `1.0`.
///
/// `last_focused_ms` is the most recent time the consequent's resource was
/// foreground (`None` = never seen).
pub fn novelty(
    consequent_resource: Option<&str>,
    foreground_resource: Option<&str>,
    last_focused_ms: Option<i64>,
    now_ms: i64,
) -> f64 {
    match (consequent_resource, foreground_resource) {
        (Some(c), Some(f)) if c == f => return 0.0, // on screen right now
        _ => {}
    }
    if let Some(t) = last_focused_ms {
        if now_ms.saturating_sub(t) < config::NOVELTY_RECENT_FOCUS_MIN * 60_000 {
            return 0.0; // focused too recently (ADR-033)
        }
    }
    1.0
}

/// Final candidate score (doc 08 §5):
/// `conf × dismiss_decay × freshness × novelty`. Compared to
/// [`config::TAU_CONF`] at trigger time (doc 08 §6.1).
pub fn score(conf: f64, dismiss_decay: f64, freshness: f64, novelty: f64) -> f64 {
    conf * dismiss_decay * freshness * novelty
}

/// Cosine similarity between two equal-length vectors; `0.0` on length mismatch
/// or zero-norm input.
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        dot += (*x as f64) * (*y as f64);
        na += (*x as f64) * (*x as f64);
        nb += (*y as f64) * (*y as f64);
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Optional semantic assist (doc 08 §5, Q30 unchanged): cosine similarity of the
/// current context embedding to a pattern's stored centroid may substitute for
/// one token when it is ≥ [`config::SEMANTIC_SIMILARITY_THRESHOLD`].
/// `[ASSUMPTION — evaluate at M3]`.
pub fn semantic_substitutes(query: &[f32], centroid: &[f32]) -> bool {
    cosine(query, centroid) >= config::SEMANTIC_SIMILARITY_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;

    #[test]
    fn recency_weight_halves_at_half_life() {
        let w = recency_weight(14 * DAY, 0, 14.0);
        assert!((w - 0.5).abs() < 1e-9, "14 d at H=14 → 0.5 (got {w})");
        let wt = recency_weight(5 * DAY, 0, 5.0);
        assert!((wt - 0.5).abs() < 1e-9, "5 d at H=5 → 0.5 (temporal)");
        assert!((recency_weight(0, 0, 14.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn incremental_decay_matches_per_occurrence_math() {
        // Two occurrences at t=0 and t=14d, read at t=28d.
        let mut stats = PatternStats::new(0);
        stats.credit_occurrence(0);
        stats.credit_occurrence(14 * DAY);
        let read = stats.decayed_to(28 * DAY);
        // Direct sum: 0.5^(28/14) + 0.5^(14/14) = 0.25 + 0.5 = 0.75.
        assert!(
            (read.weighted_support - 0.75).abs() < 1e-9,
            "factorized decay == per-occurrence decay (got {})",
            read.weighted_support
        );
    }

    #[test]
    fn confidence_is_conditional_probability() {
        let mut s = PatternStats::new(0);
        s.credit_occurrence(0); // ant ⇒ cons
        s.credit_antecedent_only(0); // ant ⇒ other
        assert!((s.confidence() - 0.5).abs() < 1e-9);
        assert_eq!(PatternStats::new(0).confidence(), 0.0, "cold start = 0");
    }

    #[test]
    fn freshness_zeroes_stale_connectors() {
        let st = ConnectorState {
            id: "x".into(),
            connector_type: "youtube".into(),
            reconstruct_payload: serde_json::json!({}),
            payload_version: 1,
            captured_ts: 0,
            stale_after_ts: Some(100),
        };
        assert_eq!(freshness(&st, 50), 1.0);
        assert_eq!(freshness(&st, 100), 0.0, "at/after TTL → zero (no stale bubbles)");
    }

    #[test]
    fn novelty_suppresses_foreground_and_recent_focus() {
        let now = 100 * 60_000;
        assert_eq!(novelty(Some("youtube"), Some("youtube"), None, now), 0.0);
        assert_eq!(
            novelty(Some("youtube"), Some("ide"), Some(now - 5 * 60_000), now),
            0.0,
            "focused 5 min ago → suppressed (ADR-033 ~10 min window)"
        );
        assert_eq!(
            novelty(Some("youtube"), Some("ide"), Some(now - 30 * 60_000), now),
            1.0
        );
        assert_eq!(novelty(Some("youtube"), None, None, now), 1.0);
    }

    #[test]
    fn cosine_and_semantic_assist() {
        let a = [1.0f32, 0.0, 0.0];
        let b = [1.0f32, 0.0, 0.0];
        let c = [0.0f32, 1.0, 0.0];
        assert!((cosine(&a, &b) - 1.0).abs() < 1e-9);
        assert!(cosine(&a, &c).abs() < 1e-9);
        assert!(semantic_substitutes(&a, &b));
        assert!(!semantic_substitutes(&a, &c));
    }
}
