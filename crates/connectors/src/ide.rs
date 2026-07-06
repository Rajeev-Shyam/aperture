//! IDE connector — VS Code first (doc 10 §5).
//!
//! Captures `ide_state` events (synthesized by [`crate::heuristics`] from VS
//! Code window titles). **Q56 spike decision (M4):** baseline is title parsing —
//! `"● {file} - {workspace} - Visual Studio Code"` (dirty-dot aware) [VERIFY per
//! VS Code version] — with the abs path resolved by the heuristics stage's
//! workspace-MRU walk when the title holds only a filename (never guess — an
//! ambiguous filename yields no capture). Line/col are best-effort (a prior
//! precise `ide_state` if any — no v1 source emits one, so they stay null).
//!
//! Reconstruct ladder (doc 10 §5, §6):
//!   1. `vscode://file/{abs_path}:{line}:{col}` → protocol handler;
//!   2. fallback `code -g {path}:{line}` CLI [VERIFY availability];
//!   3. final fallback: plain file open.
//!
//! Other editors are v2 connectors behind the same trait, each with its own
//! scheme (`jetbrains://`, …).

use std::path::Path;
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
    /// The `{workspace}` segment (empty when the title had no workspace part).
    pub workspace: String,
}

/// The IDE connector (VS Code).
#[derive(Debug, Default, Clone)]
pub struct IdeConnector;

const TTL_7D: Duration = Duration::from_secs(7 * 24 * 60 * 60);

impl IdeConnector {
    pub fn new() -> Self {
        Self
    }

    /// Parse `"● {file} - {workspace} - Visual Studio Code"` (dirty-dot aware).
    /// Pure + unit-testable; the brittle bit is the literal suffix and the ` - `
    /// separator, which shift across versions [VERIFY]. Filenames may themselves
    /// contain ` - `, so the *last* segment anchors the product and the
    /// second-to-last the workspace; everything before re-joins as the file.
    pub fn parse_title(title: &str) -> Option<ParsedIdeTitle> {
        let (dirty, rest) = match title.strip_prefix('●') {
            Some(r) => (true, r.trim_start()),
            None => (false, title),
        };
        let segments: Vec<&str> = rest.split(" - ").collect();
        if segments.len() < 2 {
            return None;
        }
        // "Visual Studio Code [Administrator]" / "Visual Studio Code - Insiders"
        // both anchor on the prefix. (Insiders' own " - " split means the last
        // segment is "Insiders"; accept when the previous one is the anchor.)
        let product_idx = if segments[segments.len() - 1].starts_with("Visual Studio Code") {
            segments.len() - 1
        } else if segments.len() >= 3
            && segments[segments.len() - 2].starts_with("Visual Studio Code")
        {
            segments.len() - 2
        } else {
            return None;
        };
        match product_idx {
            0 => None, // just "Visual Studio Code" — no file open
            1 => Some(ParsedIdeTitle {
                dirty,
                file: segments[0].to_string(),
                workspace: String::new(),
            }),
            _ => Some(ParsedIdeTitle {
                dirty,
                file: segments[..product_idx - 1].join(" - "),
                workspace: segments[product_idx - 1].to_string(),
            }),
        }
    }

    /// Build the `vscode://file/{abs_path}:{line}:{col}` protocol URI — rung 1 of
    /// the reconstruct ladder (doc 10 §5). Forward slashes; minimal
    /// percent-encoding (space/%/#/?); `:line[:col]` only when present.
    pub fn build_vscode_uri(abs_path: &str, line: Option<u32>, col: Option<u32>) -> String {
        let normalized = abs_path.replace('\\', "/");
        let mut encoded = String::with_capacity(normalized.len());
        for ch in normalized.chars() {
            match ch {
                ' ' => encoded.push_str("%20"),
                '%' => encoded.push_str("%25"),
                '#' => encoded.push_str("%23"),
                '?' => encoded.push_str("%3F"),
                _ => encoded.push(ch),
            }
        }
        let mut uri = format!("vscode://file/{encoded}");
        if let Some(l) = line {
            uri.push_str(&format!(":{l}"));
            if let Some(c) = col {
                uri.push_str(&format!(":{c}"));
            }
        }
        uri
    }
}

impl Connector for IdeConnector {
    fn id(&self) -> &'static str {
        "ide"
    }

    fn can_capture(&self, ev: &Event) -> bool {
        // ide_state events are synthesized only for VS Code processes
        // (crate::heuristics), so the type check is the whole predicate.
        matches!(ev.r#type, EventType::IdeState)
    }

    fn capture(&self, ev: &Event) -> Option<ConnectorState> {
        // The heuristics stage resolved the abs path (title parse + workspace-MRU
        // walk); re-verify so a hand-crafted event can never store a guess.
        let path = ev.payload.get("path").and_then(|v| v.as_str())?;
        if path.is_empty() || !Path::new(path).is_file() {
            return None;
        }
        let as_u32 = |key: &str| {
            ev.payload
                .get(key)
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok())
        };
        let payload = IdePayloadV1 {
            path: path.to_string(),
            line: as_u32("line"),
            col: as_u32("col"),
            workspace: ev
                .payload
                .get("workspace")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        };
        Some(crate::build_state(
            self.id(),
            serde_json::to_value(payload).ok()?,
            ev.ts,
            self.staleness_ttl(),
        ))
    }

    fn staleness_ttl(&self) -> Duration {
        // TTL 7 d (doc 10 §5).
        TTL_7D
    }

    fn reconstruct(&self, st: &ConnectorState) -> Result<ResumeArtifact, ConnectorError> {
        let payload: IdePayloadV1 = serde_json::from_value(st.reconstruct_payload.clone())
            .map_err(|e| ConnectorError::DispatchFailed(format!("bad ide payload: {e}")))?;
        // Existence re-check (gone ⇒ TargetGone → folder degrade upstream).
        if !Path::new(&payload.path).is_file() {
            return Err(ConnectorError::TargetGone(payload.path));
        }
        Ok(ResumeArtifact::ProtocolUri(Self::build_vscode_uri(
            &payload.path,
            payload.line,
            payload.col,
        )))
    }

    fn open(&self, a: &ResumeArtifact) -> Result<OpenOutcome, ConnectorError> {
        // Ladder lives in deeplinker::open_protocol_uri: vscode:// → `code -g`
        // CLI fallback → plain open (doc 10 §5).
        deeplinker::open(a)
    }

    fn validate(&self, cloud_payload: &serde_json::Value) -> Option<ConnectorState> {
        // Gate for Claude-suggested IDE actions (doc 09 §4, ADR-035): require an
        // absolute, existing path; clamp line/col; reject otherwise.
        let path = cloud_payload.get("path").and_then(|v| v.as_str())?;
        let p = Path::new(path);
        if !p.is_absolute() || !p.is_file() {
            return None;
        }
        let as_u32 = |key: &str| {
            cloud_payload
                .get(key)
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok())
        };
        let payload = IdePayloadV1 {
            path: path.to_string(),
            line: as_u32("line"),
            col: as_u32("col"),
            workspace: cloud_payload
                .get("workspace")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
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

    #[test]
    fn parses_vscode_titles() {
        assert_eq!(
            IdeConnector::parse_title("main.rs - aperture - Visual Studio Code"),
            Some(ParsedIdeTitle {
                dirty: false,
                file: "main.rs".into(),
                workspace: "aperture".into()
            })
        );
        assert_eq!(
            IdeConnector::parse_title("● main.rs - aperture - Visual Studio Code"),
            Some(ParsedIdeTitle {
                dirty: true,
                file: "main.rs".into(),
                workspace: "aperture".into()
            })
        );
        // No workspace segment.
        assert_eq!(
            IdeConnector::parse_title("scratch.py - Visual Studio Code"),
            Some(ParsedIdeTitle {
                dirty: false,
                file: "scratch.py".into(),
                workspace: String::new()
            })
        );
        // Filename containing " - " re-joins.
        assert_eq!(
            IdeConnector::parse_title("notes - draft.md - ws - Visual Studio Code"),
            Some(ParsedIdeTitle {
                dirty: false,
                file: "notes - draft.md".into(),
                workspace: "ws".into()
            })
        );
        // Not VS Code.
        assert_eq!(IdeConnector::parse_title("main.rs - Sublime Text"), None);
        assert_eq!(IdeConnector::parse_title("Visual Studio Code"), None);
    }

    #[test]
    fn builds_vscode_uris() {
        assert_eq!(
            IdeConnector::build_vscode_uri(r"C:\p\x.rs", Some(120), Some(5)),
            "vscode://file/C:/p/x.rs:120:5"
        );
        assert_eq!(
            IdeConnector::build_vscode_uri(r"C:\p\x.rs", Some(120), None),
            "vscode://file/C:/p/x.rs:120"
        );
        assert_eq!(
            IdeConnector::build_vscode_uri(r"C:\my dir\x.rs", None, None),
            "vscode://file/C:/my%20dir/x.rs"
        );
    }

    #[test]
    fn capture_requires_existing_path_and_reconstructs_uri() {
        let dir = std::env::temp_dir().join("aperture-ide-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.rs");
        std::fs::write(&file, b"fn main() {}").unwrap();
        let path = file.display().to_string();

        let ev = Event {
            id: 1,
            ts: 7_000,
            r#type: EventType::IdeState,
            app: Some("ide".into()),
            process: Some("Code.exe".into()),
            window_title: Some("main.rs - aperture - Visual Studio Code".into()),
            payload: json!({ "path": path, "workspace": "aperture" }),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        };
        let c = IdeConnector::new();
        assert!(c.can_capture(&ev));
        let st = c.capture(&ev).expect("captured");
        match c.reconstruct(&st).unwrap() {
            ResumeArtifact::ProtocolUri(uri) => {
                assert!(uri.starts_with("vscode://file/"));
                assert!(uri.ends_with("main.rs"));
            }
            other => panic!("expected ProtocolUri, got {other:?}"),
        }

        // Bogus path never captures (never guess).
        let mut bogus = ev.clone();
        bogus.payload = json!({ "path": r"C:\gone\main.rs" });
        assert!(c.capture(&bogus).is_none());
    }

    #[test]
    fn validate_requires_existing_absolute_path() {
        let dir = std::env::temp_dir().join("aperture-ide-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("lib.rs");
        std::fs::write(&file, b"").unwrap();

        let c = IdeConnector::new();
        let st = c
            .validate(&json!({ "path": file.display().to_string(), "line": 12 }))
            .expect("valid");
        let p: IdePayloadV1 = serde_json::from_value(st.reconstruct_payload).unwrap();
        assert_eq!(p.line, Some(12));
        assert!(c.validate(&json!({ "path": "src/lib.rs" })).is_none());
    }
}
