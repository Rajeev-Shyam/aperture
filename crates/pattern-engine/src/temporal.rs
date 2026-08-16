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
use crate::normalizer::Token;

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
    /// Weighted return mass per time-of-day bucket, valid as of `last_update_ms`.
    pub buckets: [f64; BUCKETS_PER_DAY],
    /// The instant the bucket masses were last re-based (epoch ms); `None`
    /// until the first return is recorded.
    last_update_ms: Option<i64>,
}

impl TemporalHistogram {
    /// Empty histogram for `resource_class`.
    pub fn new(resource_class: String) -> Self {
        Self {
            resource_class,
            buckets: [0.0; BUCKETS_PER_DAY],
            last_update_ms: None,
        }
    }

    /// Record a return visit at `ts_ms`: existing mass ages to `ts_ms` by the
    /// factorized decay (`0.5^(Δdays/H)` — the same math as
    /// [`crate::scorer::PatternStats::decayed_to`]), then the visit adds weight
    /// 1 to its bucket. `half_life_days` is the temporal half-life
    /// (ADR-033 ≈ 5 d — [`config::HALF_LIFE_TEMPORAL_DAYS`], runtime-tunable
    /// via `pattern_engine.half_life_temporal_days`, decision #17). Before
    /// this aging existed the buckets only ever grew, so a long-dead habit
    /// stayed "formed" forever.
    pub fn record_return(&mut self, ts_ms: i64, half_life_days: f64) {
        if let Some(last) = self.last_update_ms {
            let f = crate::scorer::recency_weight(ts_ms, last, half_life_days);
            for b in &mut self.buckets {
                *b *= f;
            }
        }
        self.last_update_ms = Some(ts_ms);
        self.buckets[bucket_of(ts_ms)] += 1.0;
    }

    /// Total weighted return mass across all buckets — the denominator for the
    /// temporal candidate's confidence (`peak / total`, decision #16).
    pub fn total_mass(&self) -> f64 {
        self.buckets.iter().sum()
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

/// The synthetic consequent [`Token`] a temporal pattern predicts (owner
/// decision #16, 2026-08-16). Only the `resource_class` carries meaning — the
/// connector lookup (trigger rule 3) and the action template read nothing
/// else; the fixed `app_class`/`action` just keep the encoding well-formed so
/// [`signature_for`] round-trips through `ngram::parse_signature` at hydrate.
pub fn consequent_token(resource_class: &str) -> Token {
    Token {
        app_class: "temporal".to_string(),
        action: "return".to_string(),
        resource_class: Some(resource_class.to_string()),
    }
}

/// The `patterns`-row signature keying one temporal pattern: resource × peak
/// time-of-day bucket (decision #16). Rides the normal signature grammar
/// (`antecedent ⇒ consequent`) so flush/hydrate/feedback all work unchanged;
/// the `temporal:<bucket>` antecedent can never collide with a real n-gram
/// tail key (encoded tokens always carry two `:`s, this carries one), so a
/// hydrated temporal row never leaks into sequence candidate generation.
pub fn signature_for(bucket: usize, consequent: &Token) -> String {
    format!("temporal:{bucket} ⇒ {}", consequent.encode())
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
        let h_days = config::HALF_LIFE_TEMPORAL_DAYS;
        let mut h = TemporalHistogram::new("doc:xlsx".into());
        let nine_am = 9 * 3_600_000;
        assert!(!h.is_temporal());
        // A daily 9am habit: mass is recency-weighted (H = 5 d), so 3 calendar
        // returns weigh < 3.0 and the pattern forms on the 4th day — matching
        // the sequence engine's "once-a-day habit fires ~day 4" posture.
        for day in 0..3 {
            h.record_return(nine_am + day * 86_400_000, h_days);
        }
        assert!(!h.is_temporal(), "3 aged returns stay under the floor");
        h.record_return(nine_am + 3 * 86_400_000, h_days);
        assert!(h.is_temporal(), "4th daily return crosses the weighted floor (doc 08 §4)");
        let (idx, mass) = h.peak_bucket().expect("peak");
        assert_eq!(idx, 4, "9am → bucket 4");
        // 0.5^(3/5) + 0.5^(2/5) + 0.5^(1/5) + 1 ≈ 3.29.
        assert!(mass > 3.0 && mass < 3.5, "recency-weighted mass, got {mass}");
        assert!((h.total_mass() - mass).abs() < 1e-9, "all mass in one bucket");
        assert!(h.now_is_peak(nine_am + 3 * 86_400_000));
        assert!(!h.now_is_peak(nine_am + 3 * 86_400_000 + 6 * 3_600_000));
    }

    #[test]
    fn mass_decays_so_a_dead_habit_unforms() {
        std::env::set_var("APERTURE_TZ_OFFSET_MIN", "0");
        let h_days = config::HALF_LIFE_TEMPORAL_DAYS;
        let mut h = TemporalHistogram::new("youtube".into());
        let nine_am = 9 * 3_600_000;
        for day in 0..5 {
            h.record_return(nine_am + day * 86_400_000, h_days);
        }
        assert!(h.is_temporal());
        // One stray return 30 days later: the old 9am mass has halved 6 times.
        h.record_return(nine_am + 35 * 86_400_000 + 6 * 3_600_000, h_days);
        assert!(!h.is_temporal(), "a month-dead habit no longer predicts returns");
    }

    #[test]
    fn temporal_signature_round_trips_through_parse_signature() {
        // Decision #16: temporal rows persist via the normal flush/hydrate path.
        for res in ["youtube", "url:docs.rs", "doc:xlsx"] {
            let tok = consequent_token(res);
            let sig = signature_for(4, &tok);
            let (ant_key, decoded) =
                crate::ngram::parse_signature(&sig).expect("temporal signature parses");
            assert_eq!(ant_key, "temporal:4 ⇒ *");
            assert_eq!(decoded, tok, "consequent survives hydrate");
            assert_eq!(decoded.resource_class.as_deref(), Some(res));
        }
    }
}
