//! Suggestion generator (doc 08 §6 -> doc 11 §3): the ~5 ms step 7 of Critical
//! Path A (doc 02 §4) that turns a pattern-engine [`SuggestionCandidate`] into
//! the [`BubbleSpec`] the Bubble UI renders.
//!
//! It does the formatting only — the pattern engine (doc 08) already decided a
//! bubble is warranted (all of doc 08 §6's triggers held, including a *fresh,
//! resumable* `connector_state`). This crate expands the candidate's
//! `action_template` (e.g. `"Continue {title} — {position}"`) against fields
//! pulled from the [`ConnectorState`]'s versioned `reconstruct_payload`, picks a
//! glyph from the connector type, and tags the source as `local`.
//!
//! Pure, CPU-only, no I/O: a local candidate flattens to the same `BubbleSpec`
//! a Claude-sourced suggestion does, so the UI stays source-agnostic except for
//! the source tag (doc 15 §5, doc 11 §3).

use aperture_contracts::{BubbleSpec, ConnectorState, SuggestionCandidate};
use aperture_contracts::suggestions::SuggestionSource;

/// Render a local [`SuggestionCandidate`] (+ its resolved [`ConnectorState`])
/// into a [`BubbleSpec`] for the Bubble UI (doc 08 §6 -> doc 11 §3).
///
/// - `title`/`sublabel` come from expanding `candidate.action_template`
///   (`{title}`, `{position}`, …) against `state.reconstruct_payload`;
/// - `glyph` is chosen from `state.connector_type` ([`glyph_for`]);
/// - `action_ref` carries `state.id`, which the UI resolves to a `connector_id`
///   on click (Critical Path B, doc 02 §5 / doc 11 §3);
/// - `source` is always [`SuggestionSource::Local`] here (the cloud path renders
///   the identical shape with `Claude`).
///
/// `confidence` passes through from the candidate (doc 08 §5 score).
pub fn render(candidate: &SuggestionCandidate, state: &ConnectorState) -> BubbleSpec {
    // TODO(M3:) expand `candidate.action_template` placeholders ({title},
    // {position}, …) from `state.reconstruct_payload` (per-connector v1 schema,
    // doc 10); compute the sublabel "12:34 · 2h ago" from the captured position +
    // `state.captured_ts` (doc 11 §3); fall back to a literal title if a
    // placeholder is missing. Keep this ~5 ms / allocation-light (doc 02 §4 step 7).
    let _ = (candidate, state);
    todo!("M3: expand action_template -> title/sublabel; map connector_type -> glyph")
}

/// Map a connector type (`"browser" | "youtube" | "document" | "ide"`, doc 03 §3)
/// to the Bubble's connector-type glyph (doc 11 §3 anatomy). Unknown types get a
/// neutral fallback so v2 connectors (doc 10 / doc 15 §3) still render.
pub fn glyph_for(connector_type: &str) -> &'static str {
    // TODO(M3:) finalize the glyph set against the design tokens (doc 14);
    // these are placeholders pending the icon system. [VERIFY]
    match connector_type {
        "youtube" => "video",
        "browser" => "globe",
        "document" => "doc",
        "ide" => "code",
        _ => "spark",
    }
}

/// The source tag this crate stamps on every spec it produces. The UI treats it
/// as just a tag (doc 11 §3); cloud results flatten to the same `BubbleSpec`
/// with [`SuggestionSource::Claude`] (doc 15 §5).
pub const SOURCE: SuggestionSource = SuggestionSource::Local;
