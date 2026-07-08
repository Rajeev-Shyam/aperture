//! Voice-query retrieval over history (doc 03 §5, doc 07 §5).
//!
//! For a [`Query`](crate::intent_classifier::Intent::Query) utterance:
//! ```text
//! transcript ─► embed (768-d nomic-embed, same model as ingest, doc 03 §5)
//!            ─► temporal-phrase windowing ("yesterday" sets recency_floor/ceiling)
//!            ─► KNN over ctx_vec (k = 25) JOIN events LEFT JOIN connector_state
//!            ─► re-rank: (1 - dist_norm) · recency_decay(ts) · (resumable? 1.3 : 1.0)
//!            ─► top hit above the score floor ─► answer bubble {title, when, source}
//! ```
//! Budget: the whole path is **< 2 s** (doc 07 §5). **No cloud unless the user
//! escalates** — retrieval is fully local (doc 03 §5, doc 07 §5).
//!
//! No hit above the floor ⇒ an honest empty-state bubble ("Nothing matching in
//! your history") that still offers *Ask Claude* (doc 07 §5).
//!
//! Runs against `aperture_db` (the doc 03 §5 KNN query) + an
//! [`aperture_embedding::Embedder`]. Both are injected, so the whole path is
//! unit-testable end-to-end with the in-memory DB + `HashEmbedder` (no model, no
//! GPU). Embedding and the vec search are the only non-trivial costs against the
//! < 2 s budget; the facade (doc 07 §2) wraps the call in that timeout.

use aperture_db::Db;
use aperture_embedding::Embedder;

/// End-to-end retrieval budget (doc 07 §5).
pub const RETRIEVAL_BUDGET_MS: u64 = 2_000;

/// KNN fan-out before re-rank (doc 03 §5).
pub const KNN_K: usize = 25;

/// Resumable candidates get a re-rank boost (doc 03 §5).
pub const RESUMABLE_BOOST: f64 = 1.3;

/// Default recency window when the query names no time (doc 03 §5): now − 7 days.
pub const DEFAULT_RECENCY_WINDOW_DAYS: i64 = 7;

/// Half-life for the re-rank recency decay (doc 03 §5): a hit that old scores ½
/// the weight of "now". Matches the default 7-day window intuition.
pub const RECENCY_HALF_LIFE_DAYS: f64 = 7.0;

/// Minimum re-rank score for a hit to surface as an answer (vs. empty state).
/// [ASSUMPTION: tuned in M6, doc 03 §5 / doc 07 §5].
pub const SCORE_FLOOR: f64 = 0.35;

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;

/// Time window derived from temporal phrases in the transcript (doc 03 §5).
/// Bounds are epoch ms; `None` ceiling means "up to now".
#[derive(Debug, Clone, Copy)]
pub struct TimeWindow {
    pub recency_floor_ms: i64,
    pub ceiling_ms: Option<i64>,
}

/// Parse temporal phrases ("yesterday", "last week", "this morning") into a
/// [`TimeWindow`] **before** KNN, so the SQL `WHERE e.ts >= :recency_floor` bounds
/// the search (doc 03 §5). Falls back to the default window when no phrase matches.
pub fn temporal_window(transcript: &str, now_ms: i64) -> TimeWindow {
    let t = transcript.to_lowercase();
    // (phrase, floor = now − back_floor, ceiling = Some(now − back_ceiling) | None).
    // Ordered MOST-SPECIFIC FIRST so "last week" wins before "this week", and
    // bounded past-windows ("yesterday", "last …") set a ceiling; open windows
    // ("today", "this week") leave the ceiling at "now".
    const RULES: &[(&str, i64, Option<i64>)] = &[
        ("last month", 60 * DAY_MS, Some(30 * DAY_MS)),
        ("last week", 14 * DAY_MS, Some(7 * DAY_MS)),
        ("yesterday", 2 * DAY_MS, Some(1 * DAY_MS)),
        ("this month", 30 * DAY_MS, None),
        ("this week", 7 * DAY_MS, None),
        ("this morning", 1 * DAY_MS, None),
        ("earlier today", 1 * DAY_MS, None),
        ("today", 1 * DAY_MS, None),
        ("an hour ago", 3 * HOUR_MS, None),
        ("earlier", 3 * HOUR_MS, None),
        ("just now", 15 * 60_000, None),
        ("a moment ago", 15 * 60_000, None),
        ("recently", 30 * DAY_MS, None),
    ];
    for (phrase, back_floor, back_ceiling) in RULES {
        if t.contains(phrase) {
            return TimeWindow {
                recency_floor_ms: now_ms - back_floor,
                ceiling_ms: back_ceiling.map(|c| now_ms - c),
            };
        }
    }
    // No temporal phrase → the default trailing window (doc 03 §5).
    TimeWindow {
        recency_floor_ms: now_ms - DEFAULT_RECENCY_WINDOW_DAYS * DAY_MS,
        ceiling_ms: None,
    }
}

/// Re-rank recency decay: `0.5^(age / half-life)` (doc 03 §5), so a fresher hit
/// scores higher. Pure; the half-life is [`RECENCY_HALF_LIFE_DAYS`].
pub fn recency_decay(ts_ms: i64, now_ms: i64) -> f64 {
    let age_days = (now_ms - ts_ms).max(0) as f64 / DAY_MS as f64;
    0.5_f64.powf(age_days / RECENCY_HALF_LIFE_DAYS)
}

/// One re-ranked candidate (doc 03 §5).
#[derive(Debug, Clone)]
pub struct ScoredHit {
    pub event_id: i64,
    pub ts_ms: i64,
    pub window_title: Option<String>,
    /// The joined connector's type (`youtube`/`document`/…), if any.
    pub connector_type: Option<String>,
    /// The connector's reconstruct payload, for the bubble's Resume target.
    pub reconstruct_payload: Option<serde_json::Value>,
    /// The `connector_id` iff the hit is resumable — a connector is joined AND
    /// still fresh (doc 03 §5 / doc 10 TTL). Sets the boost + the *Resume* action.
    pub resume_ref: Option<String>,
    /// Final re-rank score (doc 03 §5).
    pub score: f64,
}

/// The answer bubble the shell renders (doc 07 §5, doc 11). `Resume` dispatches
/// Critical Path B on the hit's `connector_state`; `Ask Claude` assembles a
/// payload for the preview→Send gate (doc 07 §5) — both UI-side, never auto-run.
#[derive(Debug, Clone)]
pub struct AnswerBubble {
    pub title: String,
    /// e.g. `"12:34 · 2h ago"`.
    pub when: String,
    /// Source/connector label.
    pub source: String,
    /// `connector_id` for the *Resume* action, if the hit is resumable (doc 07 §5).
    pub resume_action_ref: Option<String>,
    /// `true` for the honest empty-state bubble ("Nothing matching in your
    /// history") — *Ask Claude* is still offered (doc 07 §5).
    pub empty_state: bool,
}

/// Re-rank score: `(1 - dist_norm) · recency_decay(ts) · (resumable ? 1.3 : 1.0)`
/// (doc 03 §5). Pure so the weight tuning in M6 is testable.
pub fn rerank_score(dist_norm: f64, recency_decay: f64, resumable: bool) -> f64 {
    let boost = if resumable { RESUMABLE_BOOST } else { 1.0 };
    (1.0 - dist_norm) * recency_decay * boost
}

impl AnswerBubble {
    /// The honest empty state (doc 07 §5) — still offers *Ask Claude* (UI-side).
    pub fn empty() -> Self {
        Self {
            title: "Nothing matching in your history".to_string(),
            when: String::new(),
            source: String::new(),
            resume_action_ref: None,
            empty_state: true,
        }
    }

    fn from_hit(hit: &ScoredHit, now_ms: i64) -> Self {
        Self {
            title: hit
                .window_title
                .clone()
                .or_else(|| hit.connector_type.clone())
                .unwrap_or_else(|| "Untitled".to_string()),
            when: format_when(hit.ts_ms, now_ms),
            source: hit
                .connector_type
                .clone()
                .unwrap_or_else(|| "history".to_string()),
            resume_action_ref: hit.resume_ref.clone(),
            empty_state: false,
        }
    }
}

/// Human "when" label relative to now (e.g. `"2h ago"`). The doc's wall-clock
/// prefix ("12:34 · …") is a UI-side embellishment (no tz math in this crate).
fn format_when(ts_ms: i64, now_ms: i64) -> String {
    let mins = (now_ms - ts_ms).max(0) / 60_000;
    if mins < 1 {
        "just now".to_string()
    } else if mins < 60 {
        format!("{mins}m ago")
    } else if mins < 60 * 24 {
        format!("{}h ago", mins / 60)
    } else {
        format!("{}d ago", mins / (60 * 24))
    }
}

/// Run the full local retrieval for a query transcript and produce the answer
/// bubble (doc 03 §5, doc 07 §5). The facade wraps this in [`RETRIEVAL_BUDGET_MS`].
///
/// `db` + `embedder` are injected (same nomic model as ingest, doc 03 §5, so
/// distances are comparable). Fully local — no cloud unless the user escalates.
pub async fn run(
    db: &Db,
    embedder: &dyn Embedder,
    transcript: &str,
    now_ms: i64,
) -> Result<AnswerBubble, RetrievalError> {
    // 1. Embed the query (nomic's `search_query:` task prefix, doc 03 §5 step 1).
    let query = format!("{}{}", aperture_embedding::QUERY_PREFIX, transcript);
    let vec = embedder
        .embed(&query)
        .map_err(|e| RetrievalError::Embedding(e.to_string()))?;

    // 2. Temporal window before KNN so the SQL floor bounds the search.
    let win = temporal_window(transcript, now_ms);

    // 3. KNN over ctx_vec, floor-bounded (doc 03 §5). knn takes only a floor; the
    //    ceiling (for "yesterday"/"last week") is applied in the re-rank filter.
    let floor = win.recency_floor_ms.max(0);
    let hits = db
        .knn(&vec, KNN_K as u32, floor)
        .map_err(|e| RetrievalError::Query(e.to_string()))?;

    // 4. Re-rank: (1 − dist_norm) · recency_decay · resumable-boost (doc 03 §5).
    //    Embeddings are L2-normalized, so vec0's L2 distance lies in [0, 2];
    //    dist_norm = distance / 2 maps it into [0, 1].
    let mut scored: Vec<ScoredHit> = hits
        .into_iter()
        .filter(|h| win.ceiling_ms.map_or(true, |c| h.ts <= c))
        .map(|h| {
            let dist_norm = (h.distance / 2.0).clamp(0.0, 1.0);
            let fresh = h.stale_after_ts.map_or(true, |s| now_ms < s);
            let resumable = h.connector_id.is_some() && fresh;
            let score = rerank_score(dist_norm, recency_decay(h.ts, now_ms), resumable);
            ScoredHit {
                event_id: h.event_id,
                ts_ms: h.ts,
                window_title: h.window_title,
                connector_type: h.connector_type,
                reconstruct_payload: h.reconstruct_payload,
                resume_ref: if resumable { h.connector_id } else { None },
                score,
            }
        })
        .collect();
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));

    // 5. Top hit above the floor → answer, else the honest empty state (doc 07 §5).
    match scored.first() {
        Some(top) if top.score >= SCORE_FLOOR => Ok(AnswerBubble::from_hit(top, now_ms)),
        _ => Ok(AnswerBubble::empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_contracts::event::{Event, EventType};
    use aperture_embedding::HashEmbedder;

    const DAY: i64 = DAY_MS;

    fn window_event(ts: i64, title: &str) -> Event {
        Event {
            id: 0,
            ts,
            r#type: EventType::WindowFocus,
            app: None,
            process: Some("chrome.exe".into()),
            window_title: Some(title.into()),
            payload: serde_json::json!({}),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        }
    }

    #[test]
    fn temporal_window_parses_phrases_and_falls_back() {
        let now = 1_000 * DAY; // arbitrary, well past epoch
        let yday = temporal_window("what did I read yesterday", now);
        assert_eq!(yday.recency_floor_ms, now - 2 * DAY);
        assert_eq!(yday.ceiling_ms, Some(now - DAY), "yesterday is a bounded window");

        let week = temporal_window("find the doc from this week", now);
        assert_eq!(week.recency_floor_ms, now - 7 * DAY);
        assert_eq!(week.ceiling_ms, None, "open window up to now");

        // "last week" must win over the bare "week" and set a ceiling.
        let last = temporal_window("the video from last week", now);
        assert_eq!(last.ceiling_ms, Some(now - 7 * DAY));
        assert_eq!(last.recency_floor_ms, now - 14 * DAY);

        let none = temporal_window("resume the react tutorial", now);
        assert_eq!(none.recency_floor_ms, now - DEFAULT_RECENCY_WINDOW_DAYS * DAY);
        assert_eq!(none.ceiling_ms, None);
    }

    #[test]
    fn recency_decay_halves_at_the_half_life() {
        let now = 100 * DAY;
        assert!((recency_decay(now, now) - 1.0).abs() < 1e-9);
        let one_hl = now - (RECENCY_HALF_LIFE_DAYS as i64) * DAY;
        assert!((recency_decay(one_hl, now) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn rerank_boost_only_for_resumable() {
        assert!(rerank_score(0.0, 1.0, true) > rerank_score(0.0, 1.0, false));
        assert_eq!(rerank_score(0.0, 1.0, true), RESUMABLE_BOOST);
    }

    #[tokio::test]
    async fn run_returns_the_nearest_matching_bubble() {
        let embedder = HashEmbedder;
        let db = Db::open_in_memory().expect("in-memory db");
        let now = 1_000 * DAY;
        let query = "resume the react tutorial";
        // Store a doc whose embedding equals what `run` will embed → nearest hit.
        let matching = embedder
            .embed(&format!("{}{}", aperture_embedding::QUERY_PREFIX, query))
            .unwrap();
        db.insert_event_with_context(&window_event(now, "React Tutorial — YouTube"), None, Some(&matching))
            .unwrap();
        // A lexically unrelated distractor.
        let distractor = embedder.embed("kernel scheduler preemption latency").unwrap();
        db.insert_event_with_context(&window_event(now, "Kernel notes"), None, Some(&distractor))
            .unwrap();

        let bubble = run(&db, &embedder, query, now).await.expect("retrieval ok");
        assert!(!bubble.empty_state, "a match must not be the empty state");
        assert!(bubble.title.contains("React"), "nearest hit wins the bubble, got {:?}", bubble.title);
    }

    #[tokio::test]
    async fn run_returns_the_honest_empty_state_on_no_history() {
        let embedder = HashEmbedder;
        let db = Db::open_in_memory().expect("in-memory db");
        let bubble = run(&db, &embedder, "find that pdf", 1_000 * DAY).await.unwrap();
        assert!(bubble.empty_state, "no history ⇒ honest empty state (doc 07 §5)");
        assert!(bubble.resume_action_ref.is_none());
    }

    #[tokio::test]
    async fn a_fresh_connector_is_resumable_and_a_stale_one_is_not() {
        // Review #10: exercise the connector_id / stale_after_ts / freshness / boost
        // / resume_ref path with NON-null connector data (all prior tests used NULL).
        use aperture_contracts::connector::ConnectorState;
        let embedder = HashEmbedder;
        let now = 1_000 * DAY;
        let query = "resume the video";
        let vec = embedder
            .embed(&format!("{}{}", aperture_embedding::QUERY_PREFIX, query))
            .unwrap();
        let cstate = |id: &str, stale_after: i64| ConnectorState {
            id: id.into(),
            connector_type: "youtube".into(),
            reconstruct_payload: serde_json::json!({ "video_id": "abc" }),
            payload_version: 1,
            captured_ts: now,
            stale_after_ts: Some(stale_after),
        };

        // Fresh: stale_after in the future ⇒ resumable, carries the Resume ref.
        let fresh = Db::open_in_memory().unwrap();
        fresh.insert_connector_state(&cstate("conn-fresh", now + DAY)).unwrap();
        let mut ev = window_event(now, "Rust tutorial — YouTube");
        ev.connector_id = Some("conn-fresh".into());
        fresh.insert_event_with_context(&ev, None, Some(&vec)).unwrap();
        let bubble = run(&fresh, &embedder, query, now).await.unwrap();
        assert!(!bubble.empty_state);
        assert_eq!(bubble.resume_action_ref.as_deref(), Some("conn-fresh"), "fresh ⇒ Resume offered");
        assert_eq!(bubble.source, "youtube");

        // Stale: stale_after in the past ⇒ still an answer, but NO Resume action.
        let stale = Db::open_in_memory().unwrap();
        stale.insert_connector_state(&cstate("conn-stale", now - DAY)).unwrap();
        let mut ev2 = window_event(now, "Old tutorial");
        ev2.connector_id = Some("conn-stale".into());
        stale.insert_event_with_context(&ev2, None, Some(&vec)).unwrap();
        let bubble2 = run(&stale, &embedder, query, now).await.unwrap();
        assert!(!bubble2.empty_state, "the hit still answers");
        assert_eq!(bubble2.resume_action_ref, None, "stale connector ⇒ no Resume");
    }
}

/// Errors during retrieval.
#[derive(Debug, thiserror::Error)]
pub enum RetrievalError {
    /// Embedding the transcript failed.
    #[error("embedding failed: {0}")]
    Embedding(String),
    /// The KNN / join query failed.
    #[error("history query failed: {0}")]
    Query(String),
    /// Exceeded the [`RETRIEVAL_BUDGET_MS`] budget (doc 07 §5).
    #[error("retrieval exceeded the {0} ms budget")]
    Timeout(u64),
}
