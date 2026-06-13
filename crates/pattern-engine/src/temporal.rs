//! Temporal patterns — per-resource return-visit periodicity (doc 08 §4).
//!
//! Independently of n-grams, each resource's return visits are histogrammed into
//! time-of-day buckets ([`config::TEMPORAL_BUCKET_HOURS`]-hour, 12 buckets/day).
//! Each return contributes the same recency weight `w = 0.5^(age_days/7)` as
//! n-gram support (doc 08 §4 / [`crate::scorer`]). A resource with
//! ≥ [`config::TEMPORAL_RETURN_FLOOR`] weighted returns in one bucket forms a
//! `temporal` pattern — e.g. "opens the budget sheet ~9am".
//!
//! Buckets are keyed to **local wall-clock** by design, so DST / clock changes
//! shift habits with the user rather than fracturing them (doc 08 §9).

use crate::config;

/// Number of [`config::TEMPORAL_BUCKET_HOURS`]-hour buckets spanning a day.
pub const BUCKETS_PER_DAY: usize = (24 / config::TEMPORAL_BUCKET_HOURS) as usize;

/// Local time-of-day bucket index in `0..BUCKETS_PER_DAY` (doc 08 §4).
///
/// `ts_ms` is epoch milliseconds; bucketing uses the **local** hour.
pub fn bucket_of(_ts_ms: i64) -> usize {
    // TODO(M3): convert epoch ms → local wall-clock hour (doc 08 §9 DST note),
    // then hour / TEMPORAL_BUCKET_HOURS. Local, not UTC, by design.
    todo!("M3: local-wall-clock 2-hour bucket index (doc 08 §4)")
}

/// A recency-weighted return-visit histogram for one resource (doc 08 §4).
#[derive(Debug, Clone)]
pub struct TemporalHistogram {
    /// The resource this histogram tracks (coarse `resource_class`, doc 08 §2).
    pub resource_class: String,
    /// Weighted return mass per time-of-day bucket.
    pub buckets: [f64; BUCKETS_PER_DAY],
}

impl TemporalHistogram {
    /// Empty histogram for `resource_class`.
    pub fn new(resource_class: String) -> Self {
        Self {
            resource_class,
            buckets: [0.0; BUCKETS_PER_DAY],
        }
    }

    /// Record a return visit at `ts_ms` weighted by `recency_weight`
    /// (`w = 0.5^(age_days/7)`, doc 08 §4 / [`crate::scorer::recency_weight`]).
    pub fn record_return(&mut self, _ts_ms: i64, _recency_weight: f64) {
        // TODO(M3): buckets[bucket_of(ts_ms)] += recency_weight.
        todo!("M3: accumulate weighted return into its bucket (doc 08 §4)")
    }

    /// `true` if any bucket has ≥ [`config::TEMPORAL_RETURN_FLOOR`] weighted
    /// returns, i.e. a `temporal` pattern has formed (doc 08 §4).
    pub fn is_temporal(&self) -> bool {
        self.buckets
            .iter()
            .any(|&w| w >= config::TEMPORAL_RETURN_FLOOR)
    }

    /// The peak bucket and its weighted mass, if this resource has formed a
    /// temporal pattern (doc 08 §4). Used to decide whether "now" is a
    /// predicted return window for proactive triggering.
    pub fn peak_bucket(&self) -> Option<(usize, f64)> {
        // TODO(M3): argmax over buckets; return None if below floor.
        todo!("M3: argmax bucket for temporal prediction (doc 08 §4)")
    }
}
