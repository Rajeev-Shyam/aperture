//! Contract 3 — the Connector trait (doc 10 §1, doc 15 §3): the expansion seam (G3).
//!
//! The pattern engine and the Bubble UI know **only this trait**, so v2
//! connectors (Slack thread, terminal cwd, Figma frame…) plug in without core
//! changes. `reconstruct_payload` carries a `payload_version` for forward
//! migration; migrations are per-connector pure functions `v(n) -> v(n+1)`.
//! `validate()` is mandatory before any action **executes** — validate-on-click
//! (ADR-035): the button renders optimistically, the connector validates before
//! any `ShellExecute`/protocol dispatch and fails gracefully. The safety law is
//! unchanged: the cloud can *suggest*, only a connector can *act*, and nothing
//! executes unvalidated.

use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::event::Event;

/// The durable, resumable handle for one observed context (doc 03 `connector_state`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorState {
    /// uuid.
    pub id: String,
    /// `"browser" | "youtube" | "document" | "ide"`.
    pub connector_type: String,
    /// Versioned, per-type JSON (doc 10 per-type v1 schemas).
    pub reconstruct_payload: serde_json::Value,
    pub payload_version: i32,
    pub captured_ts: i64,
    /// Per-connector TTL boundary (doc 10); past this the freshness factor zeroes
    /// the candidate (doc 08 §5) so stale bubbles are prevented, not apologized for.
    pub stale_after_ts: Option<i64>,
}

/// What `reconstruct` produces and `open` dispatches (doc 02 §5, doc 10).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResumeArtifact {
    /// A plain URL opened in the default browser via `ShellExecuteW`.
    Url(String),
    /// A registered protocol URI, e.g. `vscode://file/C:/p/x.rs:120:5`.
    ProtocolUri(String),
    /// A file path opened via the default (or hinted) handler.
    FileOpen { path: String, app_hint: Option<String> },
}

/// Outcome of an `open` dispatch, recorded for SC7 telemetry (doc 10 §6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OpenOutcome {
    /// Resumed at the precise captured state.
    Resumed,
    /// Opened, but the precise state was unavailable (honest degrade, e.g. YouTube "from the start").
    Degraded { reason: String },
    /// Target gone (file deleted, video private); bubble swaps to fallback copy.
    Failed { reason: String },
}

/// The connector contract. Connectors self-register at startup (doc 10 §1).
pub trait Connector: Send + Sync {
    /// `"browser" | "youtube" | "document" | "ide"` (and v2 ids).
    fn id(&self) -> &'static str;
    /// Cheap predicate on a bus event.
    fn can_capture(&self, ev: &Event) -> bool;
    /// Snapshot the resumable handle (-> versioned `reconstruct_payload`).
    fn capture(&self, ev: &Event) -> Option<ConnectorState>;
    /// When captured state stops being trustworthy.
    fn staleness_ttl(&self) -> Duration;
    /// Build the artifact from stored state.
    fn reconstruct(&self, st: &ConnectorState) -> Result<ResumeArtifact, ConnectorError>;
    /// Dispatch via `ShellExecuteW` / protocol handler.
    fn open(&self, a: &ResumeArtifact) -> Result<OpenOutcome, ConnectorError>;
    /// Gate for Claude-suggested actions (doc 09 §4): produce a well-formed state
    /// from cloud JSON, or `None` and the action button is withheld.
    fn validate(&self, cloud_payload: &serde_json::Value) -> Option<ConnectorState>;
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    #[error("reconstruct target is gone: {0}")]
    TargetGone(String),
    #[error("protocol handler unregistered: {0}")]
    HandlerUnregistered(String),
    #[error("captured state is stale (past TTL)")]
    Stale,
    #[error("dispatch failed: {0}")]
    DispatchFailed(String),
}
