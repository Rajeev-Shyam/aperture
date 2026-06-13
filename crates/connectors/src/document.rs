//! Document connector (doc 10 §4).
//!
//! Captures on `document_state` / focus of known editors via a strict **path
//! resolution ladder** — and **never guesses a path**:
//!   1. a full path present in the window title;
//!   2. the title filename matched against Windows Recent Items / a per-app MRU
//!      ([VERIFY] access);
//!   3. unresolved ⇒ **no capture**.
//!
//! Resumption re-checks the file exists; a missing file degrades to opening the
//! containing folder (doc 10 §4, §6).

use std::time::Duration;

use aperture_contracts::connector::ConnectorError;
use aperture_contracts::event::{Event, EventType};
use aperture_contracts::{Connector, ConnectorState, OpenOutcome, ResumeArtifact};

use crate::deeplinker;

/// Payload v1 schema (doc 10 §4): `{path, app_hint, title}`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DocumentPayloadV1 {
    /// The resolved absolute path (the resumable target). Only ever a *resolved*
    /// path — the ladder never stores a guess.
    pub path: String,
    /// The app that owned the document, used only when the user opted into
    /// "open with same app" and it differs from the default handler (doc 10 §4).
    pub app_hint: Option<String>,
    /// Window/document title (display + the lossy search hint).
    pub title: String,
}

/// The document connector.
#[derive(Debug, Default, Clone)]
pub struct DocumentConnector;

impl DocumentConnector {
    pub fn new() -> Self {
        Self
    }

    /// The path resolution ladder (doc 10 §4). Returns `None` rather than ever
    /// guessing — an unresolved title yields no capture.
    // TODO(M4): (1) extract a full path from the title; (2) else match the title
    //   filename against Windows Recent Items / per-app MRU [VERIFY]; (3) else
    //   None. Pure where possible to keep it unit-testable.
    pub fn resolve_path(_title: &str, _app_hint: Option<&str>) -> Option<String> {
        todo!("M4: title→MRU→none path ladder; NEVER guess a path (doc 10 §4)")
    }
}

impl Connector for DocumentConnector {
    fn id(&self) -> &'static str {
        "document"
    }

    fn can_capture(&self, ev: &Event) -> bool {
        // Document-state / focus events from known editor processes.
        // TODO(M4): gate on the known-editor process set so we don't claim every
        //   focus event.
        matches!(ev.r#type, EventType::DocumentState)
    }

    fn capture(&self, _ev: &Event) -> Option<ConnectorState> {
        // TODO(M4): run resolve_path; if None ⇒ return None (no capture, doc 10
        //   §4). Else build DocumentPayloadV1, serialize into
        //   `reconstruct_payload`, payload_version = 1,
        //   stale_after_ts = captured_ts + TTL_7D.
        todo!("M4: resolve path (or no-capture) → DocumentPayloadV1 / ConnectorState")
    }

    fn staleness_ttl(&self) -> Duration {
        // TTL 7 d (doc 10 §4).
        Duration::from_secs(7 * 24 * 60 * 60)
    }

    fn reconstruct(&self, _st: &ConnectorState) -> Result<ResumeArtifact, ConnectorError> {
        // TODO(M4): deserialize DocumentPayloadV1 (dispatch on payload_version);
        //   re-check existence here (doc 10 §4). File present ⇒
        //   ResumeArtifact::FileOpen{ path, app_hint }; file gone ⇒
        //   Err(ConnectorError::TargetGone(path)) so `open`/the bubble degrades
        //   to the containing folder. (deeplinker also re-checks, defense in
        //   depth — Path B is a single round trip.)
        todo!("M4: DocumentPayloadV1 + existence re-check → FileOpen | TargetGone")
    }

    fn open(&self, a: &ResumeArtifact) -> Result<OpenOutcome, ConnectorError> {
        // Default (or hinted) handler; missing-file degrade lives in deeplinker.
        deeplinker::open(a)
    }

    fn validate(&self, _cloud_payload: &serde_json::Value) -> Option<ConnectorState> {
        // TODO(M7): gate Claude-suggested document actions (doc 09 §4) — require
        //   an absolute, existing path; reject relative/guessed paths so the
        //   button is withheld (mirrors the never-guess rule).
        todo!("M7: validate cloud payload → require existing abs path → ConnectorState or None")
    }
}
