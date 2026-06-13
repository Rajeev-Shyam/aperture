//! Browser tab / URL connector (doc 10 §2).
//!
//! Captures the foreground browser's URL on `navigation` events by reading the
//! address-bar Edit element via UIA `ValuePattern`. **Known-flaky (RK4):** the
//! element *name* is localized and version-dependent, so resolution leans on
//! `ControlType.Edit` + keyboard-focusable heuristics rather than name alone
//! ([VERIFY] per browser/version). Fallbacks: last-known URL for the window;
//! window title as a lossy search hint.
//!
//! Resumption is honest: the stored URL → `ShellExecuteW` opens a *new tab*, so
//! the copy says "Reopen page", not "restore tab" (doc 10 §2).

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
    /// Browser brand, e.g. `"chrome" | "edge" | "firefox"` (best-effort).
    pub browser: String,
}

/// The browser connector. Reliability class **RK4** (doc 10 §2).
#[derive(Debug, Default, Clone)]
pub struct BrowserConnector;

impl BrowserConnector {
    pub fn new() -> Self {
        Self
    }
}

impl Connector for BrowserConnector {
    fn id(&self) -> &'static str {
        "browser"
    }

    fn can_capture(&self, ev: &Event) -> bool {
        // Cheap predicate: a navigation event from a recognized browser process.
        // TODO(M4): match `ev.process` against the known-browser set; YouTube
        //   navigations are claimed by the more-specific YoutubeConnector first
        //   (registration order in the registry).
        matches!(ev.r#type, EventType::Navigation)
    }

    fn capture(&self, _ev: &Event) -> Option<ConnectorState> {
        // TODO(M4): read the URL via UIA `ValuePattern` on the foreground
        //   browser's address-bar Edit element (RK4 — resolve by ControlType +
        //   focusable heuristics, NOT localized name). Fallback ladder:
        //   last-known URL for this window → window title (lossy).
        //   Build BrowserPayloadV1, serialize into `reconstruct_payload`, set
        //   payload_version = 1, stale_after_ts = captured_ts + TTL_24H.
        todo!("M4: UIA ValuePattern URL capture (RK4) → BrowserPayloadV1 / ConnectorState")
    }

    fn staleness_ttl(&self) -> Duration {
        // TTL 24 h (doc 10 §2 [ASSUMPTION]).
        Duration::from_secs(24 * 60 * 60)
    }

    fn reconstruct(&self, _st: &ConnectorState) -> Result<ResumeArtifact, ConnectorError> {
        // TODO(M4): deserialize BrowserPayloadV1 from `st.reconstruct_payload`
        //   (dispatch on `st.payload_version`); return ResumeArtifact::Url(url).
        //   No existence check — a URL is always "openable"; freshness/TTL is the
        //   pattern engine's job (doc 08 §5).
        todo!("M4: BrowserPayloadV1 → ResumeArtifact::Url")
    }

    fn open(&self, a: &ResumeArtifact) -> Result<OpenOutcome, ConnectorError> {
        // Default browser, new tab — honest "Reopen page" framing (doc 10 §2).
        deeplinker::open(a)
    }

    fn validate(&self, _cloud_payload: &serde_json::Value) -> Option<ConnectorState> {
        // TODO(M4/M7): gate Claude-suggested browser actions (doc 09 §4) —
        //   require a well-formed, http(s) `url`; reject anything else so the
        //   action button is withheld. Produce a ConnectorState with a fresh id
        //   and captured_ts = now.
        todo!("M7: validate cloud payload → http(s) url only → ConnectorState or None")
    }
}
