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

use aperture_contracts::suggestions::SuggestionSource;
use aperture_contracts::{BubbleSpec, ConnectorState, SuggestionCandidate};

/// Render a local [`SuggestionCandidate`] (+ its resolved [`ConnectorState`])
/// into a [`BubbleSpec`] for the Bubble UI (doc 08 §6 -> doc 11 §3).
///
/// - `title` comes from expanding `candidate.action_template` (`{title}`,
///   `{position}`, `{line}`, …) against `state.reconstruct_payload`;
/// - `sublabel` is `"12:34 · 2h ago"`-style: position (when present) + captured
///   age (doc 11 §3);
/// - `glyph` is chosen from `state.connector_type` ([`glyph_for`]);
/// - `action_ref` carries `state.id`, which the UI resolves to a `connector_id`
///   on click (Critical Path B, doc 02 §5 / doc 11 §3);
/// - `source` is always [`SuggestionSource::Local`] here (the cloud path renders
///   the identical shape with `Claude`).
///
/// `now_ms` feeds the "·2h ago" age; `confidence` passes through (doc 08 §5).
pub fn render(candidate: &SuggestionCandidate, state: &ConnectorState, now_ms: i64) -> BubbleSpec {
    let payload = &state.reconstruct_payload;
    let title = expand_template(&candidate.action_template, payload);

    let position = payload
        .get("position_s")
        .and_then(|v| v.as_i64())
        .map(fmt_position);
    let age = fmt_age(now_ms.saturating_sub(state.captured_ts));
    let sublabel = match position {
        Some(p) => Some(format!("{p} · {age}")),
        None => Some(age),
    };

    BubbleSpec {
        title,
        glyph: glyph_for(&state.connector_type).to_string(),
        sublabel,
        action_ref: state.id.clone(),
        source: SOURCE,
        confidence: candidate.confidence,
    }
}

/// Expand `{placeholder}`s from the connector's versioned `reconstruct_payload`
/// (doc 10 per-type schemas). A missing placeholder falls back to a sensible
/// literal so the bubble never renders raw braces (graceful degrade, doc 10 §6).
fn expand_template(template: &str, payload: &serde_json::Value) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                let key = &after[..end];
                out.push_str(&resolve_placeholder(key, payload));
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    // Collapse leftover separator noise from empty expansions (" — " with no
    // position) and trim.
    out.trim().trim_end_matches('—').trim().trim_end_matches(':').to_string()
}

/// Resolve one placeholder against the payload (doc 10 v1 schemas):
/// `{title}` → title | file name | video id | url host; `{position}` → `mm:ss`;
/// `{line}` → line number.
fn resolve_placeholder(key: &str, payload: &serde_json::Value) -> String {
    let str_field = |k: &str| payload.get(k).and_then(|v| v.as_str()).map(str::to_string);
    match key {
        "title" => str_field("title")
            .or_else(|| str_field("path").map(|p| file_name(&p)))
            .or_else(|| str_field("url").map(|u| short_url(&u)))
            .or_else(|| str_field("video_id"))
            .unwrap_or_else(|| "where you left off".to_string()),
        "position" => payload
            .get("position_s")
            .and_then(|v| v.as_i64())
            .map(fmt_position)
            .unwrap_or_default(), // honest degrade: no position → "from the start" copy is the connector's job (doc 10 §3)
        "line" => payload
            .get("line")
            .and_then(|v| v.as_i64())
            .map(|l| l.to_string())
            .unwrap_or_default(),
        other => str_field(other).unwrap_or_default(),
    }
}

/// `754 → "12:34"`, `3754 → "1:02:34"`.
fn fmt_position(secs: i64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Rough age string for the sublabel (doc 11 §3): "just now", "5m ago", "2h ago",
/// "3d ago".
fn fmt_age(delta_ms: i64) -> String {
    let mins = delta_ms / 60_000;
    match mins {
        m if m < 1 => "just now".to_string(),
        m if m < 60 => format!("{m}m ago"),
        m if m < 24 * 60 => format!("{}h ago", m / 60),
        m => format!("{}d ago", m / (24 * 60)),
    }
}

/// Last path component, extension kept (a human-recognizable document name).
fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

/// Host part of a URL for title fallback.
fn short_url(url: &str) -> String {
    let after = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    after.split(['/', '?']).next().unwrap_or(after).to_string()
}

/// Map a connector type (`"browser" | "youtube" | "document" | "ide"`, doc 03 §3)
/// to the Bubble's connector-type glyph (doc 11 §3 anatomy). Unknown types get a
/// neutral fallback so v2 connectors (doc 10 / doc 15 §3) still render.
pub fn glyph_for(connector_type: &str) -> &'static str {
    // TODO(M8): finalize the glyph set against the design tokens (doc 14). [VERIFY]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn yt_state() -> ConnectorState {
        ConnectorState {
            id: "conn-1".into(),
            connector_type: "youtube".into(),
            reconstruct_payload: serde_json::json!({
                "video_id": "abc123",
                "title": "Rust lifetimes explained",
                "position_s": 754
            }),
            payload_version: 1,
            captured_ts: 0,
            stale_after_ts: None,
        }
    }

    fn candidate(template: &str) -> SuggestionCandidate {
        SuggestionCandidate {
            action_template: template.into(),
            connector_id: "conn-1".into(),
            confidence: 0.83,
            pattern_id: 7,
        }
    }

    #[test]
    fn youtube_bubble_renders_the_us1_shape() {
        let spec = render(
            &candidate("Continue {title} — {position}"),
            &yt_state(),
            2 * 3_600_000, // 2h after capture
        );
        assert_eq!(spec.title, "Continue Rust lifetimes explained — 12:34");
        assert_eq!(spec.glyph, "video");
        assert_eq!(spec.sublabel.as_deref(), Some("12:34 · 2h ago"));
        assert_eq!(spec.action_ref, "conn-1");
        assert_eq!(spec.source, SuggestionSource::Local);
        assert!((spec.confidence - 0.83).abs() < 1e-9);
    }

    #[test]
    fn missing_position_degrades_honestly() {
        let mut st = yt_state();
        st.reconstruct_payload = serde_json::json!({"title": "A video"});
        let spec = render(&candidate("Continue {title} — {position}"), &st, 60_000);
        assert_eq!(spec.title, "Continue A video", "no dangling separator");
        assert_eq!(spec.sublabel.as_deref(), Some("1m ago"));
    }

    #[test]
    fn document_title_falls_back_to_file_name() {
        let st = ConnectorState {
            id: "c2".into(),
            connector_type: "document".into(),
            reconstruct_payload: serde_json::json!({"path": r"C:\U\x\budget.xlsx"}),
            payload_version: 1,
            captured_ts: 0,
            stale_after_ts: None,
        };
        let spec = render(&candidate("Reopen {title}"), &st, 0);
        assert_eq!(spec.title, "Reopen budget.xlsx");
        assert_eq!(spec.glyph, "doc");
        assert_eq!(spec.sublabel.as_deref(), Some("just now"));
    }

    #[test]
    fn unknown_connector_type_gets_neutral_glyph() {
        assert_eq!(glyph_for("slack-thread"), "spark", "v2 seam renders (doc 15 §3)");
    }
}
