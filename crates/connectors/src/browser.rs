//! Browser tab / URL connector (doc 10 §2).
//!
//! Consumes `navigation` events. Since R2 (ADR-027) the URL on those events
//! comes **primarily from the browser extension** (tabs API via the
//! native-messaging host); the UIA address-bar read is the no-extension
//! fallback and lives in `aperture-capture` — by the time an event reaches this
//! connector the URL is already in its payload, whatever the source.
//!
//! Resumption is honest: the stored URL → `ShellExecuteW` opens a *new tab*, so
//! the copy says "Reopen page", not "restore tab" (doc 10 §2). Validation is
//! on-click (ADR-035).

use std::time::Duration;

use aperture_contracts::connector::ConnectorError;
use aperture_contracts::event::{Event, EventType};
use aperture_contracts::{Connector, ConnectorState, OpenOutcome, ResumeArtifact};

use crate::deeplinker;

/// Payload v1 schema (doc 10 §2): `{url, title, browser}`.
///
/// Versioned via [`ConnectorState::payload_version`] = 1; additive-only, with a
/// per-connector `v(n) -> v(n+1)` migration if the shape ever changes (doc 15 §6).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrowserPayloadV1 {
    /// The captured page URL (the resumable target).
    pub url: String,
    /// Tab/page title — also the lossy fallback search hint.
    pub title: String,
    /// Browser brand, e.g. `"chrome" | "opera" | "edge" | "firefox"` (best-effort).
    pub browser: String,
}

/// The browser connector. RK4 is retired to the fallback path by ADR-027.
#[derive(Debug, Default, Clone)]
pub struct BrowserConnector;

const TTL_24H: Duration = Duration::from_secs(24 * 60 * 60);

impl BrowserConnector {
    pub fn new() -> Self {
        Self
    }
}

/// Only http(s) pages are resumable "Reopen page" targets — never chrome://,
/// file://, about:, extension pages, etc.
fn is_resumable_url(url: &str) -> bool {
    url::Url::parse(url)
        .map(|u| matches!(u.scheme(), "http" | "https"))
        .unwrap_or(false)
}

fn is_watch_url(url: &str) -> bool {
    url.contains("youtube.com/watch") || url.contains("youtu.be/")
}

impl Connector for BrowserConnector {
    fn id(&self) -> &'static str {
        "browser"
    }

    fn can_capture(&self, ev: &Event) -> bool {
        // Cheap predicate: a navigation event carrying an http(s) URL. YouTube
        // watch URLs are claimed by the more-specific YoutubeConnector first
        // (registration order) — re-checked here as defense in depth.
        matches!(ev.r#type, EventType::Navigation)
            && ev
                .payload
                .get("url")
                .and_then(|v| v.as_str())
                .is_some_and(|u| is_resumable_url(u) && !is_watch_url(u))
    }

    fn capture(&self, ev: &Event) -> Option<ConnectorState> {
        let url = ev.payload.get("url").and_then(|v| v.as_str())?;
        if !is_resumable_url(url) || is_watch_url(url) {
            return None;
        }
        let title = ev
            .payload
            .get("title")
            .and_then(|v| v.as_str())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .or_else(|| ev.window_title.clone())
            .unwrap_or_default();
        let browser = ev
            .payload
            .get("browser")
            .and_then(|v| v.as_str())
            .filter(|b| !b.is_empty())
            .map(str::to_string)
            .or_else(|| ev.process.clone())
            .unwrap_or_default();
        let payload = BrowserPayloadV1 {
            url: url.to_string(),
            title,
            browser,
        };
        Some(crate::build_state(
            self.id(),
            serde_json::to_value(payload).ok()?,
            ev.ts,
            self.staleness_ttl(),
        ))
    }

    fn staleness_ttl(&self) -> Duration {
        // TTL 24 h (doc 10 §2 [ASSUMPTION], Q57 unchanged).
        TTL_24H
    }

    fn reconstruct(&self, st: &ConnectorState) -> Result<ResumeArtifact, ConnectorError> {
        // Additive-only payloads (doc 15 §6): any version parses as v1.
        let payload: BrowserPayloadV1 = serde_json::from_value(st.reconstruct_payload.clone())
            .map_err(|e| ConnectorError::DispatchFailed(format!("bad browser payload: {e}")))?;
        // No existence check — a URL is always "openable"; freshness/TTL is the
        // pattern engine's job (doc 08 §5).
        Ok(ResumeArtifact::Url(payload.url))
    }

    fn open(&self, a: &ResumeArtifact) -> Result<OpenOutcome, ConnectorError> {
        // Default browser, new tab — honest "Reopen page" framing (doc 10 §2).
        deeplinker::open(a)
    }

    fn validate(&self, cloud_payload: &serde_json::Value) -> Option<ConnectorState> {
        // Gate for Claude-suggested browser actions (doc 09 §4, ADR-035): require
        // a well-formed http(s) `url`; reject anything else so the action button
        // is withheld.
        let url = cloud_payload.get("url").and_then(|v| v.as_str())?;
        if !is_resumable_url(url) {
            return None;
        }
        let payload = BrowserPayloadV1 {
            url: url.to_string(),
            title: cloud_payload
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            browser: String::new(),
        };
        Some(crate::build_state(
            self.id(),
            serde_json::to_value(payload).ok()?,
            crate::epoch_ms(),
            self.staleness_ttl(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nav_event(url: &str) -> Event {
        Event {
            id: 1,
            ts: 5_000,
            r#type: EventType::Navigation,
            app: Some("browser".into()),
            process: Some("chrome.exe".into()),
            window_title: Some("Docs - Google Chrome".into()),
            payload: json!({ "url": url, "browser": "chrome" }),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        }
    }

    #[test]
    fn captures_http_navigations() {
        let c = BrowserConnector::new();
        let ev = nav_event("https://docs.rs/tokio");
        assert!(c.can_capture(&ev));
        let st = c.capture(&ev).expect("captured");
        assert_eq!(st.connector_type, "browser");
        assert_eq!(st.stale_after_ts, Some(ev.ts + 24 * 60 * 60 * 1000));
        let p: BrowserPayloadV1 = serde_json::from_value(st.reconstruct_payload).unwrap();
        assert_eq!(p.url, "https://docs.rs/tokio");
        assert_eq!(p.browser, "chrome");
    }

    #[test]
    fn skips_non_http_and_watch_urls() {
        let c = BrowserConnector::new();
        assert!(!c.can_capture(&nav_event("chrome://settings")));
        assert!(!c.can_capture(&nav_event("file:///C:/x.html")));
        // YouTube watch URLs belong to the YouTube connector.
        assert!(!c.can_capture(&nav_event("https://www.youtube.com/watch?v=dQw4w9WgXcQ")));
        assert!(c.capture(&nav_event("about:blank")).is_none());
    }

    #[test]
    fn reconstructs_the_stored_url() {
        let c = BrowserConnector::new();
        let st = c.capture(&nav_event("https://docs.rs/tokio")).unwrap();
        match c.reconstruct(&st).unwrap() {
            ResumeArtifact::Url(u) => assert_eq!(u, "https://docs.rs/tokio"),
            other => panic!("expected Url, got {other:?}"),
        }
    }

    #[test]
    fn validate_requires_http_url() {
        let c = BrowserConnector::new();
        assert!(c.validate(&json!({ "url": "https://example.com" })).is_some());
        assert!(c.validate(&json!({ "url": "file:///C:/secrets.txt" })).is_none());
        assert!(c.validate(&json!({ "title": "no url" })).is_none());
    }
}
