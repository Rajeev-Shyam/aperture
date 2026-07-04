//! Temporal patterns — per-resource return-visit periodicity (doc 08 §4).
//!
//! Independently of n-grams, each resource's return visits are histogrammed into
//! time-of-day buckets ([`config::TEMPORAL_BUCKET_HOURS`]-hour, 12 buckets/day).
//! Each return contributes a recency weight `w = 0.5^(age_days/H)` with the
//! **temporal half-life H ≈ 5 d** (ADR-033 — time-of-day habits shift fast;
//! see [`crate::scorer::recency_weight`] + [`config::HALF_LIFE_TEMPORAL_DAYS`]).
//! A resource with ≥ [`config::TEMPORAL_RETURN_FLOOR`] weighted returns in one
//! bucket forms a `temporal` pattern — e.g. "opens the budget sheet ~9am".
//!
//! Buckets are keyed to **local wall-clock** by design, so DST / clock changes
//! shift habits with the user rather than fracturing them (doc 08 §9).

use crate::config;

/// Number of [`config::TEMPORAL_BUCKET_HOURS`]-hour buckets spanning a day.
pub const BUCKETS_PER_DAY: usize = (24 / config::TEMPORAL_BUCKET_HOURS) as usize;

/// Local time-of-day bucket index in `0..BUCKETS_PER_DAY` (doc 08 §4).
///
/// `ts_ms` is epoch milliseconds; bucketing uses the **local** hour.
///
/// [VERIFY resolved — Step 0]: local wall-clock conversion uses `chrono::Local`
/// (per-timestamp offset, so DST transitions shift buckets *with* the user —
/// exactly the doc 08 §9 intent). Tests may pin the offset via
/// `APERTURE_TZ_OFFSET_MIN` for determinism.
pub fn bucket_of(ts_ms: i64) -> usize {
    if let Some(offset_min) = env_offset_override() {
        return bucket_of_with_offset(ts_ms, offset_min);
    }
    use chrono::{Local, TimeZone, Timelike};
    match Local.timestamp_millis_opt(ts_ms) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
            (dt.hour() as i64 / config::TEMPORAL_BUCKET_HOURS) as usize % BUCKETS_PER_DAY
        }
        chrono::LocalResult::None => bucket_of_with_offset(ts_ms, 0),
    }
}

/// Testable core: bucket for a timestamp given a fixed UTC offset in minutes.
pub fn bucket_of_with_offset(ts_ms: i64, offset_min: i64) -> usize {
    let local_ms = ts_ms + offset_min * 60_000;
    let ms_per_day = 86_400_000i64;
    let ms_of_day = local_ms.rem_euclid(ms_per_day);
    let hour = ms_of_day / 3_600_000;
    (hour / config::TEMPORAL_BUCKET_HOURS) as usize % BUCKETS_PER_DAY
}

/// Deterministic offset override for tests/harnesses.
fn env_offset_override() -> Option<i64> {
    std::env::var("APERTURE_TZ_OFFSET_MIN").ok()?.parse().ok()
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
    /// (`w = 0.5^(age_days/5)` for temporal patterns — ADR-033;
    /// [`crate::scorer::recency_weight`] with [`config::HALF_LIFE_TEMPORAL_DAYS`]).
    pub fn record_return(&mut self, ts_ms: i64, recency_weight: f64) {
        self.buckets[bucket_of(ts_ms)] += recency_weight;
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
        let (idx, &mass) = self
            .buckets
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))?;
        if mass >= config::TEMPORAL_RETURN_FLOOR {
            Some((idx, mass))
        } else {
            None
        }
    }

    /// Whether `now_ms` falls inside the peak (predicted-return) bucket.
    pub fn now_is_peak(&self, now_ms: i64) -> bool {
        matches!(self.peak_bucket(), Some((idx, _)) if idx == bucket_of(now_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_2h_local_wall_clock() {
        // 09:30 local with +60 min offset == 08:30 UTC.
        let utc_0830_ms = 8 * 3_600_000 + 30 * 60_000;
        assert_eq!(bucket_of_with_offset(utc_0830_ms, 60), 4, "09:30 local → bucket 4 (08–10h)");
        assert_eq!(bucket_of_with_offset(0, 0), 0);
        assert_eq!(bucket_of_with_offset(23 * 3_600_000, 0), 11);
    }

    #[test]
    fn negative_offsets_and_day_wrap_are_safe() {
        // 00:30 UTC with -120 min offset = 22:30 previous local day → bucket 11.
        let ms = 30 * 60_000;
        assert_eq!(bucket_of_with_offset(ms, -120), 11);
    }

    #[test]
    fn temporal_pattern_forms_at_the_floor() {
        std::env::set_var("APERTURE_TZ_OFFSET_MIN", "0");
        let mut h = TemporalHistogram::new("doc:xlsx".into());
        let nine_am = 9 * 3_600_000;
        assert!(!h.is_temporal());
        h.record_return(nine_am, 1.0);
        h.record_return(nine_am + 86_400_000, 1.0);
        h.record_return(nine_am + 2 * 86_400_000, 1.0);
        assert!(h.is_temporal(), "3 weighted returns in one bucket (doc 08 §4)");
        let (idx, mass) = h.peak_bucket().expect("peak");
        assert_eq!(idx, 4, "9am → bucket 4");
        assert!((mass - 3.0).abs() < 1e-9);
        assert!(h.now_is_peak(nine_am + 3 * 86_400_000));
        assert!(!h.now_is_peak(nine_am + 3 * 86_400_000 + 6 * 3_600_000));
    }
}
