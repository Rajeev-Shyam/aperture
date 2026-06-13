//! Recency-weighted statistics & candidate scoring (doc 08 §4-§5).
//!
//! Support is recency-weighted: each occurrence contributes
//! `w = 0.5^(age_days / HALF_LIFE_DAYS)` (exponential half-life 7 days, doc 08 §4).
//! Confidence is the recency-weighted conditional probability
//! `conf = W(antecedent ⇒ consequent) / W(antecedent ⇒ *)` (doc 08 §4).
//! The final candidate score multiplies confidence by the feedback decay,
//! connector freshness, and novelty (doc 08 §5):
//! ```text
//! score = conf × dismiss_decay × freshness(connector_state) × novelty
//! ```

use aperture_contracts::connector::ConnectorState;

use crate::config;

/// Recency weight of a single occurrence (doc 08 §4):
/// `w = 0.5^(age_days / HALF_LIFE_DAYS)`. `age_days` is `(now - ts) / 1 day`.
///
/// Pure and side-effect-free so [`crate::temporal`] can reuse it for weighted
/// return-visit mass.
pub fn recency_weight(now_ms: i64, occurrence_ms: i64) -> f64 {
    let age_days = (now_ms - occurrence_ms).max(0) as f64 / 86_400_000.0;
    0.5_f64.powf(age_days / config::HALF_LIFE_DAYS)
}

/// Recency-weighted support and confidence for one antecedent ⇒ consequent
/// signature (doc 08 §4). Persisted in / loaded from the `patterns` table.
#[derive(Debug, Clone, Default)]
pub struct PatternStats {
    /// `W(antecedent ⇒ consequent)` — weighted occurrences of this exact n-gram.
    pub weighted_support: f64,
    /// `W(antecedent ⇒ *)` — weighted occurrences of the antecedent overall;
    /// the denominator for confidence (doc 08 §4).
    pub antecedent_total: f64,
    /// Feedback multiplier in `(0, 1]`-ish, clamped at 1.0 on clicks (doc 08 §7).
    pub dismiss_decay: f64,
}

impl PatternStats {
    /// `conf = W(ant ⇒ cons) / W(ant ⇒ *)` (doc 08 §4); `0.0` when the
    /// antecedent has no weighted mass yet (cold start).
    pub fn confidence(&self) -> f64 {
        if self.antecedent_total <= 0.0 {
            0.0
        } else {
            self.weighted_support / self.antecedent_total
        }
    }
}

/// The connector-freshness factor (doc 08 §5): `1.0` while within the connector
/// TTL, else `0.0` so a stale state zeroes the candidate (no stale bubbles —
/// doc 10 / connector.rs `stale_after_ts`).
pub fn freshness(_state: &ConnectorState, _now_ms: i64) -> f64 {
    // TODO(M4): 1.0 if now_ms < state.stale_after_ts (or unset), else 0.0.
    // Cross-checked again at trigger time (doc 08 §6.3) before emitting.
    todo!("M3: TTL freshness factor from ConnectorState.stale_after_ts (doc 08 §5)")
}

/// The novelty factor (doc 08 §5): `0.0` if the consequent's resource is already
/// foreground (never suggest what's on screen), else `1.0`.
pub fn novelty(_consequent_resource: Option<&str>, _foreground_resource: Option<&str>) -> f64 {
    // TODO(M3): 0.0 when consequent resource == current foreground resource,
    // else 1.0 (doc 08 §5 / §6.6).
    todo!("M3: novelty — suppress if consequent already foreground (doc 08 §5)")
}

/// Final candidate score (doc 08 §5):
/// `conf × dismiss_decay × freshness × novelty`. Compared to
/// [`config::TAU_CONF`] at trigger time (doc 08 §6.1).
pub fn score(conf: f64, dismiss_decay: f64, freshness: f64, novelty: f64) -> f64 {
    conf * dismiss_decay * freshness * novelty
}

/// Optional semantic assist (doc 08 §5): cosine similarity of the current
/// context embedding to a pattern's stored centroid may substitute for one
/// token when it is ≥ [`config::SEMANTIC_SIMILARITY_THRESHOLD`].
/// `[ASSUMPTION — evaluate at M3]`.
pub fn semantic_substitutes(_query: &[f32], _centroid: &[f32]) -> bool {
    // TODO(M3): cosine(query, centroid) >= SEMANTIC_SIMILARITY_THRESHOLD using
    // aperture_embedding's similarity helper; gate behind an evaluation at M3.
    todo!("M3: cosine ≥ 0.75 token substitution (doc 08 §5)")
}
