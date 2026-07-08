//! Event normalization → tokens (doc 08 §2).
//!
//! Each [`Event`] collapses to a [`Token`] `(app_class, action, resource_class)`.
//! The alias table folds related processes into one class so habits generalize
//! across e.g. Chrome/Edge/Firefox. Example (doc 08 §2): opening a tutorial
//! video ⇒ `(browser, navigation, youtube)`.

use aperture_contracts::event::{redaction_flags, Event, EventType};

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

impl Token {
    /// Canonical, collision-free encoding used inside signatures (doc 08 §4).
    /// Fields are joined with `:`; a `:` inside a class is folded to `_` (classes
    /// are coarse identifiers, never raw user text, so this is lossless enough
    /// and keeps the encoding stable for feedback lookups, doc 08 §7).
    pub fn encode(&self) -> String {
        let clean = |s: &str| s.replace([':', '⇒'], "_");
        match &self.resource_class {
            Some(r) => format!(
                "{}:{}:{}",
                clean(&self.app_class),
                clean(&self.action),
                clean(r)
            ),
            None => format!("{}:{}:∅", clean(&self.app_class), clean(&self.action)),
        }
    }
}

/// Map a raw process name to its coarse `app_class` via the alias table (doc 08 §2).
///
/// `chrome`/`edge`/`firefox` → `browser`; `code` → `ide`; `excel`/`winword` →
/// `office`; otherwise the (lowercased, `.exe`-stripped) process name itself.
pub fn app_class(process: &str) -> String {
    // TODO(M3+): load the alias table from settings so users can add their own
    // process→class folds (doc 08 §9 tunables). Hard-coded seed table for now.
    let p = process.trim().to_ascii_lowercase();
    let p = p.strip_suffix(".exe").unwrap_or(&p);
    match p {
        "chrome" | "msedge" | "edge" | "firefox" | "brave" | "opera" | "opera_gx" => "browser",
        "code" | "code - insiders" | "rustrover64" | "idea64" | "pycharm64" | "devenv" => "ide",
        "excel" | "winword" | "powerpnt" | "onenote" => "office",
        "windowsterminal" | "wt" | "cmd" | "powershell" | "pwsh" => "terminal",
        "explorer" => "shell",
        other => return other.to_string(),
    }
    .to_string()
}

/// The stable action string for an [`EventType`] (doc 08 §2).
pub fn action_of(ty: EventType) -> &'static str {
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

/// Event types the miner ignores entirely (doc 08 §2): audit + feedback rows are
/// consumed by the feedback loop, not mined as behavior.
fn is_behavioral(ty: EventType) -> bool {
    !matches!(
        ty,
        EventType::SuggestionShown
            | EventType::SuggestionClicked
            | EventType::SuggestionDismissed
            | EventType::CaptureToggle
            | EventType::CloudSend
            | EventType::VoiceUtterance // telemetry role; queried, not mined (doc 07)
    )
}

/// Derive the coarse `resource_class` from an event's connector-typed payload
/// (doc 08 §2): `youtube`, `doc:<ext>`, `ide:<ext>`, `url:<host>`, else `None` (`∅`).
///
/// Only coarse classes — never full URLs/paths/titles — enter tokens.
pub fn resource_class(ev: &Event) -> Option<String> {
    let payload = &ev.payload;
    match ev.r#type {
        EventType::MediaState => {
            // youtube connector heuristics (doc 03 §2): { url, video_id, … }.
            if payload.get("video_id").is_some() {
                Some("youtube".to_string())
            } else {
                payload
                    .get("url")
                    .and_then(|u| u.as_str())
                    .and_then(host_of)
                    .map(|h| format!("url:{h}"))
            }
        }
        EventType::Navigation => payload
            .get("url")
            .and_then(|u| u.as_str())
            .and_then(|u| {
                let h = host_of(u)?;
                if h.contains("youtube.") || h == "youtu.be" {
                    Some("youtube".to_string())
                } else {
                    Some(format!("url:{h}"))
                }
            }),
        EventType::DocumentState => payload
            .get("path")
            .and_then(|p| p.as_str())
            .map(|p| match ext_of(p) {
                Some(ext) => format!("doc:{ext}"),
                None => "doc".to_string(),
            }),
        EventType::IdeState => payload
            .get("path")
            .and_then(|p| p.as_str())
            .map(|p| match ext_of(p) {
                Some(ext) => format!("ide:{ext}"),
                None => "ide".to_string(),
            }),
        // When OCR could not classify the consequent this stays None (∅) and may
        // trigger an optional local VLM assist (doc 08 §8) — never a cloud call.
        _ => None,
    }
}

/// Registrable-ish host of a URL, lowercased, `www.`-stripped. Coarse on purpose
/// (privacy: tokens carry hosts, never full URLs — doc 08 §2). Public so the
/// connector lookup (trigger rule 3) can reverse a `url:<host>` consequent against
/// a stored browser state's URL through the exact same encoding (CONN-H1).
pub fn host_of(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()?
        .split('@')
        .next_back()? // strip userinfo
        .split(':')
        .next()?; // strip port
    if host.is_empty() {
        return None;
    }
    let host = host.to_ascii_lowercase();
    Some(host.strip_prefix("www.").unwrap_or(&host).to_string())
}

/// Lowercased file extension of a path, if any.
fn ext_of(path: &str) -> Option<String> {
    let name = path.rsplit(['\\', '/']).next()?;
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() || ext.len() > 8 {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

/// Normalize one [`Event`] into a [`Token`] (doc 08 §2).
///
/// Returns `None` for events that carry no usable `app_class` (e.g. no process),
/// for non-behavioral audit/suggestion rows the miner ignores, and for
/// `EXCLUDED` / `PRIVATE_WINDOW` events — excluded contexts must never form
/// tokens (doc 13 §4).
pub fn normalize(ev: &Event) -> Option<Token> {
    if !is_behavioral(ev.r#type) {
        return None;
    }
    if ev.redaction_flags & (redaction_flags::EXCLUDED | redaction_flags::PRIVATE_WINDOW) != 0 {
        return None; // doc 13 §4: excluded contexts never enter the miner
    }
    let process = ev.process.as_deref()?;
    if process.trim().is_empty() {
        return None;
    }
    Some(Token {
        app_class: app_class(process),
        action: action_of(ev.r#type).to_string(),
        resource_class: resource_class(ev),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ty: EventType, process: &str, payload: serde_json::Value) -> Event {
        Event {
            id: 0,
            ts: 0,
            r#type: ty,
            app: None,
            process: Some(process.into()),
            window_title: None,
            payload,
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        }
    }

    #[test]
    fn browser_youtube_navigation_tokenizes_as_doc_08_example() {
        let e = ev(
            EventType::Navigation,
            "chrome.exe",
            serde_json::json!({"url": "https://www.youtube.com/watch?v=abc", "browser": "chrome"}),
        );
        let t = normalize(&e).expect("token");
        assert_eq!(t.app_class, "browser");
        assert_eq!(t.action, "navigation");
        assert_eq!(t.resource_class.as_deref(), Some("youtube"));
    }

    #[test]
    fn document_state_yields_doc_ext_class() {
        let e = ev(
            EventType::DocumentState,
            "EXCEL.EXE",
            serde_json::json!({"path": r"C:\Users\x\budget.xlsx", "app": "excel"}),
        );
        let t = normalize(&e).expect("token");
        assert_eq!(t.app_class, "office");
        assert_eq!(t.resource_class.as_deref(), Some("doc:xlsx"));
    }

    #[test]
    fn excluded_and_audit_events_never_tokenize() {
        let mut e = ev(EventType::WindowFocus, "1password.exe", serde_json::json!({}));
        e.redaction_flags = redaction_flags::EXCLUDED;
        assert!(normalize(&e).is_none(), "EXCLUDED never mined (doc 13 §4)");

        let audit = ev(EventType::CloudSend, "aperture.exe", serde_json::json!({}));
        assert!(normalize(&audit).is_none(), "audit rows never mined");
    }

    #[test]
    fn plain_urls_reduce_to_host_only() {
        let e = ev(
            EventType::Navigation,
            "firefox.exe",
            serde_json::json!({"url": "https://user@docs.rs:443/tokio/latest?x=1#frag"}),
        );
        let t = normalize(&e).expect("token");
        assert_eq!(t.resource_class.as_deref(), Some("url:docs.rs"));
    }

    #[test]
    fn token_encoding_is_stable_and_separator_safe() {
        let t = Token {
            app_class: "we:ird".into(),
            action: "focus".into(),
            resource_class: None,
        };
        assert_eq!(t.encode(), "we_ird:focus:∅");
    }
}
