//! Unconditional `voice_utterance` logging (doc 07 §3, doc 03 §2).
//!
//! **Locked decision B (doc 07 §3):** *every* transcribed utterance is stored as a
//! `voice_utterance` event and embedded — the telemetry role is **unconditional**,
//! regardless of intent. This runs in [`crate::VoiceSubsystem::ptt_up`] **before**
//! any intent branch, so a query, an escalation, and a plain statement are all
//! recorded identically. (Only a sub-300 ms accidental tap — discarded before
//! transcription — is never stored, because no real utterance occurred.)
//!
//! The `voice_utterance` payload fields are fixed by the taxonomy (doc 03 §2):
//! `{ transcript, duration_ms, stt_model, confidence, intent(query|telemetry) }`.
//! The intent string is the §4 classification; escalation is recorded as `query`
//! for the taxonomy's two-value `intent` field (it is a user-initiated request,
//! not background telemetry).
//!
//! Writes go through the Tier-0 single-writer (doc 03 §1) — this crate hands the
//! [`Event`] + its 768-d embedding to `aperture_db` in one atomic transaction; it
//! never opens the DB itself. The `Db` + `Embedder` are injected, so the path is
//! unit-testable with the in-memory DB + `HashEmbedder` (no model, no GPU).

use aperture_contracts::event::{Event, EventType};
use aperture_contracts::Intent as PayloadIntent;

/// Default STT model recorded on the utterance (doc 07 §3). Opt-in
/// distil-large-v3 int8 overrides this from settings.
pub const DEFAULT_STT_MODEL: &str = "whisper-small";

/// The taxonomy's two-value `intent` field for `voice_utterance` (doc 03 §2).
/// Distinct from the richer [`crate::intent_classifier::Intent`]: escalation is a
/// user-initiated request, so it is logged as `Query`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggedIntent {
    Query,
    Telemetry,
}

impl LoggedIntent {
    /// Collapse the §4 classification to the taxonomy's two values (doc 03 §2).
    pub fn from_classified(intent: crate::intent_classifier::Intent) -> Self {
        use crate::intent_classifier::Intent;
        match intent {
            Intent::Query | Intent::Escalation => LoggedIntent::Query,
            Intent::Telemetry => LoggedIntent::Telemetry,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            LoggedIntent::Query => "query",
            LoggedIntent::Telemetry => "telemetry",
        }
    }
}

/// The recorded fields of one utterance (doc 03 §2 `voice_utterance` payload).
#[derive(Debug, Clone)]
pub struct UtteranceRecord {
    pub transcript: String,
    pub duration_ms: u32,
    pub stt_model: String,
    pub confidence: f32,
    pub intent: LoggedIntent,
}

impl UtteranceRecord {
    /// Build the in-flight [`Event`] (`id = 0`; the DB assigns it on insert,
    /// doc 15 §1). `app`/`process`/`window_title` are `None` — voice is not tied to
    /// a foreground window.
    pub fn to_event(&self, ts_ms: i64) -> Event {
        Event {
            id: 0,
            ts: ts_ms,
            r#type: EventType::VoiceUtterance,
            app: None,
            process: None,
            window_title: None,
            payload: serde_json::json!({
                "transcript": self.transcript,
                "duration_ms": self.duration_ms,
                "stt_model": self.stt_model,
                "confidence": self.confidence,
                "intent": self.intent.as_str(),
            }),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        }
    }
}

/// Errors from the unconditional telemetry write.
#[derive(Debug, thiserror::Error)]
pub enum LoggerError {
    /// Persisting the `voice_utterance` event failed.
    #[error("store failed: {0}")]
    Store(String),
    /// Computing or persisting the 768-d embedding failed.
    #[error("embed failed: {0}")]
    Embed(String),
}

/// Store the `voice_utterance` event **and** its embedding — unconditionally
/// (locked decision B, doc 07 §3). Returns the DB-assigned `event_id` so the
/// embedding row in `ctx_vec` (doc 03 §3) keys to the same event.
pub async fn store_and_embed(
    db: &aperture_db::Db,
    embedder: &dyn aperture_embedding::Embedder,
    record: &UtteranceRecord,
    now_ms: i64,
) -> Result<i64, LoggerError> {
    let ev = record.to_event(now_ms);
    // Embed the transcript as a stored document (doc 03 §5; the embedder applies
    // the DOC task prefix). A blank transcript embeds nothing but still stores the
    // event — the telemetry role is unconditional (locked decision B, doc 07 §3).
    let embedding = if record.transcript.trim().is_empty() {
        None
    } else {
        Some(
            embedder
                .embed(&record.transcript)
                .map_err(|e| LoggerError::Embed(e.to_string()))?,
        )
    };
    // One atomic single-writer transaction: the event row and its `ctx_vec`
    // embedding commit together (doc 03 §1). Returns the DB-assigned event_id.
    db.insert_event_with_context(&ev, None, embedding.as_deref())
        .map_err(|e| LoggerError::Store(e.to_string()))
}

/// Map the §4 intent to the [`ContextPayload`](aperture_contracts::ContextPayload)
/// intent for an escalation draft (doc 07 §5): a voice query escalated to Claude
/// is [`PayloadIntent::AnswerQuery`]. Helper for the facade's escalation branch.
pub fn escalation_payload_intent() -> PayloadIntent {
    PayloadIntent::AnswerQuery
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_db::Db;
    use aperture_embedding::{Embedder, HashEmbedder};

    fn record(transcript: &str, confidence: f32) -> UtteranceRecord {
        UtteranceRecord {
            transcript: transcript.into(),
            duration_ms: 1_200,
            stt_model: DEFAULT_STT_MODEL.into(),
            confidence,
            intent: LoggedIntent::Telemetry,
        }
    }

    #[tokio::test]
    async fn store_and_embed_persists_event_and_indexes_the_transcript() {
        let db = Db::open_in_memory().expect("in-memory db");
        let embedder = HashEmbedder;
        let id = store_and_embed(&db, &embedder, &record("note to self buy milk", 0.92), 5_000)
            .await
            .expect("stored");
        assert!(id > 0);
        // The event persisted as a voice utterance with its payload fields.
        let ev = db.read_event(id).expect("event row");
        assert_eq!(ev.r#type, EventType::VoiceUtterance);
        assert_eq!(ev.payload["transcript"], "note to self buy milk");
        assert_eq!(ev.payload["intent"], "telemetry");
        // The embedding landed in ctx_vec — a KNN by the same text finds it.
        let q = embedder.embed("note to self buy milk").unwrap();
        let hits = db.knn(&q, 5, 0).expect("knn");
        assert!(
            hits.iter().any(|h| h.event_id == id),
            "the utterance transcript is retrievable from ctx_vec"
        );
    }

    #[tokio::test]
    async fn blank_transcript_still_stores_the_event() {
        let db = Db::open_in_memory().expect("in-memory db");
        let embedder = HashEmbedder;
        // A sub-300 ms tap is discarded upstream; but a genuine empty transcription
        // must still record the utterance event (telemetry is unconditional).
        let id = store_and_embed(&db, &embedder, &record("   ", 0.4), 1)
            .await
            .expect("stored");
        assert!(db.read_event(id).is_ok(), "blank transcript still stores the event");
    }
}
