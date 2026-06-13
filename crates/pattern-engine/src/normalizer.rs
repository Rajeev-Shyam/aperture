//! Event normalization → tokens (doc 08 §2).
//!
//! Each [`Event`] collapses to a [`Token`] `(app_class, action, resource_class)`.
//! The alias table folds related processes into one class so habits generalize
//! across e.g. Chrome/Edge/Firefox. Example (doc 08 §2): opening a tutorial
//! video ⇒ `(browser, navigation, youtube)`.

use aperture_contracts::event::{Event, EventType};

/// The normalized unit mined by the n-gram and temporal stages (doc 08 §2-§4).
///
/// All three fields are coarse, low-cardinality classes — never raw titles or
/// URLs — so the pattern table stays small and privacy-preserving.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Token {
    /// Process folded through [`app_class`] (e.g. `"browser"`, `"ide"`, `"office"`).
    pub app_class: String,
    /// The event type as a stable action string (doc 08 §2: focus/open/navigation/…).
    pub action: String,
    /// Coarse resource class from the connector type, or `None` (`∅`, doc 08 §2).
    pub resource_class: Option<String>,
}

/// Map a raw process name to its coarse `app_class` via the alias table (doc 08 §2).
///
/// `chrome`/`edge`/`firefox` → `browser`; `code` → `ide`; `excel`/`winword` →
/// `office`; otherwise the (lowercased, `.exe`-stripped) process name itself.
pub fn app_class(process: &str) -> String {
    // TODO(M3): load the alias table from settings so users can add their own
    // process→class folds (doc 08 §9 tunables). Hard-coded seed table for now.
    let p = process.trim().to_ascii_lowercase();
    let p = p.strip_suffix(".exe").unwrap_or(&p);
    match p {
        "chrome" | "msedge" | "edge" | "firefox" => "browser",
        "code" | "code - insiders" => "ide",
        "excel" | "winword" => "office",
        other => return other.to_string(),
    }
    .to_string()
}

/// The stable action string for an [`EventType`] (doc 08 §2).
pub fn action_of(ty: EventType) -> &'static str {
    // TODO(M3): confirm coverage as the event taxonomy grows (doc 03 §2 is additive).
    match ty {
        EventType::WindowFocus => "focus",
        EventType::WindowOpen => "open",
        EventType::WindowClose => "close",
        EventType::Navigation => "navigation",
        EventType::MediaState => "media",
        EventType::DocumentState => "document",
        EventType::IdeState => "ide",
        EventType::VoiceUtterance => "voice",
        EventType::SuggestionShown => "suggestion_shown",
        EventType::SuggestionClicked => "suggestion_clicked",
        EventType::SuggestionDismissed => "suggestion_dismissed",
        EventType::CaptureToggle => "capture_toggle",
        EventType::CloudSend => "cloud_send",
    }
}

/// Derive the coarse `resource_class` from an event's `connector_id` /
/// connector-typed payload (doc 08 §2): `youtube`, `doc:xlsx`, `ide:rs`,
/// `url:domain`, else `None` (`∅`).
pub fn resource_class(_ev: &Event) -> Option<String> {
    // TODO(M3): derive from connector_id + payload (doc 08 §2 examples):
    //   - youtube           from the youtube connector
    //   - doc:<ext>         from the document connector's file extension
    //   - ide:<lang>        from the ide connector's language
    //   - url:<domain>      from the browser connector's host (eTLD+1)
    // When OCR could not classify the consequent this stays None (∅) and may
    // trigger an optional local VLM assist (doc 08 §8) — never a cloud call.
    todo!("M3: map connector_id/payload → coarse resource_class (doc 08 §2)")
}

/// Normalize one [`Event`] into a [`Token`] (doc 08 §2).
///
/// Returns `None` for events that carry no usable `app_class` (e.g. no process),
/// or that are non-behavioral audit/suggestion rows the miner should ignore.
pub fn normalize(_ev: &Event) -> Option<Token> {
    // TODO(M3): build (app_class, action, resource_class); skip audit/feedback
    // event types (CaptureToggle/CloudSend/Suggestion*) so they don't pollute
    // n-grams. EXCLUDED / PRIVATE_WINDOW events must never form tokens (doc 13 §4).
    todo!("M3: Event → Token via app_class()/action_of()/resource_class() (doc 08 §2)")
}
