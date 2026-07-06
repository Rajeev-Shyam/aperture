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

use std::sync::Arc;

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
///
/// The indicator is emitted from the OBSERVED outcome by the capture driver
/// (`main::spawn_capture_driver`) — never from the requested state here
/// (doc 13 §8: "the indicator is always truthful"). Only the transitional
/// "releasing…" hint is emitted eagerly.
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
    Ok(on)
}

/// Record bubble feedback (doc 08 §7, ADR-040/Q81): update the durable
/// suggestions row (state/resolved_ts/useful_rating — dismissed bubbles must
/// not resurrect on a WebView respawn) and forward the signal to the pattern
/// engine's decay/mute ladder. `kind`: "clicked" | "dismissed" | "expired" |
/// "up" | "down". The 👍/👎 affordance renders at the next UI pass (doc 11 §3);
/// this seam already accepts it.
#[tauri::command]
pub async fn record_feedback(
    id: String,
    kind: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let row_id: i64 = id.parse().map_err(|_| format!("bad suggestion id: {id}"))?;
    let now = crate::pipeline::epoch_ms();
    let (fb, sql, uses_ts) = match kind.as_str() {
        "clicked" => (
            aperture_pattern_engine::FeedbackEvent::Clicked,
            "UPDATE suggestions SET state='clicked', resolved_ts=?2 WHERE id=?1",
            true,
        ),
        "dismissed" => (
            aperture_pattern_engine::FeedbackEvent::Dismissed,
            "UPDATE suggestions SET state='dismissed', resolved_ts=?2 WHERE id=?1",
            true,
        ),
        "expired" => (
            aperture_pattern_engine::FeedbackEvent::Expired,
            "UPDATE suggestions SET state='expired', resolved_ts=?2 WHERE id=?1",
            true,
        ),
        "up" => (
            aperture_pattern_engine::FeedbackEvent::ThumbsUp,
            "UPDATE suggestions SET useful_rating='up' WHERE id=?1",
            false,
        ),
        "down" => (
            aperture_pattern_engine::FeedbackEvent::ThumbsDown,
            "UPDATE suggestions SET useful_rating='down' WHERE id=?1",
            false,
        ),
        other => return Err(format!("unknown feedback kind: {other}")),
    };
    let pattern_id = state
        .db
        .with_conn(|c| {
            if uses_ts {
                c.execute(sql, rusqlite::params![row_id, now])?;
            } else {
                c.execute(sql, rusqlite::params![row_id])?;
            }
            c.query_row(
                "SELECT pattern_id FROM suggestions WHERE id = ?1",
                [row_id],
                |r| r.get::<_, Option<i64>>(0),
            )
        })
        .map_err(|e| e.to_string())?;
    if let Some(pid) = pattern_id {
        let _ = state.feedback_tx.send((pid, fb)); // task gone = shutdown; fine
    }
    Ok(())
}

/// Global bubble snooze (ADR-040/Q95, doc 11 §6, doc 13 §8): silences bubble
/// EMISSION while capture + learning continue — distinct from the capture
/// toggle. `mode`: "off" | "15m" | "1h" | "forever" (until re-enabled).
#[tauri::command]
pub async fn set_snooze(mode: String, state: State<'_, AppState>) -> Result<(), String> {
    let now = crate::pipeline::epoch_ms();
    let until = match mode.as_str() {
        "off" => 0,
        "15m" => now + 15 * 60_000,
        "1h" => now + 3_600_000,
        "forever" => i64::MAX,
        other => return Err(format!("unknown snooze mode: {other}")),
    };
    state
        .snooze_until
        .store(until, std::sync::atomic::Ordering::SeqCst);
    Ok(())
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
    // ADR-040/Q95: while snoozed the overlay renders nothing; queued rows
    // surface here once the snooze lifts.
    let snoozed = state
        .snooze_until
        .load(std::sync::atomic::Ordering::SeqCst)
        > crate::pipeline::epoch_ms();
    if snoozed {
        return Ok(Vec::new());
    }
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

/// Critical Path B (doc 02 §5): resolve `action_ref` (the `connector_state`
/// uuid) → load the row from SQLite → `reconstruct()` the artifact →
/// `open()` via `ShellExecuteW`/protocol handler. Target < 200 ms. Records
/// `suggestion_clicked{outcome}` (SC7). Failure degrades honestly (doc 10 §6):
/// a bad target returns `Ok(Failed{..})` so the bubble swaps to fallback copy —
/// `Err` is reserved for malformed requests.
///
/// Validate-on-click (ADR-035): the button rendered optimistically; here —
/// before any dispatch — the state's freshness is re-checked and `reconstruct`
/// re-validates the target (e.g. the document connector re-checks the file
/// exists). Nothing executes unvalidated, and only a connector acts.
/// (`Connector::validate(cloud_payload)` is the *cloud*-suggestion gate — M7.)
#[tauri::command]
pub async fn bubble_click(
    id: String,
    action_ref: String,
    state: State<'_, AppState>,
) -> Result<OpenOutcome, String> {
    let started = std::time::Instant::now();
    if action_ref.is_empty() {
        return Err("bubble_click: empty action_ref".into());
    }
    let st = state
        .db
        .read_connector_state(&action_ref)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("bubble_click: no connector_state row {action_ref}"))?;

    // Reconstruct + dispatch on a blocking thread (ShellExecuteW + fs checks).
    let registry = Arc::clone(&state.connectors);
    let outcome = tokio::task::spawn_blocking(move || -> Result<OpenOutcome, String> {
        let Some(connector) = registry.by_type(&st.connector_type) else {
            return Err(format!("unknown connector type: {}", st.connector_type));
        };
        let now = crate::pipeline::epoch_ms();
        if st.stale_after_ts.is_some_and(|t| t <= now) {
            // The freshness factor should have zeroed this candidate long ago
            // (doc 08 §5); if a stale row is clicked anyway, fail gracefully.
            return Ok(OpenOutcome::Failed {
                reason: "captured state is stale (past TTL)".into(),
            });
        }
        match connector.reconstruct(&st) {
            Ok(artifact) => match connector.open(&artifact) {
                Ok(outcome) => Ok(outcome),
                Err(e) => Ok(OpenOutcome::Failed { reason: e.to_string() }),
            },
            Err(e) => Ok(OpenOutcome::Failed { reason: e.to_string() }),
        }
    })
    .await
    .map_err(|e| e.to_string())??;

    // Record the outcome (doc 10 §6, SC7). record_feedback("clicked") — sent
    // separately by the UI — owns state/resolved_ts; this owns `outcome`.
    let outcome_str = match &outcome {
        OpenOutcome::Resumed => "resumed",
        OpenOutcome::Degraded { .. } => "degraded",
        OpenOutcome::Failed { .. } => "failed_fallback",
    };
    if let Ok(row_id) = id.parse::<i64>() {
        if let Err(e) = state.db.with_conn(|c| {
            c.execute(
                "UPDATE suggestions SET outcome = ?2 WHERE id = ?1",
                rusqlite::params![row_id, outcome_str],
            )
            .map(|_| ())
        }) {
            tracing::error!(%e, "suggestion outcome update failed");
        }
    }
    // The suggestion_clicked event row (doc 03 §2) — persist-then-notify.
    let mut click_ev = aperture_contracts::Event {
        id: 0,
        ts: crate::pipeline::epoch_ms(),
        r#type: aperture_contracts::EventType::SuggestionClicked,
        app: None,
        process: None,
        window_title: None,
        payload: serde_json::json!({ "suggestion_id": id, "outcome": outcome_str }),
        connector_id: Some(action_ref),
        session_id: None,
        redaction_flags: 0,
    };
    match state.db.insert_event(&click_ev) {
        Ok(eid) => {
            click_ev.id = eid;
            let _ = state.bus.publish(click_ev);
        }
        Err(e) => tracing::error!(%e, "suggestion_clicked persist failed"),
    }

    let elapsed = started.elapsed();
    if elapsed > aperture_connectors::deeplinker::PATH_B_BUDGET {
        tracing::warn!(?elapsed, "Path B exceeded its 200 ms budget (doc 02 §5)");
    }
    Ok(outcome)
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
