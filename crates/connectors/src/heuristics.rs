//! "Connector heuristics" (doc 03 §2): synthesize the secondary event types —
//! `document_state` / `ide_state` — from primary focus/title events of known
//! editor processes.
//!
//! Doc 03's event taxonomy sources these two types from *connector heuristics*
//! rather than OS hooks: the capture crate only knows windows and titles, while
//! the path-resolution ladders (doc 10 §4–5) are connector knowledge. The
//! pipeline runs [`SecondaryDeriver::derive`] over primary bus events; a `Some`
//! result is persisted + published like any other event, where the document/IDE
//! connectors then claim it (`can_capture` on the synthesized type) and the
//! pattern engine tokenizes it (`doc:xlsx`, `ide:rs`, … — doc 08 §2).
//!
//! **Never guess** (doc 10 §4): derivation only succeeds when a real, existing
//! path resolved. Excluded events (redaction flag) never derive anything.

use aperture_contracts::event::{redaction_flags, Event, EventType};

use crate::document::DocumentConnector;
use crate::ide::IdeConnector;
use crate::vscode_mru::VsCodeMru;

/// VS Code process names (lowercase). Other editors are v2 connectors (doc 10 §5).
const IDE_PROCESSES: &[&str] = &["code.exe", "code - insiders.exe"];

/// Known document editors/viewers (lowercase) whose focus events are worth
/// running the document ladder on. [ASSUMPTION — extend as dogfooding surfaces
/// more; a miss only means no capture, never a wrong one.]
const DOCUMENT_PROCESSES: &[&str] = &[
    "winword.exe",
    "excel.exe",
    "powerpnt.exe",
    "notepad.exe",
    "notepad++.exe",
    "sumatrapdf.exe",
    "acrord32.exe",
    "acrobat.exe",
    "wordpad.exe",
];

/// Derives secondary (`document_state` / `ide_state`) events from primary ones.
/// Holds the VS Code MRU resolver + its caches, so construct once and share.
#[derive(Default)]
pub struct SecondaryDeriver {
    vscode: VsCodeMru,
}

impl SecondaryDeriver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test/spike hook: point the VS Code MRU at a specific `state.vscdb`.
    pub fn with_vscode_state_db(state_db: std::path::PathBuf) -> Self {
        Self {
            vscode: VsCodeMru::with_state_db(state_db),
        }
    }

    /// Derive a secondary event from a primary focus/title event, or `None`.
    ///
    /// The returned event carries `id: 0` (unpersisted — the pipeline assigns
    /// the row id via its normal persist-then-publish path), the source event's
    /// timestamp/app/process/title, and a type-specific payload per doc 03 §2.
    pub fn derive(&self, ev: &Event) -> Option<Event> {
        // Excluded contexts stay metadata-only — never mine a path out of them.
        if ev.redaction_flags & redaction_flags::EXCLUDED != 0 {
            return None;
        }
        if !matches!(ev.r#type, EventType::WindowFocus | EventType::WindowOpen) {
            return None;
        }
        let process = ev.process.as_deref()?.to_ascii_lowercase();
        let title = ev.window_title.as_deref()?;
        if title.is_empty() {
            return None;
        }
        if IDE_PROCESSES.contains(&process.as_str()) {
            self.derive_ide(ev, title)
        } else if DOCUMENT_PROCESSES.contains(&process.as_str()) {
            self.derive_document(ev, title, &process)
        } else {
            None
        }
    }

    fn derive_ide(&self, ev: &Event, title: &str) -> Option<Event> {
        let parsed = IdeConnector::parse_title(title)?;
        // A customized window.title may hold the full path already; the default
        // holds only the filename → workspace-MRU resolution (Q56 baseline).
        let path = if std::path::Path::new(&parsed.file).is_absolute()
            && std::path::Path::new(&parsed.file).is_file()
        {
            parsed.file.clone()
        } else {
            self.vscode.resolve(&parsed.workspace, &parsed.file)?
        };
        // Line/col: best-effort from a prior precise ide_state (doc 10 §5) — no
        // v1 source emits one, so they are absent (⇒ null) here.
        let payload = serde_json::json!({
            "path": path,
            "workspace": parsed.workspace,
        });
        Some(secondary_event(ev, EventType::IdeState, payload))
    }

    fn derive_document(&self, ev: &Event, title: &str, process: &str) -> Option<Event> {
        let path = DocumentConnector::resolve_path(title, Some(process))?;
        let payload = serde_json::json!({
            "path": path,
            "app": process,
            "title": title,
        });
        Some(secondary_event(ev, EventType::DocumentState, payload))
    }
}

fn secondary_event(src: &Event, r#type: EventType, payload: serde_json::Value) -> Event {
    Event {
        id: 0,
        ts: src.ts,
        r#type,
        app: src.app.clone(),
        process: src.process.clone(),
        window_title: src.window_title.clone(),
        payload,
        connector_id: None,
        session_id: None,
        redaction_flags: src.redaction_flags,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn focus_event(process: &str, title: &str) -> Event {
        Event {
            id: 42,
            ts: 11_000,
            r#type: EventType::WindowFocus,
            app: Some("office".into()),
            process: Some(process.into()),
            window_title: Some(title.into()),
            payload: json!({}),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        }
    }

    #[test]
    fn derives_document_state_for_known_editors() {
        let dir = std::env::temp_dir().join("aperture-heur-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("todo.txt");
        std::fs::write(&file, b"x").unwrap();

        let d = SecondaryDeriver::new();
        let title = format!("{} - Notepad", file.display());
        let ev = d
            .derive(&focus_event("notepad.exe", &title))
            .expect("derived");
        assert!(matches!(ev.r#type, EventType::DocumentState));
        assert_eq!(ev.id, 0);
        assert_eq!(ev.ts, 11_000);
        assert_eq!(
            ev.payload.get("path").and_then(|v| v.as_str()),
            Some(file.display().to_string().as_str())
        );
    }

    #[test]
    fn unknown_processes_and_unresolved_titles_derive_nothing() {
        let d = SecondaryDeriver::new();
        assert!(d.derive(&focus_event("randomapp.exe", "whatever")).is_none());
        // Known editor, but the ladder floor: no path, no guess, no event.
        assert!(d
            .derive(&focus_event("winword.exe", "Document1 - Word"))
            .is_none());
    }

    #[test]
    fn excluded_events_never_derive() {
        let dir = std::env::temp_dir().join("aperture-heur-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("secret.txt");
        std::fs::write(&file, b"x").unwrap();

        let d = SecondaryDeriver::new();
        let mut ev = focus_event("notepad.exe", &format!("{} - Notepad", file.display()));
        ev.redaction_flags = redaction_flags::EXCLUDED;
        assert!(d.derive(&ev).is_none());
    }

    #[test]
    fn derives_ide_state_when_title_carries_the_full_path() {
        let dir = std::env::temp_dir().join("aperture-heur-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.rs");
        std::fs::write(&file, b"fn main() {}").unwrap();

        let d = SecondaryDeriver::new();
        let title = format!("{} - aperture - Visual Studio Code", file.display());
        let ev = d.derive(&focus_event("Code.exe", &title)).expect("derived");
        assert!(matches!(ev.r#type, EventType::IdeState));
        assert_eq!(
            ev.payload.get("workspace").and_then(|v| v.as_str()),
            Some("aperture")
        );
    }
}
