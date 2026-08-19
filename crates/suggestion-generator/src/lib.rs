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
use aperture_contracts::{BubbleSpec, ConnectorState, ExclusionOffer, SuggestionCandidate};

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
    let title = expand_template(&candidate.action_template, payload, &state.connector_type);

    let position = payload
        .get("position_s")
        .and_then(|v| v.as_i64())
        .map(fmt_position);
    let age = fmt_age(now_ms.saturating_sub(state.captured_ts));
    let sublabel = match position {
        Some(p) => Some(format!("{p} · {age}")),
        // US1 acceptance d (doc 10 §3): an unknown video position is stated,
        // not hidden — the honest degrade is part of the copy.
        None if state.connector_type == "youtube" => Some(format!("from the start · {age}")),
        None => Some(age),
    };

    BubbleSpec {
        title,
        glyph: glyph_for(&state.connector_type).to_string(),
        sublabel,
        action_ref: state.id.clone(),
        source: SOURCE,
        confidence: candidate.confidence,
        // Freshness input for the overlay's slot-admission score (decision #5):
        // the moment the suggestion was made, not the moment its context was
        // captured (`state.captured_ts` is already the sublabel's "2h ago").
        created_ts: Some(now_ms),
        // Decision #8: the ⋯ menu's real "stop capturing this" action.
        exclusion_offers: exclusion_offers_for(state),
    }
}

/// The `(match_kind, pattern)` rules a bubble's ⋯ menu can apply in one click
/// (owner decision #8, 2026-08-16) for the context this bubble is *about*.
///
/// Derived here, core-side, rather than in the WebView for two reasons: the
/// regex escaping must match what
/// `aperture_capture::exclusion::validate_pattern` will accept (a pattern that
/// fails to compile becomes a rule the Privacy panel shows as active protection
/// while it silently matches nothing), and the choice of *which* rule is
/// right-sized is a product decision, not a rendering one:
///
/// - **browser / youtube** → the site (`url_pattern`). Excluding the whole
///   browser process from one page's bubble is far more than the user asked
///   for; "stop capturing docs.rs" is the honest reading. The whole-browser
///   rule is still one click away in the exclusion manager.
/// - **document / app_focus** → the owning app (`process`). Here the app IS the
///   subject ("you keep switching to Slack"), so the process rule is the
///   right-sized one.
/// - **ide** → nothing. The stored payload (`path`/`line`/`workspace`) names no
///   process, and guessing one would produce a rule that protects nothing.
///
/// An empty result is honest: the menu then offers only the exclusion manager.
pub fn exclusion_offers_for(state: &ConnectorState) -> Vec<ExclusionOffer> {
    let payload = &state.reconstruct_payload;
    let field = |k: &str| {
        payload
            .get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };

    match state.connector_type.as_str() {
        "browser" | "youtube" => match field("url").and_then(host_of) {
            Some(host) => vec![ExclusionOffer {
                label: host.clone(),
                match_kind: "url_pattern".to_string(),
                pattern: host_url_pattern(&host),
            }],
            None => Vec::new(),
        },
        "document" | "app_focus" => {
            // `app_hint` (document) / `process` (app_focus) hold the image name;
            // `app` is app_focus's display form ("Slack") for the menu label.
            let Some(process) = field("app_hint").or_else(|| field("process")) else {
                return Vec::new();
            };
            // Process matching is an exact (case-insensitive) match on the full
            // image name: a bare brand string like "chrome" would never fire, so
            // offering it would be a rule that quietly protects nothing.
            if !process.to_ascii_lowercase().ends_with(".exe") {
                return Vec::new();
            }
            let label = field("app").unwrap_or(process).to_string();
            vec![ExclusionOffer {
                label,
                match_kind: "process".to_string(),
                pattern: process.to_ascii_lowercase(),
            }]
        }
        _ => Vec::new(),
    }
}

/// Registrable host of a URL, lowercased, `www.` dropped (the pattern below
/// re-admits it as a subdomain). `None` for anything without a usable host.
fn host_of(url: &str) -> Option<String> {
    let after = url.split_once("://").map(|(_, r)| r)?;
    let host = after.split(['/', '?', '#']).next()?;
    // Drop userinfo and port.
    let host = host.rsplit('@').next()?;
    let host = host.split(':').next()?.trim_end_matches('.');
    if host.is_empty() || !host.contains('.') {
        return None;
    }
    let host = host.to_ascii_lowercase();
    Some(host.strip_prefix("www.").unwrap_or(&host).to_string())
}

/// A `url_pattern` regex for one host, in the same shape as the shipped
/// defaults (`crates/capture/src/exclusion.rs`): scheme-anchored, one optional
/// subdomain label, terminated so `docs.rs` cannot match `docs.rs.evil.com`.
/// Compiled case-insensitively by the matcher, so no `(?i)` is needed.
fn host_url_pattern(host: &str) -> String {
    format!(r"^https?://([a-z0-9-]+\.)?{}([:/?#]|$)", regex_escape(host))
}

/// Escape regex metacharacters — the `regex` crate's `escape`, inlined so this
/// pure formatting crate keeps no runtime regex dependency. The dev-dependency
/// test below compiles every pattern this produces, which is what actually
/// proves the escaping right.
fn regex_escape(s: &str) -> String {
    const META: &str = r"\.+*?()|[]{}^$#&-~";
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if META.contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Expand `{placeholder}`s from the connector's versioned `reconstruct_payload`
/// (doc 10 per-type schemas). A missing placeholder falls back to a sensible
/// literal so the bubble never renders raw braces (graceful degrade, doc 10 §6).
fn expand_template(template: &str, payload: &serde_json::Value, connector_type: &str) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                let key = &after[..end];
                out.push_str(&resolve_placeholder(key, payload, connector_type));
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
fn resolve_placeholder(key: &str, payload: &serde_json::Value, connector_type: &str) -> String {
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
            // US1 acceptance d (doc 10 §3): a video with no captured position
            // says so honestly — "from the start", never a fabricated number.
            .unwrap_or_else(|| {
                if connector_type == "youtube" {
                    "from the start".to_string()
                } else {
                    String::new()
                }
            }),
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
        // Decision #15's "Switch to X" bubbles: their own semantic token so the
        // overlay does not render them with the generic fallback mark.
        "app_focus" => "switch",
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
    fn missing_position_says_from_the_start() {
        // US1 acceptance d (doc 10 §3): the degrade is stated, not hidden.
        let mut st = yt_state();
        st.reconstruct_payload = serde_json::json!({"title": "A video"});
        let spec = render(&candidate("Continue {title} — {position}"), &st, 60_000);
        assert_eq!(spec.title, "Continue A video — from the start");
        assert_eq!(spec.sublabel.as_deref(), Some("from the start · 1m ago"));
    }

    #[test]
    fn missing_position_on_non_youtube_drops_the_separator() {
        let st = ConnectorState {
            id: "c3".into(),
            connector_type: "browser".into(),
            reconstruct_payload: serde_json::json!({"title": "A page", "url": "https://x.example/p"}),
            payload_version: 1,
            captured_ts: 0,
            stale_after_ts: None,
        };
        let spec = render(&candidate("Return to {title} — {position}"), &st, 60_000);
        assert_eq!(spec.title, "Return to A page", "no dangling separator");
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

    // --- decision #8: the bubble's one-click "stop capturing this" ------------

    fn state_of(connector_type: &str, payload: serde_json::Value) -> ConnectorState {
        ConnectorState {
            id: "c".into(),
            connector_type: connector_type.into(),
            reconstruct_payload: payload,
            payload_version: 1,
            captured_ts: 0,
            stale_after_ts: None,
        }
    }

    /// Compile with the SAME configuration `exclusion::validate_pattern` uses.
    fn compiled(pattern: &str) -> regex::Regex {
        regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .expect("every emitted pattern must compile")
    }

    #[test]
    fn browser_offers_the_site_not_the_whole_browser() {
        let st = state_of(
            "browser",
            serde_json::json!({"url": "https://docs.rs/tokio/latest", "browser": "chrome.exe"}),
        );
        let offers = exclusion_offers_for(&st);
        assert_eq!(offers.len(), 1, "one right-sized rule, not the browser process");
        assert_eq!(offers[0].match_kind, "url_pattern");
        assert_eq!(offers[0].label, "docs.rs");

        let re = compiled(&offers[0].pattern);
        assert!(re.is_match("https://docs.rs/tokio/latest"));
        assert!(re.is_match("https://DOCS.RS/"), "matcher is case-insensitive");
        assert!(re.is_match("http://www.docs.rs/x"), "www is a subdomain, not a different site");
        assert!(!re.is_match("https://docs.rs.evil.com/"), "host is terminated");
        assert!(!re.is_match("https://notdocs.rs/"), "subdomain label needs its dot");
        assert!(!re.is_match("https://example.com/?q=docs.rs"), "not just a substring");
    }

    #[test]
    fn youtube_offers_the_site_from_its_watch_url() {
        let st = state_of(
            "youtube",
            serde_json::json!({"url": "https://www.youtube.com/watch?v=abc", "video_id": "abc"}),
        );
        let offers = exclusion_offers_for(&st);
        assert_eq!(offers[0].label, "youtube.com", "www dropped from the label");
        assert!(compiled(&offers[0].pattern).is_match("https://www.youtube.com/watch?v=abc"));
    }

    #[test]
    fn app_focus_offers_the_process_with_its_display_label() {
        let st = state_of("app_focus", serde_json::json!({"process": "Slack.exe", "app": "Slack"}));
        let offers = exclusion_offers_for(&st);
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].match_kind, "process");
        assert_eq!(offers[0].label, "Slack", "the menu reads the display name");
        assert_eq!(offers[0].pattern, "slack.exe", "matcher compares lowercased");
    }

    #[test]
    fn document_offers_its_owning_app() {
        let st = state_of(
            "document",
            serde_json::json!({"path": r"C:\U\budget.xlsx", "app_hint": "EXCEL.EXE"}),
        );
        let offers = exclusion_offers_for(&st);
        assert_eq!(offers[0].pattern, "excel.exe");
        assert_eq!(offers[0].label, "EXCEL.EXE", "no display name stored — say the real thing");
    }

    #[test]
    fn nothing_derivable_offers_nothing_rather_than_a_rule_that_protects_nothing() {
        // A bare brand string can never match the exact-image-name matcher.
        let bare = state_of("document", serde_json::json!({"path": "x", "app_hint": "chrome"}));
        assert!(exclusion_offers_for(&bare).is_empty());
        // IDE payloads name no process; guessing one would be a dead rule.
        let ide = state_of("ide", serde_json::json!({"path": "x", "workspace": "aperture"}));
        assert!(exclusion_offers_for(&ide).is_empty());
        // A URL with no host is not a site.
        let odd = state_of("browser", serde_json::json!({"url": "about:blank"}));
        assert!(exclusion_offers_for(&odd).is_empty());
        // Bare-hostname intranet URLs have no registrable dot — no honest rule.
        let intranet = state_of("browser", serde_json::json!({"url": "http://intranet/home"}));
        assert!(exclusion_offers_for(&intranet).is_empty());
    }

    #[test]
    fn hosts_with_regex_metacharacters_are_escaped_not_interpreted() {
        // Odd hosts must never turn into a wildcard rule.
        let st = state_of("browser", serde_json::json!({"url": "https://a+b.example.com/x"}));
        let offers = exclusion_offers_for(&st);
        let re = compiled(&offers[0].pattern);
        assert!(re.is_match("https://a+b.example.com/x"));
        assert!(!re.is_match("https://ab.example.com/x"), "'+' stayed a literal");
    }

    #[test]
    fn render_stamps_the_creation_time_for_the_admission_score() {
        // Decision #5: without this the overlay cannot tell a just-arrived
        // bubble from one restored hours later out of SQLite.
        let spec = render(&candidate("Continue {title}"), &yt_state(), 1_700_000_000_000);
        assert_eq!(spec.created_ts, Some(1_700_000_000_000));
    }
}
