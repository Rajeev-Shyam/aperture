//! Shared suggestion types — the source-agnostic shape local candidates and
//! cloud results both flatten into (doc 09 §4, doc 15 §5), plus the
//! pattern-engine -> UI handoff types (doc 08, doc 11).

use serde::{Deserialize, Serialize};

/// Emitted by the pattern engine (doc 08 §5) -> the suggestion generator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestionCandidate {
    /// e.g. `"Continue {title} — {position}"`; rendered into a [`BubbleSpec`].
    pub action_template: String,
    pub connector_id: String,
    pub confidence: f64,
    pub pattern_id: i64,
}

/// What the Bubble UI renders (doc 08 §6 -> doc 11 §3). `action_ref` resolves to
/// a `connector_id` on click (Critical Path B, doc 02 §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BubbleSpec {
    pub title: String,
    /// Connector-type glyph.
    pub glyph: String,
    /// e.g. `"12:34 · 2h ago"`.
    pub sublabel: Option<String>,
    pub action_ref: String,
    /// `"local"` or `"claude"` — the only thing the UI treats differently (a tag).
    pub source: SuggestionSource,
    pub confidence: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionSource {
    Local,
    Claude,
}

/// The structured-output contract (doc 09 §4). The cloud is asked to return this;
/// every `reconstruct_payload` is **re-validated by the target connector** before
/// any bubble offers it. Invalid suggestions degrade to `answer_text`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredSuggestions {
    #[serde(default)]
    pub suggestions: Vec<CloudSuggestion>,
    /// Optional prose answer (rendered when there is no actionable suggestion).
    #[serde(default)]
    pub answer_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudSuggestion {
    pub title: String,
    /// `"browser" | "youtube" | "document" | "ide" | "none"`.
    pub connector_type: String,
    #[serde(default)]
    pub reconstruct_payload: serde_json::Value,
    pub rationale: String,
}
