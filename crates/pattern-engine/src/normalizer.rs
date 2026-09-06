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

    /// Inverse of [`encode`](Self::encode): parse one encoded token back into a
    /// [`Token`]. Fields split on `:`; a `∅` resource decodes to `None`. Returns
    /// `None` for a string that isn't the expected `app:action:resource` shape
    /// (a corrupt persisted signature, skipped by the hydrate rather than
    /// crashing it — CONN-M2).
    ///
    /// `encode` folds the `:` inside prefixed resource classes (`url:<host>`,
    /// `doc:<ext>`, `ide:<ext>`) to `_`, so the round-trip MUST restore it —
    /// without this, every hydrated pattern's consequent came back as e.g.
    /// `url_docs.rs`, which the connector lookup (`url:`-prefix match) rejected
    /// forever: after the first app restart no learned browser/doc/IDE pattern
    /// could ever bubble again (2026-08-15 review — the "zero recommendations"
    /// root cause). Hosts and extensions never legitimately start with these
    /// prefixes, so the restoration is unambiguous; DB-persisted signatures keep
    /// the folded form, which stays byte-stable across this fix.
    pub fn decode(encoded: &str) -> Option<Token> {
        let parts: Vec<&str> = encoded.split(':').collect();
        if parts.len() != 3 {
            return None;
        }
        let resource_class = (parts[2] != "∅").then(|| {
            let r = parts[2];
            for prefix in ["url_", "doc_", "ide_"] {
                if let Some(rest) = r.strip_prefix(prefix) {
                    return format!("{}:{rest}", &prefix[..prefix.len() - 1]);
                }
            }
            r.to_string()
        });
        Some(Token {
            app_class: parts[0].to_string(),
            action: parts[1].to_string(),
            resource_class,
        })
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
        // VS Code and its forks (Antigravity IDE is the owner's daily driver,
        // 2026-09-06; Cursor / Windsurf ship the same shell).
        "code" | "code - insiders" | "antigravity ide" | "antigravity" | "cursor" | "windsurf"
        | "rustrover64" | "idea64" | "pycharm64" | "devenv" => "ide",
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
        EventType::McpSearch => "mcp_search",
    }
}

/// Event types the miner ignores entirely (doc 08 §2): audit + feedback rows are
/// consumed by the feedback loop, not mined as behavior — and `window_close`,
/// which doc 08 §2's action list (focus/open/navigation/media/document/ide)
/// never included. Mining closes was drift with teeth (2026-09-06, the owner's
/// "not a single recommendation"): on the owner's box `window_close` was 60 %
/// of the event stream (shell popups, tooltips, browser tab windows), so the
/// n-gram window was mostly `close` tokens, every `⇒ *` denominator was spread
/// across close-noise consequents, and no sequence of real app switches ever
/// reached the 0.4 confidence floor. Closing a window is not a behaviour the
/// engine can act on either — nothing resumes a closed window.
pub fn is_behavioral(ty: EventType) -> bool {
    !matches!(
        ty,
        EventType::WindowClose
            | EventType::SuggestionShown
            | EventType::SuggestionClicked
            | EventType::SuggestionDismissed
            | EventType::CaptureToggle
            | EventType::CloudSend
            | EventType::McpSearch // audit row (ADR-037), never behavior
            | EventType::VoiceUtterance // telemetry role; queried, not mined (doc 07)
    )
}

/// Whether a (persisted or freshly mined) token could be minted by
/// [`normalize`] today — its action is one of the behavioural event types'.
/// The hydrate uses it to drop rows learned under an older token vocabulary
/// (the `close`-noise table, 2026-09-06) instead of carrying them forever.
pub fn is_minable(token: &Token) -> bool {
    ALL_EVENT_TYPES
        .iter()
        .copied()
        .filter(|ty| is_behavioral(*ty))
        .any(|ty| action_of(ty) == token.action)
}

/// Every taxonomy variant, so [`is_minable`] stays in lock-step with
/// [`is_behavioral`] / [`action_of`] without a second hand-written list.
const ALL_EVENT_TYPES: &[EventType] = &[
    EventType::WindowFocus,
    EventType::WindowOpen,
    EventType::WindowClose,
    EventType::Navigation,
    EventType::MediaState,
    EventType::DocumentState,
    EventType::IdeState,
    EventType::VoiceUtterance,
    EventType::SuggestionShown,
    EventType::SuggestionClicked,
    EventType::SuggestionDismissed,
    EventType::CaptureToggle,
    EventType::CloudSend,
    EventType::McpSearch,
];

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
    // A focus/open with no window title is a transient shell surface — a
    // tooltip, a jump list, `PopupHost`, the bare desktop — not a step the
    // user took (2026-09-06; excluded contexts are title-less too, but they
    // were already dropped by the redaction flag above, so this only ever sees
    // the genuinely nameless). Payload-bearing types (navigation, media,
    // document, ide) identify themselves and need no title.
    if matches!(ev.r#type, EventType::WindowFocus | EventType::WindowOpen)
        && ev.window_title.as_deref().map_or(true, |t| t.trim().is_empty())
    {
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

    /// The hydration round-trip (2026-08-15 review, root cause of "zero
    /// recommendations"): a prefixed resource class must survive
    /// encode → persist → decode byte-identically as a TOKEN, colon restored.
    #[test]
    fn prefixed_resource_classes_survive_the_encode_decode_round_trip() {
        for resource in ["url:docs.rs", "doc:xlsx", "ide:rs", "youtube", "url:my_host"] {
            let t = Token {
                app_class: "browser".into(),
                action: "navigation".into(),
                resource_class: Some(resource.into()),
            };
            let decoded = Token::decode(&t.encode()).expect("decodes");
            assert_eq!(
                decoded.resource_class.as_deref(),
                Some(resource),
                "resource class mangled through encode/decode"
            );
            // And the encoding itself is stable (persisted signatures keep matching).
            assert_eq!(decoded.encode(), t.encode());
        }
        let none = Token { app_class: "a".into(), action: "focus".into(), resource_class: None };
        assert_eq!(Token::decode(&none.encode()).expect("decodes").resource_class, None);
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
        e.window_title = Some("Personal vault".into());
        e.redaction_flags = redaction_flags::EXCLUDED;
        assert!(normalize(&e).is_none(), "EXCLUDED never mined (doc 13 §4)");

        let audit = ev(EventType::CloudSend, "aperture.exe", serde_json::json!({}));
        assert!(normalize(&audit).is_none(), "audit rows never mined");
    }

    /// 2026-09-06: `window_close` is not a doc 08 §2 action and is not mined;
    /// a nameless focus/open is a shell transient, not a behaviour; a titled
    /// focus still tokenizes. Payload-bearing types need no title.
    #[test]
    fn close_and_nameless_focus_are_not_mined_but_titled_focus_is() {
        let mut close = ev(EventType::WindowClose, "opera.exe", serde_json::json!({}));
        close.window_title = Some("Claude - Opera".into());
        assert!(normalize(&close).is_none(), "close is not a minable action");

        let nameless = ev(EventType::WindowFocus, "explorer.exe", serde_json::json!({}));
        assert!(normalize(&nameless).is_none(), "no title ⇒ shell transient");
        let mut blank = ev(EventType::WindowOpen, "explorer.exe", serde_json::json!({}));
        blank.window_title = Some("   ".into());
        assert!(normalize(&blank).is_none(), "blank title ⇒ shell transient");

        let mut titled = ev(EventType::WindowFocus, "opera.exe", serde_json::json!({}));
        titled.window_title = Some("Claude - Opera".into());
        let t = normalize(&titled).expect("a titled focus tokenizes");
        assert_eq!((t.app_class.as_str(), t.action.as_str()), ("browser", "focus"));

        let nav = ev(
            EventType::Navigation,
            "opera.exe",
            serde_json::json!({"url": "https://docs.rs/tokio"}),
        );
        assert!(normalize(&nav).is_some(), "payload-bearing types need no title");
    }

    #[test]
    fn is_minable_tracks_the_behavioural_action_list() {
        let tok = |action: &str| Token {
            app_class: "browser".into(),
            action: action.into(),
            resource_class: None,
        };
        for ok in ["focus", "open", "navigation", "media", "document", "ide"] {
            assert!(is_minable(&tok(ok)), "{ok} is a doc 08 §2 action");
        }
        for bad in ["close", "capture_toggle", "voice", "suggestion_clicked", "bogus"] {
            assert!(!is_minable(&tok(bad)), "{bad} must not be minable");
        }
    }

    #[test]
    fn vscode_forks_fold_into_the_ide_class() {
        assert_eq!(app_class("Antigravity IDE.exe"), "ide");
        assert_eq!(app_class("Cursor.exe"), "ide");
        assert_eq!(app_class("code.exe"), "ide");
        assert_eq!(app_class("opera.exe"), "browser");
        assert_eq!(app_class("claude.exe"), "claude", "unknown apps keep their own class");
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
