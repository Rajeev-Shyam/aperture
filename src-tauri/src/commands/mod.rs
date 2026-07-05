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
//!
//! Milestone policy: commands whose subsystems are later milestones return an
//! honest `Err("not built until M<n>")` instead of `todo!()` — a stray invoke
//! must never panic the overlay shell.

use aperture_contracts::suggestions::SuggestionSource;
use aperture_contracts::{BubbleSpec, ContextPayload, Intent, OpenOutcome, StructuredSuggestions};
use tauri::State;
use uuid::Uuid;

use crate::app_state::AppState;
use crate::events::{self, BubbleSpecEnvelope, CaptureIndicator};

/// Toggle capture ON/OFF (doc 02 §7, doc 12 §2, §6).
///
/// Routes to the orchestration `ToggleOwner` — the single writer of capture
/// state. OFF runs the release sequence (capture release; sidecar kill wires at
/// M5, doc 12 §6); the capture subsystem emits the `capture_toggle` audit row.
/// Capture defaults OFF until opt-in (doc 13 §8).
#[tauri::command]
pub async fn toggle_capture(
    on: bool,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    {
        let mut orch = state.orchestration.lock().await;
        if on {
            orch.toggle().turn_on().await;
        } else {
            // The UI shows "releasing…" while the ≤3 s OFF path runs (doc 12 §6).
            let _ = events::emit_capture_indicator(&app, CaptureIndicator::Releasing);
            orch.toggle().turn_off().await;
        }
    }
    let _ = events::emit_capture_indicator(
        &app,
        if on { CaptureIndicator::On } else { CaptureIndicator::Off },
    );
    Ok(on)
}

/// Return the currently-renderable bubbles for the overlay (doc 11 §3).
///
/// Reads the queued/shown suggestion rows; the cap of 3 visible (doc 11 §3,
/// doc 14: ≤2 glass + opaque 3rd, ADR-039) is enforced by the UI — this hands
/// over the live set (queued rows survive a WebView2 respawn in SQLite, doc 11 §7).
#[tauri::command]
pub async fn list_suggestions(
    state: State<'_, AppState>,
) -> Result<Vec<BubbleSpecEnvelope>, String> {
    state
        .db
        .with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, title, glyph, confidence, connector_id \
                 FROM suggestions WHERE state IN ('queued','shown') \
                 ORDER BY shown_ts DESC LIMIT 16",
            )?;
            let rows = stmt.query_map([], |row| {
                let id: i64 = row.get(0)?;
                Ok(BubbleSpecEnvelope {
                    id: id.to_string(),
                    spec: BubbleSpec {
                        title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        glyph: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        sublabel: None,
                        action_ref: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                        source: SuggestionSource::Local,
                        confidence: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                    },
                })
            })?;
            rows.collect()
        })
        .map_err(|e| e.to_string())
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
    // M4: the connector registry (doc 10) resolves + validates-on-click
    // (ADR-035) + dispatches. No connectors exist yet.
    Err("bubble_click: connectors are the M4 milestone (doc 16)".into())
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
    // M7: payload builder (doc 03 §4) + redaction pipeline (doc 13 §5).
    Err("request_preview: the reasoning gateway is the M7 milestone (doc 16)".into())
}

/// The ONLY setter of `ContextPayload::user_approved` (doc 15 §2(b), doc 11 §4).
#[tauri::command]
pub async fn preview_set_approved(
    _payload_id: Uuid,
    _state: State<'_, AppState>,
) -> Result<(), String> {
    Err("preview_set_approved: the reasoning gateway is the M7 milestone (doc 16)".into())
}

/// The ONLY call that reaches the network (doc 15 §2(c), doc 13 §2) — via the
/// gateway, with an already-approved payload, SHA-256 audit-logged (M7).
#[tauri::command]
pub async fn preview_send(
    _payload: ContextPayload,
    _state: State<'_, AppState>,
) -> Result<StructuredSuggestions, String> {
    Err("preview_send: the reasoning gateway is the M7 milestone (doc 16)".into())
}

/// PTT key pressed (doc 07, Path C) — M6.
#[tauri::command]
pub async fn voice_ptt_down(_state: State<'_, AppState>) -> Result<(), String> {
    Err("voice_ptt_down: voice is the M6 milestone (doc 16)".into())
}

/// PTT key released (doc 07, Path C) — M6.
#[tauri::command]
pub async fn voice_ptt_up(_state: State<'_, AppState>) -> Result<(), String> {
    Err("voice_ptt_up: voice is the M6 milestone (doc 16)".into())
}

/// Read the current settings as opaque JSON (doc 13 §6): the `settings` table's
/// key/value rows, merged into one object. (First-run seeding from
/// `config/settings.default.json` + the encrypted store land at M9.)
#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    state
        .db
        .with_conn(|c| {
            let mut stmt = c.prepare("SELECT key, value FROM settings")?;
            let mut obj = serde_json::Map::new();
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (k, v) = row?;
                let parsed = serde_json::from_str(&v).unwrap_or(serde_json::Value::String(v));
                obj.insert(k, parsed);
            }
            Ok(serde_json::Value::Object(obj))
        })
        .map_err(|e| e.to_string())
}

/// Persist settings (doc 13 §6): each top-level key of `patch` upserts one
/// `settings` row. Some changes (e.g. loadout L1<->L2) are applied by
/// orchestration on the next job; the shell only stores them.
#[tauri::command]
pub async fn set_settings(
    patch: serde_json::Value,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let serde_json::Value::Object(map) = patch else {
        return Err("set_settings expects a JSON object".into());
    };
    state
        .db
        .with_conn(|c| {
            for (k, v) in &map {
                c.execute(
                    "INSERT INTO settings (key, value) VALUES (?1, ?2) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    rusqlite::params![k, v.to_string()],
                )?;
            }
            Ok(())
        })
        .map_err(|e| e.to_string())
}
