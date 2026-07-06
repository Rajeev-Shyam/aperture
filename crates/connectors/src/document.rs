//! Document connector (doc 10 §4).
//!
//! Captures on `document_state` events (synthesized by [`crate::heuristics`]
//! from focus/title events of known editors) via a strict **path resolution
//! ladder** — and **never guesses a path**:
//!   1. a full path present in the window title (existence-checked);
//!   2. the title filename matched against Windows Recent Items (`.lnk` target
//!      resolved via `IShellLinkW`);
//!   3. per-app MRU registry reads — deliberately deferred: version-fragile per
//!      app (`[VERIFY]`, Q62); rungs 1–2 cover the common editors, and the floor
//!      below keeps us honest meanwhile;
//!   4. unresolved ⇒ **no capture** (the floor).
//!
//! Resumption re-checks the file exists; a missing file degrades to opening the
//! containing folder (doc 10 §4, §6).

use std::path::Path;
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

const TTL_7D: Duration = Duration::from_secs(7 * 24 * 60 * 60);

impl DocumentConnector {
    pub fn new() -> Self {
        Self
    }

    /// The path resolution ladder (doc 10 §4). Returns `None` rather than ever
    /// guessing — an unresolved title yields no capture.
    pub fn resolve_path(title: &str, app_hint: Option<&str>) -> Option<String> {
        Self::resolve_path_with(title, app_hint, recent_items_lookup)
    }

    /// Ladder core with the Recent-Items rung injected, so rungs 1 and 4 are
    /// unit-testable without a real `%APPDATA%\..\Recent` directory.
    fn resolve_path_with(
        title: &str,
        _app_hint: Option<&str>,
        recent_lookup: impl Fn(&str) -> Option<String>,
    ) -> Option<String> {
        // Rung 1: a full path in the title, existence-checked.
        if let Some(path) = extract_title_path(title) {
            if Path::new(&path).is_file() {
                return Some(path);
            }
        }
        // Rung 2: title filename × Windows Recent Items.
        if let Some(filename) = extract_title_filename(title) {
            if let Some(path) = recent_lookup(&filename) {
                if Path::new(&path).is_file() {
                    return Some(path);
                }
            }
        }
        // Rung 3 (per-app MRU registry) deferred — [VERIFY]/Q62, version-fragile.
        // Rung 4: the floor — never guess.
        None
    }
}

/// Extract a drive-letter path embedded in a window title, e.g.
/// `"C:\notes\todo.txt - Notepad"` or `"*C:\x\y.md - Notepad++"` (dirty marker).
/// Stops at the ` - ` title separator or end-of-string; trims quote/paren wrap.
fn extract_title_path(title: &str) -> Option<String> {
    let bytes = title.as_bytes();
    let mut start = None;
    for i in 0..bytes.len().saturating_sub(2) {
        if bytes[i].is_ascii_alphabetic()
            && bytes[i + 1] == b':'
            && (bytes[i + 2] == b'\\' || bytes[i + 2] == b'/')
        {
            // Word boundary before the drive letter (start / space / quote / paren / dirty-*).
            let boundary_ok = i == 0
                || matches!(bytes[i - 1], b' ' | b'"' | b'\'' | b'(' | b'[' | b'*' | b'\t');
            if boundary_ok {
                start = Some(i);
                break;
            }
        }
    }
    let start = start?;
    let rest = &title[start..];
    let end = rest.find(" - ").unwrap_or(rest.len());
    let candidate = rest[..end]
        .trim_end()
        .trim_end_matches(['"', '\'', ')', ']', '*']);
    if candidate.len() < 4 {
        return None;
    }
    Some(candidate.to_string())
}

/// Extract a plausible bare filename from the title's first ` - ` segment, e.g.
/// `"report.docx - Word"` → `report.docx`. Requires an extension and no path
/// separators; rejects anything with characters illegal in Windows filenames.
fn extract_title_filename(title: &str) -> Option<String> {
    let first = title.split(" - ").next()?.trim();
    // Editors mark dirty state with a leading `*`/`●` — strip before matching.
    let first = first.trim_start_matches(['*', '●', ' ']);
    if first.is_empty() || first.len() > 200 {
        return None;
    }
    if first.contains(['\\', '/', ':', '"', '<', '>', '|', '?']) {
        return None;
    }
    let (stem, ext) = first.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() || ext.len() > 5 || !ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    Some(first.to_string())
}

/// Rung 2: `%APPDATA%\Microsoft\Windows\Recent\<filename>.lnk` → shell-link
/// target. Only an exact filename match is accepted (never guess).
#[cfg(windows)]
fn recent_items_lookup(filename: &str) -> Option<String> {
    let appdata = std::env::var_os("APPDATA")?;
    let lnk = Path::new(&appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Recent")
        .join(format!("{filename}.lnk"));
    if !lnk.is_file() {
        return None;
    }
    lnk_target(&lnk)
}

#[cfg(not(windows))]
fn recent_items_lookup(_filename: &str) -> Option<String> {
    None
}

/// Resolve a `.lnk` shortcut's target via `IShellLinkW` (COM). Returns `None`
/// on any COM failure — the ladder just falls to its floor. [VERIFY on-target:
/// first M4 smoke test should confirm Office/Notepad recents resolve.]
#[cfg(windows)]
fn lnk_target(lnk_path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::Interface;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED, STGM_READ,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    unsafe {
        // Balance CoUninitialize only when this call owns/joined the apartment;
        // RPC_E_CHANGED_MODE (already MTA) ⇒ proceed without balancing.
        let init_hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let balance = init_hr.is_ok();
        let result = (|| -> Option<String> {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
            let persist: IPersistFile = link.cast().ok()?;
            let wide: Vec<u16> = lnk_path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            persist
                .Load(windows::core::PCWSTR(wide.as_ptr()), STGM_READ)
                .ok()?;
            let mut buf = [0u16; 520];
            link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            if len == 0 {
                return None;
            }
            Some(String::from_utf16_lossy(&buf[..len]))
        })();
        if balance {
            CoUninitialize();
        }
        result
    }
}

impl Connector for DocumentConnector {
    fn id(&self) -> &'static str {
        "document"
    }

    fn can_capture(&self, ev: &Event) -> bool {
        // document_state events are synthesized only for known editor processes
        // (crate::heuristics), so the type check is the whole predicate.
        matches!(ev.r#type, EventType::DocumentState)
    }

    fn capture(&self, ev: &Event) -> Option<ConnectorState> {
        // The heuristics stage already ran the ladder and only synthesizes
        // document_state when a path resolved — but re-verify here so a
        // hand-crafted event can never store a guess (doc 10 §4).
        let path = ev.payload.get("path").and_then(|v| v.as_str())?;
        if path.is_empty() || !Path::new(path).is_file() {
            return None;
        }
        let title = ev
            .payload
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| ev.window_title.clone())
            .unwrap_or_default();
        let app_hint = ev
            .payload
            .get("app")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| ev.process.clone());
        let payload = DocumentPayloadV1 {
            path: path.to_string(),
            app_hint,
            title,
        };
        Some(crate::build_state(
            self.id(),
            serde_json::to_value(payload).ok()?,
            ev.ts,
            self.staleness_ttl(),
        ))
    }

    fn staleness_ttl(&self) -> Duration {
        // TTL 7 d (doc 10 §4).
        TTL_7D
    }

    fn reconstruct(&self, st: &ConnectorState) -> Result<ResumeArtifact, ConnectorError> {
        let payload: DocumentPayloadV1 = serde_json::from_value(st.reconstruct_payload.clone())
            .map_err(|e| ConnectorError::DispatchFailed(format!("bad document payload: {e}")))?;
        // Existence re-check here (doc 10 §4); deeplinker re-checks too (defense
        // in depth — Path B is a single round trip).
        if !Path::new(&payload.path).is_file() {
            return Err(ConnectorError::TargetGone(payload.path));
        }
        Ok(ResumeArtifact::FileOpen {
            path: payload.path,
            app_hint: payload.app_hint,
        })
    }

    fn open(&self, a: &ResumeArtifact) -> Result<OpenOutcome, ConnectorError> {
        // Default (or hinted) handler; missing-file folder-degrade lives in deeplinker.
        deeplinker::open(a)
    }

    fn validate(&self, cloud_payload: &serde_json::Value) -> Option<ConnectorState> {
        // Gate for Claude-suggested document actions (doc 09 §4, ADR-035):
        // require an absolute, *existing* path — mirrors the never-guess rule.
        let path = cloud_payload.get("path").and_then(|v| v.as_str())?;
        let p = Path::new(path);
        if !p.is_absolute() || !p.is_file() {
            return None;
        }
        let payload = DocumentPayloadV1 {
            path: path.to_string(),
            app_hint: None,
            title: cloud_payload
                .get("title")
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

    fn doc_event(path: &str, title: &str) -> Event {
        Event {
            id: 1,
            ts: 9_000,
            r#type: EventType::DocumentState,
            app: Some("office".into()),
            process: Some("winword.exe".into()),
            window_title: Some(title.to_string()),
            payload: json!({ "path": path, "app": "winword.exe", "title": title }),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        }
    }

    #[test]
    fn extracts_full_paths_from_titles() {
        assert_eq!(
            extract_title_path(r"C:\notes\todo.txt - Notepad"),
            Some(r"C:\notes\todo.txt".to_string())
        );
        assert_eq!(
            extract_title_path(r"*C:\x\draft.md - Notepad++"),
            Some(r"C:\x\draft.md".to_string())
        );
        assert_eq!(extract_title_path("report.docx - Word"), None);
        assert_eq!(extract_title_path("no path here"), None);
    }

    #[test]
    fn extracts_bare_filenames() {
        assert_eq!(
            extract_title_filename("report.docx - Word"),
            Some("report.docx".to_string())
        );
        assert_eq!(
            extract_title_filename("● budget.xlsx - Excel"),
            Some("budget.xlsx".to_string())
        );
        // Not filename-shaped → None (the never-guess rule).
        assert_eq!(extract_title_filename("Inbox - Outlook"), None);
        assert_eq!(extract_title_filename(r"C:\x\y.txt - Notepad"), None);
    }

    #[test]
    fn ladder_rung1_requires_existence() {
        let dir = std::env::temp_dir().join("aperture-doc-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("exists.txt");
        std::fs::write(&file, b"x").unwrap();

        let title = format!("{} - Notepad", file.display());
        let resolved = DocumentConnector::resolve_path_with(&title, None, |_| None);
        assert_eq!(resolved, Some(file.display().to_string()));

        // Same shape, nonexistent file ⇒ the floor (no capture), never a guess.
        let missing_title = r"C:\definitely\not\here-9f2e.txt - Notepad";
        assert_eq!(
            DocumentConnector::resolve_path_with(missing_title, None, |_| None),
            None
        );
    }

    #[test]
    fn ladder_rung2_uses_recent_items_lookup() {
        let dir = std::env::temp_dir().join("aperture-doc-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("report.docx");
        std::fs::write(&file, b"x").unwrap();
        let target = file.display().to_string();

        let resolved = DocumentConnector::resolve_path_with("report.docx - Word", None, |name| {
            (name == "report.docx").then(|| target.clone())
        });
        assert_eq!(resolved, Some(target));
    }

    #[test]
    fn capture_reverifies_and_reconstruct_rechecks_existence() {
        let dir = std::env::temp_dir().join("aperture-doc-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cap.txt");
        std::fs::write(&file, b"x").unwrap();
        let path = file.display().to_string();

        let c = DocumentConnector::new();
        let st = c.capture(&doc_event(&path, "cap.txt - Notepad")).expect("captured");
        match c.reconstruct(&st).unwrap() {
            ResumeArtifact::FileOpen { path: p, app_hint } => {
                assert_eq!(p, path);
                assert_eq!(app_hint.as_deref(), Some("winword.exe"));
            }
            other => panic!("expected FileOpen, got {other:?}"),
        }

        // A hand-crafted event with a bogus path never captures.
        assert!(c.capture(&doc_event(r"C:\nope\gone.txt", "gone.txt - Notepad")).is_none());

        // Deleted between capture and click ⇒ TargetGone (folder degrade upstream).
        std::fs::remove_file(&file).unwrap();
        assert!(matches!(
            c.reconstruct(&st),
            Err(ConnectorError::TargetGone(_))
        ));
    }

    #[test]
    fn validate_requires_existing_absolute_path() {
        let dir = std::env::temp_dir().join("aperture-doc-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("valid.txt");
        std::fs::write(&file, b"x").unwrap();

        let c = DocumentConnector::new();
        assert!(c
            .validate(&json!({ "path": file.display().to_string() }))
            .is_some());
        assert!(c.validate(&json!({ "path": "relative/path.txt" })).is_none());
        assert!(c.validate(&json!({ "path": r"C:\gone\nope.txt" })).is_none());
    }
}
