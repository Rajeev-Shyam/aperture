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
    /// When this suggestion was created, epoch ms — the freshness half of the
    /// UI's slot-admission score (owner decision #5, 2026-08-16). A queued
    /// stale-but-confident bubble must not outrank a fresh one forever, and the
    /// overlay cannot know a restored row's age without this. `None` (an older
    /// row, or a producer that has not set it) reads as "unknown age" and the UI
    /// scores it on confidence alone — the pre-decision behavior.
    #[serde(default)]
    pub created_ts: Option<i64>,
    /// Ready-to-apply "stop capturing this" rules for the bubble's ⋯ menu (owner
    /// decision #8, 2026-08-16). Derived core-side from the subject's
    /// `connector_state` so the pattern escaping + match-kind choice stay next
    /// to [`aperture_capture::exclusion`]'s matcher rather than being rebuilt in
    /// the WebView. Empty = nothing honestly derivable; the menu then offers
    /// only the exclusion manager.
    #[serde(default)]
    pub exclusion_offers: Vec<ExclusionOffer>,
}

/// One exclusion rule a bubble can apply in a click (owner decision #8): the
/// `(match_kind, pattern)` pair [`aperture_capture::exclusion::validate_pattern`]
/// accepts, plus the human label the menu item reads with ("Stop capturing
/// docs.rs"). The UI never composes patterns itself — a hand-built regex that
/// fails to compile becomes a rule the panel shows as active protection while it
/// silently matches nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExclusionOffer {
    /// What the menu item names, e.g. `"docs.rs"` or `"Slack"`.
    pub label: String,
    /// `"process" | "window_class" | "title_regex" | "url_pattern"` (doc 13 §4).
    pub match_kind: String,
    /// The literal (process/class) or regex (title/url) the matcher compiles.
    pub pattern: String,
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
