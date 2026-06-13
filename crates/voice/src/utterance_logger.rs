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
//! [`Event`] to `aperture_db`, it does not open the DB itself.
//!
//! TODO(M2/M6:) embedding lands in M2; the store+embed wiring lands in M6.

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
pub async fn store_and_embed(_record: &UtteranceRecord) -> Result<i64, LoggerError> {
    // TODO(M6:) pipeline (single-writer, doc 03 §1):
    //   1. ev = record.to_event(now_ms);
    //   2. event_id = aperture_db: insert ev into `events`.
    //   3. vec = aperture_embedding::embed(&record.transcript)   // 768-d, doc 03 §5
    //   4. aperture_db: insert (event_id, vec) into `ctx_vec`.
    //   5. return event_id.
    // This MUST run before any intent branch in ptt_up (telemetry is unconditional).
    todo!("M6: insert voice_utterance event + embed transcript into ctx_vec")
}

/// Map the §4 intent to the [`ContextPayload`](aperture_contracts::ContextPayload)
/// intent for an escalation draft (doc 07 §5): a voice query escalated to Claude
/// is [`PayloadIntent::AnswerQuery`]. Helper for the facade's escalation branch.
pub fn escalation_payload_intent() -> PayloadIntent {
    PayloadIntent::AnswerQuery
}
