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
#[derive(Debug, Clone, Serialize)]
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
