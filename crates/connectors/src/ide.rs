//! IDE connector — VS Code first (doc 10 §5).
//!
//! Captures `ide_state` by parsing the window title
//! `"● {file} - {workspace} - Visual Studio Code"` (dirty-dot aware) [VERIFY per
//! VS Code version]; when the title holds only a filename, the path is resolved
//! via the workspace MRU. Line/col are best-effort (a prior precise `ide_state`,
//! else null).
//!
//! Reconstruct ladder (doc 10 §5, §6):
//!   1. `vscode://file/{abs_path}:{line}:{col}` → protocol handler;
//!   2. fallback `code -g {path}:{line}` CLI [VERIFY availability];
//!   3. final fallback: plain file open.
//!
//! Other editors are v2 connectors behind the same trait, each with its own
//! scheme (`jetbrains://`, …).

use std::time::Duration;

use aperture_contracts::connector::ConnectorError;
use aperture_contracts::event::{Event, EventType};
use aperture_contracts::{Connector, ConnectorState, OpenOutcome, ResumeArtifact};

use crate::deeplinker;

/// Payload v1 schema (doc 10 §5): `{path, line|null, col|null, workspace}`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IdePayloadV1 {
    /// Resolved absolute file path (the resumable target).
    pub path: String,
    /// 1-based line (best-effort; `None` when no precise `ide_state` was seen).
    pub line: Option<u32>,
    /// 1-based column (best-effort; `None` when unknown).
    pub col: Option<u32>,
    /// The workspace/folder name parsed from the title (display + MRU key).
    pub workspace: String,
}

/// Parsed pieces of a VS Code window title (doc 10 §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedIdeTitle {
    /// `true` when the leading dirty-dot `●` was present (unsaved changes).
    pub dirty: bool,
    /// The `{file}` segment — may be a bare filename needing MRU resolution.
    pub file: String,
    /// The `{workspace}` segment.
    pub workspace: String,
}

/// The IDE connector (VS Code).
#[derive(Debug, Default, Clone)]
pub struct IdeConnector;

impl IdeConnector {
    pub fn new() -> Self {
        Self
    }

    /// Parse `"● {file} - {workspace} - Visual Studio Code"` (dirty-dot aware).
    /// Pure + unit-testable; the brittle bit is the literal suffix and the ` - `
    /// separator, which shift across versions [VERIFY].
    // TODO(M4): strip an optional leading "● ", split on " - ", require the
    //   trailing "Visual Studio Code"; return None on any other shape.
    pub fn parse_title(_title: &str) -> Option<ParsedIdeTitle> {
        todo!("M4: parse VS Code title (dirty-dot, ' - ' segments) [VERIFY per version]")
    }

    /// Build the `vscode://file/{abs_path}:{line}:{col}` protocol URI — rung 1 of
    /// the reconstruct ladder (doc 10 §5). Omits `:line:col` when line is `None`.
    // TODO(M4): percent-encode the abs path per the documented URI form; use
    //   forward slashes; only append `:{line}` / `:{line}:{col}` when present.
    pub fn build_vscode_uri(_abs_path: &str, _line: Option<u32>, _col: Option<u32>) -> String {
        todo!("M4: format vscode://file/<abs>:<line>:<col> (documented URI form)")
    }
}

impl Connector for IdeConnector {
    fn id(&self) -> &'static str {
        "ide"
    }

    fn can_capture(&self, ev: &Event) -> bool {
        // ide_state events from VS Code.
        // TODO(M4): confirm the process is VS Code (Code.exe) before claiming.
        matches!(ev.r#type, EventType::IdeState)
    }

    fn capture(&self, _ev: &Event) -> Option<ConnectorState> {
        // TODO(M4): parse_title; resolve the file to an abs path via workspace
        //   MRU when only a filename is present (never guess — mirror doc 10 §4).
        //   Carry forward line/col from a prior precise ide_state if any, else
        //   null. Build IdePayloadV1, serialize, payload_version = 1,
        //   stale_after_ts = captured_ts + TTL_7D.
        todo!("M4: title parse + workspace-MRU path resolve → IdePayloadV1 / ConnectorState")
    }

    fn staleness_ttl(&self) -> Duration {
        // TTL 7 d (doc 10 §5).
        Duration::from_secs(7 * 24 * 60 * 60)
    }

    fn reconstruct(&self, _st: &ConnectorState) -> Result<ResumeArtifact, ConnectorError> {
        // TODO(M4): deserialize IdePayloadV1 (dispatch on payload_version);
        //   re-check the file exists (gone ⇒ TargetGone → folder degrade). Else
        //   ResumeArtifact::ProtocolUri(build_vscode_uri(..)); the deeplinker
        //   walks the ladder (protocol → `code -g` → plain open) and may return
        //   Degraded if it had to fall back a rung (doc 10 §5, §6).
        todo!("M4: IdePayloadV1 → ResumeArtifact::ProtocolUri(vscode://file/...)")
    }

    fn open(&self, a: &ResumeArtifact) -> Result<OpenOutcome, ConnectorError> {
        // Ladder lives in deeplinker::open_protocol_uri: vscode:// → `code -g`
        // CLI fallback → plain open (doc 10 §5).
        deeplinker::open(a)
    }

    fn validate(&self, _cloud_payload: &serde_json::Value) -> Option<ConnectorState> {
        // TODO(M7): gate Claude-suggested IDE actions (doc 09 §4) — require an
        //   absolute, existing path; clamp line/col; reject otherwise so the
        //   button is withheld.
        todo!("M7: validate cloud payload → require existing abs path → ConnectorState or None")
    }
}
