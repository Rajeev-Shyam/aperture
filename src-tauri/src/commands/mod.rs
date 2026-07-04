//! The IPC command surface (doc 11 §1, doc 02 §4-§6) — a CONTRACT with the UI
//! agent: these `#[tauri::command]` names + signatures match the WebView's
//! `invoke()` calls exactly. Do not rename without updating the UI in lockstep.
//!
//! Boundary invariants enforced by who-calls-what here:
//! - [`toggle_capture`] is the ONLY entry to capture state and routes to the
//!   orchestration `ToggleOwner` — the single writer (doc 02 §7, doc 12 §2).
//! - [`preview_set_approved`] is the ONLY setter of `ContextPayload::user_approved`
//!   (doc 15 §2(b)); [`preview_send`] is the ONLY call that reaches the network,
//!   and only with an already-approved payload (doc 15 §2(c), doc 13 §2).
//! - [`bubble_click`] is Critical Path B (doc 02 §5): resolve `action_ref` ->
//!   connector -> reconstruct -> open, target < 200 ms.

use aperture_contracts::{ContextPayload, Intent, OpenOutcome, StructuredSuggestions};
use tauri::State;
use uuid::Uuid;

use crate::app_state::AppState;
use crate::events::BubbleSpecEnvelope;

/// Toggle capture ON/OFF (doc 02 §7, doc 12 §2, §6).
///
/// Routes to the orchestration `ToggleOwner` — the single writer of capture
/// state. OFF runs the end-to-end release sequence (capture release + sidecar
/// kill, VRAM -> ~0 in < 3 s, doc 12 §6) and emits the `capture_toggle{off}`
/// audit event. Capture defaults OFF until opt-in (doc 13 §8).
#[tauri::command]
pub async fn toggle_capture(
    _on: bool,
    _state: State<'_, AppState>,
) -> Result<bool, String> {
    // TODO(M1:) state.toggle_owner.set(on) -> broadcast capture state;
    //   on OFF, drive the <3 s SLA (doc 12 §6) and emit CaptureIndicator::Releasing
    //   then Off via events::emit_capture_indicator. Return the new capturing state
    //   (the UI also reconciles via the capture_indicator event).
    todo!("M1: route to orchestration::ToggleOwner (single writer); OFF release SLA")
}

/// Return the currently-renderable bubbles for the overlay (doc 11 §3).
///
/// Reads the queued/idle `BubbleSpec`s; the cap of 3 visible (doc 11 §3, doc 14)
/// is enforced by the UI, this just hands over the live set (queued survive a
/// WebView2 respawn in SQLite, doc 11 §7).
#[tauri::command]
pub async fn list_suggestions(
    _state: State<'_, AppState>,
) -> Result<Vec<BubbleSpecEnvelope>, String> {
    // TODO(M3:) read live suggestions from the suggestion generator / DB and wrap
    //   each as { id, spec } so the UI can key lifecycle events back to a bubble.
    todo!("M3: return live {{ id, spec }} envelopes (UI enforces the 3-visible cap, doc 11 §3)")
}

/// Critical Path B (doc 02 §5): resolve `action_ref` -> `connector_id`, load the
/// `connector_state` from SQLite, `reconstruct()` the artifact, `open()` it via
/// `ShellExecuteW`/protocol handler. Target < 200 ms. Records
/// `suggestion_clicked{outcome}`. Failure degrades honestly (doc 10 §6).
#[tauri::command]
pub async fn bubble_click(
    _id: String,
    _action_ref: String,
    _state: State<'_, AppState>,
) -> Result<OpenOutcome, String> {
    // TODO(M4:) resolve action_ref -> connector; reconstruct + open (Critical
    //   Path B); write SuggestionClicked{outcome} keyed by the bubble `id`
    //   (doc 02 §5, doc 10). Return the OpenOutcome so the UI can honest-degrade.
    todo!("M4: Critical Path B resume (<200ms); record suggestion_clicked{{outcome}}")
}

/// Build + redact the Context Payload for an intent and return the EXACT wire
/// object the preview renders (doc 03 §4, doc 11 §4, doc 13 §5).
///
/// "Preview == wire": this returns the single object that will later be sent
/// byte-for-byte. It does NOT set `user_approved` (that is
/// [`preview_set_approved`] only) and does NOT touch the network.
#[tauri::command]
pub async fn request_preview(
    _intent: Intent,
    // Optional originating bubble/answer `action_ref` so the preview seeds its
    // connector item from the same context the user clicked (doc 11 §4-§5).
    _seed_action_ref: Option<String>,
    _state: State<'_, AppState>,
) -> Result<ContextPayload, String> {
    // TODO(M7:) payload builder (doc 03 §4) -> redaction pipeline (doc 13 §5):
    //   assemble items (seeded from seed_action_ref if present), cap event_trail
    //   at 50, apply ordered redaction rules with hit counts, set transport_target
    //   from settings. Return the wire object.
    todo!("M7: build + redact the ContextPayload; preview==wire (doc 03 §4, doc 13 §5)")
}

/// The ONLY setter of `ContextPayload::user_approved` (doc 15 §2(b), doc 11 §4).
///
/// Marks the previewed payload approved-for-send after the user clicks Send in
/// the trust surface. The gateway refuses any payload this did not flip.
#[tauri::command]
pub async fn preview_set_approved(
    _payload_id: Uuid,
    _state: State<'_, AppState>,
) -> Result<(), String> {
    // TODO(M7:) flip user_approved=true on the in-process payload keyed by
    //   payload_id; this is the sole writer of that flag (doc 15 §2(b)).
    todo!("M7: set user_approved=true (the ONLY setter, doc 15 §2(b))")
}

/// The ONLY call that reaches the network (doc 15 §2(c), doc 13 §2).
///
/// Hands the already-approved payload to the reasoning gateway, which picks the
/// first healthy transport, transmits the EXACT serialized bytes (SHA-256
/// audit-logged as `cloud_send`, doc 03 §4 / doc 13 §3), and returns
/// source-agnostic [`StructuredSuggestions`]. Rejects an unapproved payload.
#[tauri::command]
pub async fn preview_send(
    _payload: ContextPayload,
    _state: State<'_, AppState>,
) -> Result<StructuredSuggestions, String> {
    // TODO(M7:) assert payload.user_approved (else reject); state.gateway.send(&payload).
    //   The gateway is the sole network/Claude-CLI emitter (doc 13 §2) and the
    //   sole consumer of an approved payload (doc 15 §2(c)). Hash-log the wire bytes.
    todo!("M7: the ONLY network path — gateway.send on an approved payload (doc 13 §2)")
}

/// PTT key pressed (doc 07, Path C). Start WASAPI capture while held; surface the
/// listening pill (doc 11 §5). STT runs later as a priority-100 GPU job (doc 12 §3).
#[tauri::command]
pub async fn voice_ptt_down(_state: State<'_, AppState>) -> Result<(), String> {
    // TODO(M6:) start voice capture (doc 07); emit_voice_surface(listening pill).
    todo!("M6: PTT down -> start WASAPI capture, listening pill (doc 07, doc 11 §5)")
}

/// PTT key released (doc 07, Path C). Stop capture, VAD-trim, enqueue the STT job
/// (priority 100, never cancellable, doc 12 §3); transcript -> answer surface.
#[tauri::command]
pub async fn voice_ptt_up(_state: State<'_, AppState>) -> Result<(), String> {
    // TODO(M6:) stop capture + VAD trim; STT GpuJob (priority STT_VOICE=100);
    //   store voice_utterance; route query intent to retrieval (doc 07, doc 02 §6).
    todo!("M6: PTT up -> STT job (priority 100) -> transcript/answer (doc 07)")
}

/// Read the current settings as opaque JSON (doc 13 §6). At runtime settings live
/// inside the encrypted DB; this returns the merged effective view for the UI.
#[tauri::command]
pub async fn get_settings(_state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    // TODO(M0:) read settings from the encrypted DB (seeded from config/settings.default.json).
    todo!("M0: return effective settings JSON (doc 13 §6)")
}

/// Persist settings (doc 13 §6). Some changes (e.g. loadout L1<->L2) are applied
/// by orchestration on the next job; the shell only stores them.
#[tauri::command]
pub async fn set_settings(
    _patch: serde_json::Value,
    _state: State<'_, AppState>,
) -> Result<(), String> {
    // TODO(M0:) validate + persist settings into the encrypted DB; notify
    //   orchestration of loadout/timer changes (doc 12 §7).
    todo!("M0: persist settings to the encrypted DB (doc 13 §6)")
}
