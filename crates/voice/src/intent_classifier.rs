//! Deterministic intent classification (doc 07 §4).
//!
//! Local, explainable, **no model required in v1**. Given a transcript and the
//! STT confidence, decide whether the utterance is a *query* (search history),
//! an *escalation* (`"ask claude …"`), or *telemetry-only* (stored, no UI):
//!
//! 1. Leading-verb match against the command lexicon
//!    (`open, reopen, continue, find, show, resume, search, ask`) **or** an
//!    interrogative (`what, where, when, which`) ⇒ [`Intent::Query`].
//! 2. `"ask claude …"` prefix ⇒ [`Intent::Escalation`] — still goes through the
//!    transparency gate; **never auto-sends** (doc 07 §4.2, doc 13 §2).
//! 3. Otherwise ⇒ [`Intent::Telemetry`] (stored + embedded, no UI).
//! 4. STT confidence `< `[`CONFIRM_CONFIDENCE_FLOOR`]` ⇒ the subsystem shows a
//!    transcript chip ("Did you say: …?") with *Run* / *Dismiss* and never acts on
//!    a guess (doc 07 §4.4). The floor check lives in the facade so classification
//!    itself stays a pure function of the words.
//!
//! [`classify`] is a **pure function** — no I/O, no GPU, no clock — so it is
//! exhaustively unit-testable (doc 07 §4).

/// Confidence below which the subsystem must confirm before acting (doc 07 §4.4)
/// [ASSUMPTION].
pub const CONFIRM_CONFIDENCE_FLOOR: f32 = 0.6;

/// Leading verbs that mark a history query (doc 07 §4.1). Note `ask` is here for a
/// bare "ask …" query; the `"ask claude"` *prefix* is matched first as escalation.
pub const COMMAND_VERBS: &[&str] = &[
    "open", "reopen", "continue", "find", "show", "resume", "search", "ask",
];

/// Interrogatives that mark a history query (doc 07 §4.1).
pub const INTERROGATIVES: &[&str] = &["what", "where", "when", "which"];

/// The escalation prefix that routes through the transparency gate (doc 07 §4.2).
pub const ESCALATION_PREFIX: &str = "ask claude";

/// The classified role of an utterance (doc 07 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Search the local history and surface an answer bubble (doc 07 §5).
    Query,
    /// `"ask claude …"` — assemble a payload for the preview→Send gate; the words
    /// after the prefix become the `user_addition` (doc 07 §5). Never auto-sent.
    Escalation,
    /// Stored + embedded only; no UI (doc 07 §4.3).
    Telemetry,
}

/// A classification with its confidence, so the facade can apply the §4.4 floor.
#[derive(Debug, Clone)]
pub struct IntentResult {
    pub intent: Intent,
    /// STT confidence carried through unchanged; the facade compares it to
    /// [`CONFIRM_CONFIDENCE_FLOOR`] to decide on the confirm chip.
    pub confidence: f32,
    /// For [`Intent::Escalation`]: the text after the `"ask claude"` prefix, i.e.
    /// the user's actual question. `None` for other intents.
    pub escalation_query: Option<String>,
}

impl IntentResult {
    /// `true` when the subsystem must show the "Did you say: …?" confirm chip
    /// instead of acting (doc 07 §4.4).
    pub fn needs_confirmation(&self) -> bool {
        self.confidence < CONFIRM_CONFIDENCE_FLOOR
    }
}

/// Classify a transcript (doc 07 §4). **Pure**: depends only on its inputs.
pub fn classify(transcript: &str, confidence: f32) -> IntentResult {
    let normalized = transcript.trim().to_lowercase();

    // (2) Escalation prefix wins over the generic verb match (doc 07 §4.2). Require
    // a word boundary after the prefix so "ask claudes plan" is NOT an escalation —
    // only a bare "ask claude" or "ask claude <ws>…" is.
    if let Some(rest) = normalized.strip_prefix(ESCALATION_PREFIX) {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let query = rest.trim();
            return IntentResult {
                intent: Intent::Escalation,
                confidence,
                escalation_query: (!query.is_empty()).then(|| query.to_string()),
            };
        }
        // else: falls through to the verb/interrogative match below.
    }

    // (1) Leading-verb / interrogative match ⇒ query (doc 07 §4.1).
    let first_word = normalized.split_whitespace().next().unwrap_or("");
    let is_query =
        COMMAND_VERBS.contains(&first_word) || INTERROGATIVES.contains(&first_word);

    IntentResult {
        intent: if is_query {
            Intent::Query
        } else {
            // (3) Otherwise telemetry-only (doc 07 §4.3).
            Intent::Telemetry
        },
        confidence,
        escalation_query: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_verb_is_query() {
        assert_eq!(classify("find that pdf I had open", 0.9).intent, Intent::Query);
        assert_eq!(classify("Resume the React tutorial", 0.9).intent, Intent::Query);
    }

    #[test]
    fn interrogative_is_query() {
        assert_eq!(classify("what was that error", 0.9).intent, Intent::Query);
    }

    #[test]
    fn ask_claude_prefix_is_escalation() {
        let r = classify("ask claude to summarize this thread", 0.9);
        assert_eq!(r.intent, Intent::Escalation);
        assert_eq!(r.escalation_query.as_deref(), Some("to summarize this thread"));
    }

    #[test]
    fn bare_ask_claude_is_escalation_with_no_query() {
        let r = classify("ask claude", 0.9);
        assert_eq!(r.intent, Intent::Escalation);
        assert_eq!(r.escalation_query, None);
    }

    #[test]
    fn ask_claude_requires_a_word_boundary() {
        // "ask claudes plan" must NOT strip to escalation "s plan"; it falls through
        // to the leading-verb match ("ask" is a command verb) → Query.
        let r = classify("ask claudes plan for the week", 0.9);
        assert_eq!(r.intent, Intent::Query);
        assert_eq!(r.escalation_query, None);
    }

    #[test]
    fn plain_statement_is_telemetry() {
        assert_eq!(classify("note to self buy milk", 0.9).intent, Intent::Telemetry);
    }

    #[test]
    fn low_confidence_needs_confirmation() {
        assert!(classify("find the doc", 0.5).needs_confirmation());
        assert!(!classify("find the doc", 0.8).needs_confirmation());
    }
}
