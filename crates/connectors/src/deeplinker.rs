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
//! `aperture-reasoning-gateway` may do that.

use std::path::Path;
use std::time::Duration;

use aperture_contracts::{OpenOutcome, ResumeArtifact};

use aperture_contracts::connector::ConnectorError;

/// The Path B latency ceiling (doc 02 §5). Used to bound the file pre-check and
/// the `code -g` fallback; the bare `ShellExecuteW` launch is well under it.
pub const PATH_B_BUDGET: Duration = Duration::from_millis(200);

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
fn open_url(_url: &str) -> Result<OpenOutcome, ConnectorError> {
    // TODO(M4): call `ShellExecuteW(None, "open", url, None, None, SW_SHOWNORMAL)`
    //   via the `windows` crate; HINSTANCE <= 32 ⇒ DispatchFailed. Returns
    //   `Resumed` (the URL itself was exact; YouTube position degrade is decided
    //   upstream in reconstruct, not here).
    todo!("M4: ShellExecuteW(open, url) → default browser; map HINSTANCE<=32 to DispatchFailed")
}

/// Dispatch a protocol URI, with the IDE ladder's CLI fallback (doc 10 §5).
fn open_protocol_uri(_uri: &str) -> Result<OpenOutcome, ConnectorError> {
    // TODO(M4): ladder per doc 10 §5/§6:
    //   1. ShellExecuteW(uri) — registered handler (e.g. `vscode://`).
    //   2. on HandlerUnregistered (HINSTANCE == SE_ERR_NOASSOC) ⇒ try the
    //      `code -g <path>:<line>:<col>` CLI fallback (parse the URI back out, or
    //      have the IDE connector pass the parts). [VERIFY] `code` on PATH.
    //   3. final rung: plain file open of the abs path.
    //   Each successful rung past the first returns Degraded{reason}; total work
    //   stays within PATH_B_BUDGET (don't block on the spawned `code` process).
    todo!("M4: protocol-URI dispatch with `code -g` CLI fallback then plain open")
}

/// Open a file via its default (or hinted) handler, after an existence check
/// (doc 10 §4). Missing file ⇒ degrade to the containing folder.
fn open_file(path: &str, _app_hint: Option<&str>) -> Result<OpenOutcome, ConnectorError> {
    // Cheap pre-check keeps us honest and inside the budget: never ShellExecute a
    // path we already know is gone (doc 10 §4, §6).
    if !Path::new(path).exists() {
        // TODO(M4): degrade — ShellExecuteW the containing folder and return
        //   Degraded{reason:"file moved/deleted; opened folder"}; if even the
        //   folder is gone, Err(TargetGone).
        return Err(ConnectorError::TargetGone(path.to_string()));
    }
    // TODO(M4): ShellExecuteW("open", path); honor `app_hint` only when the user
    //   opted into "open with same app" and it differs from the default
    //   (doc 10 §4 [ASSUMPTION]). HINSTANCE <= 32 ⇒ DispatchFailed.
    todo!("M4: ShellExecuteW(open, path) via default/hinted handler → Resumed")
}
