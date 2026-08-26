//! Core -> WebView event channels (doc 11 §1, §6; doc 02 §1).
//!
//! These names are a CONTRACT shared with the UI agent: the WebView listens on
//! the exact same string keys. The Tauri event channel bridges the in-process
//! `tokio::broadcast` bus (doc 15 §1) to the overlay's JS `listen()` handlers.
//!
//! Honors the degrade-under-load contract (doc 14, wired here): `gpu_busy`
//! drives the glass <-> opaque-fallback swap in the overlay (doc 11 §6).

// Contract surface: several channels/emitters are consumed by later milestones
// (gpu_busy forwarder at M5, voice surfaces at M6, lifecycle at M3-UI) — kept
// warning-free so the UI-facing names never churn.
#![allow(dead_code)]

use serde::Serialize;
use tauri::{AppHandle, Emitter};

// ---------------------------------------------------------------------------
// Channel name constants — MUST match the UI agent's `listen()` keys exactly.
// ---------------------------------------------------------------------------

/// A `BubbleSpec` to render/queue in the overlay (doc 08 §6 -> doc 11 §3).
pub const BUBBLE_SPEC: &str = "bubble_spec";
/// GPU mutex held? Drives glass<->opaque swap + animation simplification (doc 11 §6).
pub const GPU_BUSY: &str = "gpu_busy";
/// Capture indicator state for the overlay dot + tray (doc 12 §6).
pub const CAPTURE_INDICATOR: &str = "capture_indicator";
/// Voice UI surface: listening pill / transcript chip / answer bubble (doc 07, doc 11 §5).
pub const VOICE_SURFACE: &str = "voice_surface";
/// Bubble lifecycle transition (queued/entering/idle/clicked/dismissed/expired, doc 11 §3).
pub const SUGGESTION_LIFECYCLE: &str = "suggestion_lifecycle";
/// Open the Dashboard on the overlay window named in the payload's `target`
/// (tray click, second app launch, HUD button). Broadcast with a target
/// (decision #13): the named window opens, every OTHER window closes its copy
/// — controls follow the cursor and exactly one instance exists at a time.
pub const DASHBOARD_OPEN: &str = "dashboard_open";
/// Ask ONE overlay to open the Context-Preview panel for a payload the core
/// staged (the MCP gated-search flow, ADR-037). Carries the full payload.
/// Targeted (`emit_to`) at the window already hosting a preview panel — so the
/// request queues there, never clobbering a mid-edit review — else at the
/// cursor's monitor (decision #13).
pub const PREVIEW_REQUEST: &str = "preview_request";
/// Open the Activity & Privacy panel on the payload's `target` window (bubble
/// overflow "Exclusions…", HUD button). Broadcast with a target, same
/// convergence contract as [`DASHBOARD_OPEN`].
pub const PRIVACY_OPEN: &str = "privacy_open";
/// A window's Context-Preview panel just opened (`set_preview_host`): every
/// other window folds its own panel — cancelling its sessions core-side — so
/// exactly one preview exists across monitors (decision #13; the 08-15
/// dismissal-convergence pattern).
pub const PREVIEW_CLAIMED: &str = "preview_claimed";
/// An audit-trail write failed (decision #41): what egressed and what the
/// `cloud_send` trail records now disagree — the overlay shows a dismissible
/// warning banner, because the trail is the sole answer to "what left this
/// machine?" (doc 13 §3).
pub const AUDIT_ALERT: &str = "audit_alert";
/// VLM weight download progress/terminal state (decision #30): the Dashboard's
/// user-initiated ~3.3 GB fetch streams `{ phase, file, bytes }` here.
pub const VLM_FETCH: &str = "vlm_fetch";
/// A bubble's 👍/👎 was recorded (decision #10). Broadcast so the pressed thumb
/// lights on EVERY monitor's copy of that bubble — the rating is a one-shot
/// signal and its only receipt is the visibly-set thumb, which was appearing on
/// the window the click landed in and nowhere else.
pub const SUGGESTION_RATED: &str = "suggestion_rated";
/// Settings were written (`set_settings`). Broadcast so every surface that
/// CACHED a settings value re-reads it instead of waiting for a restart
/// (decisions #7, #17, #39). Carries the top-level section names in the patch,
/// so a listener can ignore writes it does not care about.
pub const SETTINGS_CHANGED: &str = "settings_changed";
/// The live suggestion set may have changed without a `bubble_spec` emit —
/// a snooze lifted (SDLC review 2026-08-19, finding 2). Rows mined while
/// snoozed queue in SQLite (ADR-040/Q95: only EMISSION is silenced); this tells
/// the overlay to re-run `list_suggestions`, which is what actually surfaces
/// them. Without it a queued row waited for the next WebView remount.
pub const SUGGESTIONS_REFRESH: &str = "suggestions_refresh";

/// The capture-indicator state the overlay/tray render (doc 12 §6).
/// `Releasing` covers the <3 s toggle-OFF window (doc 12 §6 step 5). Internal
/// state; the wire payload the overlay consumes is [`CaptureIndicatorPayload`].
#[derive(Debug, Clone, Copy)]
pub enum CaptureIndicator {
    Off,
    On,
    Releasing,
}

/// The `capture_indicator` wire payload (matches the UI's `CaptureIndicatorEvent`):
/// a boolean plus an optional one-line status (e.g. the releasing detail).
/// Deserialize: the tray mirrors this same channel (`tray.rs`) so the menu's
/// capture checkmark can never disagree with the overlay dot.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct CaptureIndicatorPayload {
    pub capturing: bool,
    pub detail: Option<String>,
}

/// The `bubble_spec` wire envelope (matches the UI's `BubbleSpecEvent`): a stable
/// instance `id` (the `suggestions` row id, stringified) plus the renderable
/// [`aperture_contracts::BubbleSpec`]. The id lets the UI tie
/// `suggestion_lifecycle` transitions back to a specific bubble (doc 11 §3).
#[derive(Debug, Clone, Serialize)]
pub struct BubbleSpecEnvelope {
    pub id: String,
    pub spec: aperture_contracts::BubbleSpec,
}

// ---------------------------------------------------------------------------
// Emit helpers — the only sanctioned way the core pushes to the WebView.
// Each forwards a typed payload onto the matching channel constant.
// ---------------------------------------------------------------------------

/// Push a `BubbleSpec` to the overlay as an `{ id, spec }` envelope (Critical
/// Path A step 8, doc 02 §4). `id` is the suggestion instance id the UI echoes
/// back in `suggestion_lifecycle`.
pub fn emit_bubble_spec(
    app: &AppHandle,
    id: &str,
    spec: &aperture_contracts::BubbleSpec,
) -> tauri::Result<()> {
    app.emit(
        BUBBLE_SPEC,
        BubbleSpecEnvelope { id: id.to_string(), spec: spec.clone() },
    )
}

/// Broadcast the mutex-derived `gpu_busy` observable (doc 11 §6, doc 12 §3).
/// The overlay swaps glass surfaces to the opaque fallback class while `true`.
pub fn emit_gpu_busy(app: &AppHandle, busy: bool) -> tauri::Result<()> {
    app.emit(GPU_BUSY, busy)
}

/// Update the capture indicator (tray + overlay dot, doc 12 §6), mapping the
/// internal 3-state enum to the `{ capturing, detail }` payload the UI consumes.
pub fn emit_capture_indicator(app: &AppHandle, state: CaptureIndicator) -> tauri::Result<()> {
    let payload = match state {
        CaptureIndicator::Off => CaptureIndicatorPayload { capturing: false, detail: None },
        CaptureIndicator::On => CaptureIndicatorPayload { capturing: true, detail: None },
        CaptureIndicator::Releasing => CaptureIndicatorPayload {
            capturing: false,
            detail: Some("releasing… sidecars down".to_string()),
        },
    };
    app.emit(CAPTURE_INDICATOR, payload)
}

/// Surface a voice UI state to the overlay (doc 07, doc 11 §5). `payload` is a
/// source-agnostic JSON value the UI agent's voice components consume.
pub fn emit_voice_surface(app: &AppHandle, payload: &serde_json::Value) -> tauri::Result<()> {
    app.emit(VOICE_SURFACE, payload)
}

/// The `dashboard_open` / `privacy_open` / `preview_claimed` wire payload
/// (matches the UI's `ControlOpenEvent`): which window the surface belongs to.
#[derive(Debug, Clone, Serialize)]
pub struct ControlOpenPayload {
    pub target: String,
}

/// Open the Dashboard on the cursor's monitor (decision #13) — tray left-click
/// / menu item, or a second instance launch handing off to the running one.
/// The cursor is where the user just acted, so the surface lands with them.
pub fn emit_dashboard_open(app: &AppHandle) -> tauri::Result<()> {
    emit_dashboard_open_on(app, &crate::overlay::cursor_overlay_label(app))
}

/// Open the Dashboard on a specific overlay window — used by the `open_dashboard`
/// command, where the calling window IS the cursor's monitor (the click proves
/// it). Broadcast: every non-target window closes its copy.
pub fn emit_dashboard_open_on(app: &AppHandle, target: &str) -> tauri::Result<()> {
    app.emit(DASHBOARD_OPEN, ControlOpenPayload { target: target.to_string() })
}

/// Ask ONE overlay to open the preview panel on a core-staged payload (MCP
/// gated search, ADR-037): the user must SEE what Claude asked for before
/// anything can be approved, and approval releases it via `aperture_get_context`.
///
/// Routing (decision #13): the window already hosting a preview panel, if any
/// — the request must QUEUE behind a possibly-mid-edit review, never open a
/// second panel elsewhere — otherwise the cursor's monitor, falling back to
/// the primary.
pub fn emit_preview_request(
    app: &AppHandle,
    payload: &aperture_contracts::ContextPayload,
) -> tauri::Result<()> {
    use tauri::Manager;
    let host = app
        .try_state::<crate::overlay::PreviewHost>()
        .and_then(|h| h.get())
        .filter(|label| app.get_webview_window(label).is_some());
    let target = host.unwrap_or_else(|| crate::overlay::cursor_overlay_label(app));
    app.emit_to(&target, PREVIEW_REQUEST, payload)
}

/// Open the Activity & Privacy panel on a specific overlay window (exclusions
/// manager — bubble overflow "Exclusions…", HUD button, dashboard link; the
/// calling window is the cursor's monitor). Broadcast, same convergence
/// contract as [`emit_dashboard_open_on`].
pub fn emit_privacy_open_on(app: &AppHandle, target: &str) -> tauri::Result<()> {
    app.emit(PRIVACY_OPEN, ControlOpenPayload { target: target.to_string() })
}

/// Announce that `target`'s preview panel just opened (decision #13): every
/// other window cancels + closes its own panel so exactly one exists.
pub fn emit_preview_claimed(app: &AppHandle, target: &str) -> tauri::Result<()> {
    app.emit(PREVIEW_CLAIMED, ControlOpenPayload { target: target.to_string() })
}

/// The `audit_alert` wire payload (matches the UI's `AuditAlertEvent`).
#[derive(Debug, Clone, Serialize)]
pub struct AuditAlertPayload {
    pub message: String,
}

/// Warn that an audit write failed (decision #41). Broadcast; only the primary
/// overlay renders the banner (App.tsx) — same split as the HUD, so one failure
/// never yields one banner per monitor.
pub fn emit_audit_alert(app: &AppHandle, message: &str) -> tauri::Result<()> {
    app.emit(AUDIT_ALERT, AuditAlertPayload { message: message.to_string() })
}

/// The `vlm_fetch` wire payload (matches the UI's `VlmFetchEvent`). `phase` is
/// `"downloading" | "done" | "error"`; byte counts are OVERALL across both
/// weight files so the Dashboard renders one progress bar.
#[derive(Debug, Clone, Serialize)]
pub struct VlmFetchPayload {
    pub phase: String,
    /// The artifact currently downloading (label under the bar).
    pub file: Option<String>,
    pub received_bytes: u64,
    pub total_bytes: u64,
    pub error: Option<String>,
}

/// Stream VLM download progress / the terminal state to the Dashboard
/// (decision #30). Broadcast; only the primary overlay renders the Dashboard,
/// same split as `audit_alert`.
pub fn emit_vlm_fetch(app: &AppHandle, payload: &VlmFetchPayload) -> tauri::Result<()> {
    app.emit(VLM_FETCH, payload.clone())
}

/// The `suggestion_rated` wire payload (matches the UI's `SuggestionRatedEvent`).
#[derive(Debug, Clone, Serialize)]
pub struct SuggestionRatedPayload {
    pub id: String,
    /// `"up"` | `"down"`.
    pub rating: String,
}

/// Broadcast a recorded 👍/👎 so every overlay window sets the same thumb
/// (decision #10). Emitted only when the rating actually CHANGED — the caller
/// (`record_feedback`) drops duplicates before reaching here, so this can never
/// re-announce a no-op.
pub fn emit_suggestion_rated(app: &AppHandle, id: &str, rating: &str) -> tauri::Result<()> {
    app.emit(
        SUGGESTION_RATED,
        SuggestionRatedPayload { id: id.to_string(), rating: rating.to_string() },
    )
}

/// The `settings_changed` wire payload (matches the UI's `SettingsChangedEvent`).
#[derive(Debug, Clone, Serialize)]
pub struct SettingsChangedPayload {
    /// Top-level sections in the write, e.g. `["ui"]` / `["reasoning"]`.
    pub sections: Vec<String>,
}

/// Announce a settings write to every window (decisions #7, #17, #39).
///
/// Cached-at-mount settings were the reason a Dashboard control could only ever
/// be a next-launch preference: the overlay read `ui.bubble_dwell_sec` once and
/// the pattern task re-read its block on a 24-hour tick. This makes a write
/// observable, so "changes it at runtime" is literally true.
pub fn emit_settings_changed(app: &AppHandle, sections: Vec<String>) -> tauri::Result<()> {
    app.emit(SETTINGS_CHANGED, SettingsChangedPayload { sections })
}

/// The `suggestions_refresh` wire payload (matches the UI's
/// `SuggestionsRefreshEvent`). `reason` is `"snooze_off"` (the user turned
/// snooze off) or `"snooze_expired"` (a timed snooze ran out).
#[derive(Debug, Clone, Serialize)]
pub struct SuggestionsRefreshPayload {
    pub reason: String,
}

/// Ask every overlay window to re-fetch `list_suggestions` (review finding 2).
/// Broadcast: each monitor's container re-runs the same read, and the
/// admission/cap logic converges them exactly as on mount.
pub fn emit_suggestions_refresh(app: &AppHandle, reason: &str) -> tauri::Result<()> {
    app.emit(SUGGESTIONS_REFRESH, SuggestionsRefreshPayload { reason: reason.to_string() })
}

/// Emit a suggestion-lifecycle transition (doc 11 §3). The matching
/// `suggestion_*` event is written to SQLite by the lifecycle owner, not here.
pub fn emit_suggestion_lifecycle(
    app: &AppHandle,
    payload: &serde_json::Value,
) -> tauri::Result<()> {
    app.emit(SUGGESTION_LIFECYCLE, payload)
}

// TODO(M3:) spawn the bus->WebView forwarder task in main.rs setup: subscribe to
// the orchestration `gpu_busy` broadcast (doc 12) and to suggestion-generator
// `BubbleSpec` output, fanning each onto the channels above.

/// v2 agent task view (Doc 22 §9, decision #53) — broadcast on every change;
/// `null` payload means no task exists (the surface unmounts).
pub const AGENT_TASK: &str = "agent_task";

/// Emit the current agent task view to every window.
pub fn emit_agent_task(app: &AppHandle, view: Option<&crate::agent::TaskView>) -> tauri::Result<()> {
    app.emit(AGENT_TASK, view)
}
