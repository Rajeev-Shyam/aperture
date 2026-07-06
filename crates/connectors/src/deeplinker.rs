//! Dispatch layer for Critical Path B (doc 10 §6 / doc 02 §5).
//!
//! [`open`] turns a [`ResumeArtifact`] into an OS launch. All three variants go
//! through `ShellExecuteW`, with one extra rung for IDE protocol URIs (a `code -g`
//! CLI fallback when the `vscode://` handler is unregistered, doc 10 §5).
//!
//! **Budget:** Path B is the click→resume path with a **< 200 ms** ceiling
//! (doc 02 §5). `ShellExecuteW` returns as soon as the launch is *handed to the
//! shell*, not when the target app finishes opening, so the dispatch itself is
//! cheap; the budget mostly polices the file-existence pre-check and the CLI
//! fallback. We never block on the launched process.
//!
//! **Graceful degrade (doc 10 §6):** if the target is gone (file deleted, handler
//! unregistered) we return [`ConnectorError`] / [`OpenOutcome::Failed`] so the
//! Bubble UI can swap to fallback copy and record `suggestion_clicked{outcome:
//! failed_fallback}` for SC7 — we never panic and never leave a half-launch.
//!
//! **Transparency gate (invariant 2):** `ShellExecuteW` is an OS shell call, not
//! a network socket. This module opens no sockets and spawns no Claude CLI; only
//! `aperture-reasoning-gateway` may do that. (The `code -g` rung also goes
//! through `ShellExecuteW`, deliberately — no process-spawn API in this crate.)

use std::path::Path;
use std::time::Duration;

use aperture_contracts::{OpenOutcome, ResumeArtifact};

use aperture_contracts::connector::ConnectorError;

/// The Path B latency ceiling (doc 02 §5). Used to bound the file pre-check and
/// the `code -g` fallback; the bare `ShellExecuteW` launch is well under it.
pub const PATH_B_BUDGET: Duration = Duration::from_millis(200);

/// Low-level dispatch failure, kept distinct so the protocol-URI ladder can
/// tell "no handler registered" (fall a rung) from any other failure (stop).
#[derive(Debug)]
enum ShellError {
    /// `SE_ERR_NOASSOC` / `SE_ERR_ASSOCINCOMPLETE` — nothing registered.
    NoAssoc,
    Other(String),
}

/// Dispatch a reconstructed artifact to the OS (doc 10 §6).
///
/// * [`ResumeArtifact::Url`] → `ShellExecuteW("open", url)` → default browser
///   (a *new tab*, framed honestly as "Reopen page", doc 10 §2).
/// * [`ResumeArtifact::ProtocolUri`] → `ShellExecuteW` on the URI (e.g.
///   `vscode://file/…`); on `HandlerUnregistered`, fall one rung to the
///   `code -g` CLI, then to a plain file open (doc 10 §5).
/// * [`ResumeArtifact::FileOpen`] → exists-check first; then `ShellExecuteW` via
///   the default (or hinted) handler. Missing file degrades to the containing
///   folder (doc 10 §4).
pub fn open(artifact: &ResumeArtifact) -> Result<OpenOutcome, ConnectorError> {
    match artifact {
        ResumeArtifact::Url(url) => open_url(url),
        ResumeArtifact::ProtocolUri(uri) => open_protocol_uri(uri),
        ResumeArtifact::FileOpen { path, app_hint } => open_file(path, app_hint.as_deref()),
    }
}

/// `ShellExecuteW`-launch a plain URL in the default browser (doc 10 §2).
fn open_url(url: &str) -> Result<OpenOutcome, ConnectorError> {
    match shell_execute(url, None) {
        Ok(()) => Ok(OpenOutcome::Resumed),
        Err(ShellError::NoAssoc) => Err(ConnectorError::HandlerUnregistered(url.to_string())),
        Err(ShellError::Other(e)) => Err(ConnectorError::DispatchFailed(e)),
    }
}

/// Dispatch a protocol URI, with the IDE ladder's CLI fallback (doc 10 §5).
fn open_protocol_uri(uri: &str) -> Result<OpenOutcome, ConnectorError> {
    match shell_execute(uri, None) {
        Ok(()) => return Ok(OpenOutcome::Resumed),
        Err(ShellError::Other(e)) => return Err(ConnectorError::DispatchFailed(e)),
        Err(ShellError::NoAssoc) => {}
    }
    // Rung 2 (vscode:// only): the `code -g <path>:<line>[:<col>]` CLI, launched
    // via ShellExecuteW so this crate never touches a process-spawn API.
    // [VERIFY] `code` on PATH — the installer's "Add to PATH" default.
    if let Some((path, goto_arg)) = parse_vscode_file_uri(uri) {
        for cli in ["code.cmd", "code"] {
            if shell_execute(cli, Some(&format!("-g \"{goto_arg}\""))).is_ok() {
                return Ok(OpenOutcome::Degraded {
                    reason: "vscode:// handler unregistered; opened via `code -g`".into(),
                });
            }
        }
        // Rung 3: plain file open (loses cursor position — still honest progress).
        if Path::new(&path).is_file() && shell_execute(&path, None).is_ok() {
            return Ok(OpenOutcome::Degraded {
                reason: "opened file without cursor position".into(),
            });
        }
    }
    Err(ConnectorError::HandlerUnregistered(uri.to_string()))
}

/// Open a file via its default (or hinted) handler, after an existence check
/// (doc 10 §4). Missing file ⇒ degrade to the containing folder.
fn open_file(path: &str, _app_hint: Option<&str>) -> Result<OpenOutcome, ConnectorError> {
    let p = Path::new(path);
    if !p.is_file() {
        // Degrade: offer the containing folder (doc 10 §4). If even the folder
        // is gone, the target is truly unreachable.
        if let Some(parent) = p.parent().filter(|d| d.is_dir()) {
            let parent_str = parent.display().to_string();
            if shell_execute(&parent_str, None).is_ok() {
                return Ok(OpenOutcome::Degraded {
                    reason: "file moved or deleted; opened its folder".into(),
                });
            }
        }
        return Err(ConnectorError::TargetGone(path.to_string()));
    }
    // `app_hint` is honored only when the user opted into "open with same app"
    // and it differs from the default handler (doc 10 §4 [ASSUMPTION]) — that
    // opt-in doesn't exist yet, so the default handler is always used.
    match shell_execute(path, None) {
        Ok(()) => Ok(OpenOutcome::Resumed),
        Err(ShellError::NoAssoc) => Err(ConnectorError::HandlerUnregistered(path.to_string())),
        Err(ShellError::Other(e)) => Err(ConnectorError::DispatchFailed(e)),
    }
}

/// Parse a `vscode://file/<abs>:<line>[:<col>]` URI back into
/// `(abs_path, "path[:line[:col]]")` for the `code -g` rung.
fn parse_vscode_file_uri(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix("vscode://file/")?;
    let decoded = rest
        .replace("%20", " ")
        .replace("%23", "#")
        .replace("%3F", "?")
        .replace("%25", "%");
    // Split trailing :line[:col] — numeric-only segments after the last path chars.
    let mut path = decoded.as_str();
    let mut line_col = String::new();
    for _ in 0..2 {
        if let Some((head, tail)) = path.rsplit_once(':') {
            if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
                line_col = if line_col.is_empty() {
                    format!(":{tail}")
                } else {
                    format!(":{tail}{line_col}")
                };
                path = head;
                continue;
            }
        }
        break;
    }
    Some((path.to_string(), format!("{path}{line_col}")))
}

/// The one dispatch primitive: `ShellExecuteW("open", file, params)`.
/// HINSTANCE > 32 ⇒ success (per the API contract); `SE_ERR_NOASSOC` (31) /
/// `SE_ERR_ASSOCINCOMPLETE` (27) ⇒ [`ShellError::NoAssoc`].
#[cfg(windows)]
fn shell_execute(file: &str, params: Option<&str>) -> Result<(), ShellError> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    const SE_ERR_ASSOCINCOMPLETE: isize = 27;
    const SE_ERR_NOASSOC: isize = 31;

    let file_w = HSTRING::from(file);
    let params_w = params.map(HSTRING::from);
    let hinst = unsafe {
        ShellExecuteW(
            HWND::default(),
            &HSTRING::from("open"),
            &file_w,
            params_w
                .as_ref()
                .map(|p| PCWSTR(p.as_ptr()))
                .unwrap_or(PCWSTR::null()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = hinst.0 as isize;
    if code > 32 {
        Ok(())
    } else if code == SE_ERR_NOASSOC || code == SE_ERR_ASSOCINCOMPLETE {
        Err(ShellError::NoAssoc)
    } else {
        Err(ShellError::Other(format!(
            "ShellExecuteW({file}) failed with code {code}"
        )))
    }
}

#[cfg(not(windows))]
fn shell_execute(file: &str, _params: Option<&str>) -> Result<(), ShellError> {
    Err(ShellError::Other(format!(
        "ShellExecuteW unavailable on this platform (would open {file})"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vscode_file_uris_for_the_cli_rung() {
        let (path, goto) = parse_vscode_file_uri("vscode://file/C:/p/x.rs:120:5").unwrap();
        assert_eq!(path, "C:/p/x.rs");
        assert_eq!(goto, "C:/p/x.rs:120:5");

        let (path, goto) = parse_vscode_file_uri("vscode://file/C:/my%20dir/x.rs:12").unwrap();
        assert_eq!(path, "C:/my dir/x.rs");
        assert_eq!(goto, "C:/my dir/x.rs:12");

        let (path, goto) = parse_vscode_file_uri("vscode://file/C:/p/x.rs").unwrap();
        assert_eq!(path, "C:/p/x.rs");
        assert_eq!(goto, "C:/p/x.rs");

        assert!(parse_vscode_file_uri("jetbrains://open?file=x").is_none());
    }

    #[test]
    fn open_file_missing_target_with_missing_folder_is_target_gone() {
        // Both the file and its folder are gone ⇒ TargetGone, never a dispatch.
        let r = open_file(r"C:\definitely\not\a\real\dir-9f2e\file.txt", None);
        assert!(matches!(r, Err(ConnectorError::TargetGone(_))));
    }
}
