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
//! TODO(M6:) implement against `aperture_db` (the `ctx_vec`/`events`/`connector_state`
//! query in doc 03 §5) + `aperture_embedding`. Embedding and the vec search are the
//! only non-trivial costs against the < 2 s budget.

use aperture_contracts::connector::ConnectorState;

/// End-to-end retrieval budget (doc 07 §5).
pub const RETRIEVAL_BUDGET_MS: u64 = 2_000;

/// KNN fan-out before re-rank (doc 03 §5).
pub const KNN_K: usize = 25;

/// Resumable candidates get a re-rank boost (doc 03 §5).
pub const RESUMABLE_BOOST: f64 = 1.3;

/// Default recency window when the query names no time (doc 03 §5): now − 7 days.
pub const DEFAULT_RECENCY_WINDOW_DAYS: i64 = 7;

/// Minimum re-rank score for a hit to surface as an answer (vs. empty state).
/// [ASSUMPTION: tuned in M6, doc 03 §5 / doc 07 §5].
pub const SCORE_FLOOR: f64 = 0.35;

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
pub fn temporal_window(_transcript: &str, _now_ms: i64) -> TimeWindow {
    // TODO(M6:) lightweight phrase lexicon -> floor/ceiling; default = now-7d.
    todo!("M6: temporal-phrase windowing (yesterday/last week/...) -> TimeWindow")
}

/// One re-ranked candidate (doc 03 §5).
#[derive(Debug, Clone)]
pub struct ScoredHit {
    pub event_id: i64,
    pub ts_ms: i64,
    pub window_title: Option<String>,
    /// Present iff the event had a fresh `connector_state` join (doc 03 §5); its
    /// presence sets the resumable boost and enables the bubble's *Resume* action.
    pub connector: Option<ConnectorState>,
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

/// Run the full local retrieval for a query transcript and produce the answer
/// bubble (doc 03 §5, doc 07 §5). Must complete within [`RETRIEVAL_BUDGET_MS`].
pub async fn run(_transcript: &str) -> Result<AnswerBubble, RetrievalError> {
    // TODO(M6:) pipeline:
    //   1. vec = aperture_embedding::embed(transcript)            // 768-d
    //   2. win = temporal_window(transcript, now)
    //   3. rows = aperture_db: KNN(ctx_vec, vec, k=25) JOIN events
    //             LEFT JOIN connector_state WHERE ts in win        // doc 03 §5 SQL
    //   4. hits = rows.map(rerank_score).sorted_desc()
    //   5. top.score >= SCORE_FLOOR ? AnswerBubble::from(top) : AnswerBubble::empty()
    todo!("M6: embed -> temporal window -> KNN -> rerank -> answer bubble (<2 s)")
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
