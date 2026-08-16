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
    // Persist the decision + stamp the `capture_toggle` audit row BEFORE driving
    // the mechanism (doc 13 §3, §8, M9). Order matters: if we cannot record that
    // capture started, we must not start it — so an ON toggle aborts here.
    //
    // An OFF toggle proceeds regardless: a failed write must never trap the user
    // in the ON state. But it is NOT swallowed — the error is returned after the
    // release, because the decision did not persist and the next launch will
    // restore capture from the stale stored value. The user needs to know that.
    let persist_error = {
        let mut consent = state.consent.lock().await;
        match consent.set_capture_enabled(on, crate::pipeline::epoch_ms()) {
            Ok(()) => None,
            Err(e) => {
                tracing::error!(%e, on, "consent/audit write failed");
                if on {
                    return Err(format!("could not record capture consent: {e}"));
                }
                Some(e)
            }
        }
    };
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
    match persist_error {
        None => Ok(on),
        Some(e) => Err(format!(
            "capture was turned off, but the setting could not be saved ({e}); \
             it may switch back on at next launch"
        )),
    }
}

// ---------------------------------------------------------------------------
// M9 — privacy surface (doc 13). Consent, the audit trail, exclusions, Purge All.
// ---------------------------------------------------------------------------

/// Let a modal overlay surface accept input (doc 11 §2, M9).
///
/// The overlay is click-through and unfocusable by default, which is right for
/// passive bubbles and wrong for a dialog the user must answer. The first-run
/// consent flow and the Activity & Privacy panel call this `true` on mount and
/// `false` on unmount; without it their buttons are unclickable.
///
/// Routed through [`crate::hit_test::HitTestState`] so the modal override and
/// the bubble hover poller compose instead of overwriting each other's
/// `WS_EX_TRANSPARENT` writes (a modal closing must not kill a live bubble's
/// clickability, and vice versa).
///
/// Failure is non-fatal but IS surfaced: a modal the user cannot dismiss is a
/// worse outcome than a logged error, so the caller learns about it.
#[tauri::command]
pub async fn set_overlay_interactive(
    interactive: bool,
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    hit_test: State<'_, crate::hit_test::HitTestState>,
) -> Result<(), String> {
    hit_test.set_modal(&app, window.label(), interactive);
    Ok(())
}

/// Publish the overlay's interactive rects (doc 11 §2, M3-UI wiring closed).
///
/// The UI measures every `.surface-interactive` element (physical px, relative
/// to its own window) and calls this on change; the cursor poller
/// (`hit_test::spawn_poller`) then flips click-through only while the cursor is
/// inside one of these rects. An empty list restores full click-through — the
/// doc 11 §7 watchdog reset.
#[tauri::command]
pub async fn set_hit_test_rects(
    rects: Vec<crate::overlay::BubbleRect>,
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    hit_test: State<'_, crate::hit_test::HitTestState>,
) -> Result<(), String> {
    hit_test.set_rects(&app, window.label(), rects);
    Ok(())
}

/// Reset this window's interactivity state to click-through. The UI root calls
/// this once on mount so a WebView reload/crash cannot orphan a modal override
/// (doc 11 §7 watchdog for the modal half).
#[tauri::command]
pub async fn reset_overlay_interactivity(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    hit_test: State<'_, crate::hit_test::HitTestState>,
) -> Result<(), String> {
    hit_test.reset(&app, window.label());
    Ok(())
}

/// The consent snapshot the first-run flow and the settings view read
/// (doc 13 §8). Also reports whether at-rest encryption is actually in force, so
/// the UI can tell the truth about it rather than assume.
#[tauri::command]
pub async fn get_consent(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let consent = state.consent.lock().await;
    let s = consent.state();
    Ok(serde_json::json!({
        "first_run_completed": s.first_run_completed,
        "capture_enabled": s.capture_enabled,
        "voice_opt_in": s.voice_opt_in,
        "capture_opt_in_ts": s.capture_opt_in_ts,
        "db_encrypted": state.db.is_encrypted(),
    }))
}

/// Complete the first-run consent sequence (doc 13 §8, ADR-040). `enable_capture`
/// is the user's explicit decision; declining is a first-class outcome that still
/// marks first-run done, so the flow never re-nags.
#[tauri::command]
pub async fn complete_first_run(
    enable_capture: bool,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    {
        let mut consent = state.consent.lock().await;
        consent
            .complete_first_run(enable_capture, crate::pipeline::epoch_ms())
            .map_err(|e| e.to_string())?;
    }
    // Drive the mechanism to match the decision (INVARIANT 3: the toggle owner
    // is still the single writer of capture state).
    if enable_capture {
        state.orchestration.lock().await.toggle().turn_on().await;
    } else {
        let _ = events::emit_capture_indicator(&app, CaptureIndicator::Off);
    }
    // From now on the app is "just there" at login (regardless of the capture
    // decision — capture itself still follows consent at every launch). Release
    // builds only: a dev run must not write a target\debug path into HKCU Run.
    // Failure is non-fatal; the toggle stays available in the tray + dashboard.
    if !cfg!(debug_assertions) {
        match apply_autostart(&app, true) {
            Ok(()) => {
                if let Err(e) = persist_autostart(&state.db, true) {
                    tracing::warn!(%e, "autostart choice could not be persisted");
                }
            }
            Err(e) => tracing::warn!(%e, "start-at-login registration failed"),
        }
    }
    Ok(())
}

/// Grant microphone consent at first PTT (doc 13 §8). If capture is already
/// running, voice comes up immediately — no restart needed.
///
/// The enable is gated on the LIVE toggle state, not persisted consent: a
/// failed `capture.start()` leaves `consent.capture_enabled` true while the
/// mechanism is Off (decision vs mechanism rows, doc 13 §3), and voice must
/// follow the mechanism — never a stale decision (multi-agent review, 2026-08-13).
#[tauri::command]
pub async fn grant_voice_consent(state: State<'_, AppState>) -> Result<(), String> {
    {
        let mut consent = state.consent.lock().await;
        consent.grant_voice().map_err(|e| e.to_string())?;
    }
    if capture_is_live(&state).await {
        let _ = state.voice.tx.send(crate::voice::VoiceCmd::Enable);
    }
    Ok(())
}

/// The mechanism truth: is capture actually ON right now (doc 02 §7)?
async fn capture_is_live(state: &State<'_, AppState>) -> bool {
    let mut orch = state.orchestration.lock().await;
    orch.toggle().state() == aperture_orchestration::toggle_owner::CaptureState::On
}

/// The Activity & Privacy view's audit feed (doc 13 §3, §7, ADR-040): "when was
/// it watching?" and "what ever left this machine?", newest first.
#[tauri::command]
pub async fn list_audit(
    limit: Option<u32>,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let rows = state
        .db
        .recent_audit_events(limit.unwrap_or(200))
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id,
                "ts": e.ts,
                "type": e.r#type,
                "payload": e.payload,
            })
        })
        .collect())
}

/// One-click Purge All (doc 13 §7). The confirmation lives in the UI; by the time
/// this is invoked the user has confirmed. Returns the number of history rows
/// deleted so the UI can report what actually happened.
///
/// Audit rows inside the 30-day window, the exclusion list, and consent all
/// survive — see `Db::purge_all` for why.
#[tauri::command]
pub async fn purge_all(state: State<'_, AppState>) -> Result<usize, String> {
    let policy = aperture_db::retention::RetentionPolicy::default();
    state
        .db
        .purge_all(crate::pipeline::epoch_ms(), &policy)
        .map_err(|e| e.to_string())
}

/// The user's exclusion rules, for the Activity & Privacy view (doc 13 §4).
#[tauri::command]
pub async fn list_exclusions(state: State<'_, AppState>) -> Result<Vec<serde_json::Value>, String> {
    let rows = state.db.read_exclusion_list().map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|(id, match_kind, pattern, enabled)| {
            serde_json::json!({ "id": id, "match_kind": match_kind, "pattern": pattern, "enabled": enabled })
        })
        .collect())
}

/// Add an exclusion rule — the one-click "exclude this app/domain" affordance
/// and the first-run confirm rows (doc 13 §4, §9).
///
/// The pattern is validated with the SAME regex builder the capture gate
/// compiles with (`exclusion::validate_pattern`). This matters: `compile` fails
/// *open* on a bad regex, so persisting an invalid one would produce a rule the
/// UI shows as an active protection while it silently matches nothing.
///
/// The rule is durable immediately AND hot-swapped into the running capture
/// matcher: `AppState.exclusions` is the same shared handle the sampler and
/// normalizer hold, so `replace` takes effect on the next frame/event.
#[tauri::command]
pub async fn add_exclusion(
    match_kind: String,
    pattern: String,
    state: State<'_, AppState>,
) -> Result<i64, String> {
    let pattern = pattern.trim();
    aperture_capture::exclusion::validate_pattern(&match_kind, pattern)?;
    let id = state
        .db
        .add_exclusion_rule(&match_kind, pattern)
        .map_err(|e| e.to_string())?;
    reload_exclusions(&state);
    Ok(id)
}

/// Enable/disable (`enabled: Some`) or delete (`enabled: None`) one rule.
#[tauri::command]
pub async fn set_exclusion(
    id: i64,
    enabled: Option<bool>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.db.set_exclusion_enabled(id, enabled).map_err(|e| e.to_string())?;
    reload_exclusions(&state);
    Ok(())
}

/// Recompile the exclusion matcher from the durable rows and swap it into the
/// live capture gate (doc 13 §4).
///
/// A re-read failure keeps the PREVIOUS compiled list — never an empty one.
/// Failing open here would silently drop the user's protections mid-session,
/// the exact bug the M9 review closed at startup (`main::load_exclusions`).
fn reload_exclusions(state: &State<'_, AppState>) {
    match state.db.read_exclusion_list() {
        Ok(rows) => {
            let rules = aperture_capture::exclusion::rules_from_rows(rows);
            let n = rules.len();
            state.exclusions.replace(rules);
            tracing::info!(count = n, "exclusion matcher hot-reloaded (doc 13 §4)");
        }
        Err(e) => tracing::error!(
            %e,
            "exclusion re-read failed — keeping the previous matcher; \
             the new rule is durable and applies at next launch"
        ),
    }
}

/// First-run detect-and-suggest (doc 13 §4, §8; ADR-029/ADR-040): scan locally
/// for installed password managers / finance apps and return *candidates* the
/// user confirms. Nothing is applied here — confirming calls [`add_exclusion`].
#[tauri::command]
pub async fn suggest_exclusions(
    state: State<'_, AppState>,
) -> Result<Vec<aperture_privacy::detect_suggest::SuggestedExclusion>, String> {
    let already: Vec<String> = state
        .db
        .read_exclusion_list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(_, _, pattern, _)| pattern)
        .collect();
    // The filesystem walk is blocking; keep it off the async executor.
    let installed =
        tauri::async_runtime::spawn_blocking(aperture_privacy::detect_suggest::installed_process_names)
            .await
            .map_err(|e| e.to_string())?;
    Ok(aperture_privacy::detect_suggest::suggest_from(&installed, &already))
}

// ---------------------------------------------------------------------------
// Dashboard (doc 11, ADR-040): read-only views over the local history — "what
// has it captured, what does it know". Everything here is metadata + text the
// user's own DB already holds; nothing egresses (two-emitter rule untouched).
// ---------------------------------------------------------------------------

/// Aggregate counts + storage facts for the dashboard's Overview tab.
#[tauri::command]
pub async fn dashboard_stats(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let counts = state
        .db
        .with_conn(|c| {
            let count = |sql: &str| -> rusqlite::Result<i64> { c.query_row(sql, [], |r| r.get(0)) };
            Ok(serde_json::json!({
                "events": count("SELECT COUNT(*) FROM events")?,
                "ocr_texts": count("SELECT COUNT(*) FROM screen_context WHERE ocr_text IS NOT NULL")?,
                "embeddings": count("SELECT COUNT(*) FROM ctx_vec")?,
                "patterns": count("SELECT COUNT(*) FROM patterns")?,
                "suggestions": count("SELECT COUNT(*) FROM suggestions")?,
                "connector_states": count("SELECT COUNT(*) FROM connector_state")?,
                "voice_utterances": count("SELECT COUNT(*) FROM events WHERE type = 'voice_utterance'")?,
                "sessions": count("SELECT COUNT(DISTINCT session_id) FROM events WHERE session_id IS NOT NULL")?,
                "first_event_ts": c.query_row("SELECT MIN(ts) FROM events", [], |r| r.get::<_, Option<i64>>(0))?,
                "last_event_ts": c.query_row("SELECT MAX(ts) FROM events", [], |r| r.get::<_, Option<i64>>(0))?,
            }))
        })
        .map_err(|e| e.to_string())?;
    let db_bytes = std::fs::metadata(aperture_db::default_db_path())
        .map(|m| m.len())
        .unwrap_or(0);
    let mut stats = counts;
    stats["db_bytes"] = serde_json::json!(db_bytes);
    stats["db_encrypted"] = serde_json::json!(state.db.is_encrypted());
    {
        let consent = state.consent.lock().await;
        stats["capture_enabled"] = serde_json::json!(consent.state().capture_enabled);
        stats["voice_opt_in"] = serde_json::json!(consent.state().voice_opt_in);
    }
    Ok(stats)
}

/// The History tab: recent events joined with their screen context, optionally
/// filtered by taxonomy type and a LIKE search over app/title/OCR text.
#[tauri::command]
pub async fn list_events(
    limit: Option<u32>,
    kind: Option<String>,
    search: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let limit = limit.unwrap_or(100).min(500);
    let kind = kind.filter(|s| !s.is_empty());
    let needle = search
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{s}%"));
    state
        .db
        .with_conn(|c| {
            let mut sql = String::from(
                "SELECT e.id, e.ts, e.type, e.app, e.process, e.window_title, e.session_id, \
                        e.redaction_flags, e.payload, \
                        substr(sc.ocr_text, 1, 280), sc.vlm_summary \
                 FROM events e LEFT JOIN screen_context sc ON sc.event_id = e.id WHERE 1=1",
            );
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            if let Some(k) = &kind {
                sql.push_str(" AND e.type = ?");
                params.push(Box::new(k.clone()));
            }
            if let Some(n) = &needle {
                sql.push_str(
                    " AND (e.window_title LIKE ? OR e.app LIKE ? OR sc.ocr_text LIKE ?)",
                );
                params.push(Box::new(n.clone()));
                params.push(Box::new(n.clone()));
                params.push(Box::new(n.clone()));
            }
            sql.push_str(" ORDER BY e.ts DESC LIMIT ?");
            params.push(Box::new(limit));

            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
                |r| {
                    let payload: Option<String> = r.get(8)?;
                    Ok(serde_json::json!({
                        "id": r.get::<_, i64>(0)?,
                        "ts": r.get::<_, i64>(1)?,
                        "type": r.get::<_, String>(2)?,
                        "app": r.get::<_, Option<String>>(3)?,
                        "process": r.get::<_, Option<String>>(4)?,
                        "title": r.get::<_, Option<String>>(5)?,
                        "session_id": r.get::<_, Option<i64>>(6)?,
                        "redaction_flags": r.get::<_, i64>(7)?,
                        "payload": payload
                            .and_then(|p| serde_json::from_str::<serde_json::Value>(&p).ok()),
                        "ocr": r.get::<_, Option<String>>(9)?,
                        "vlm_summary": r.get::<_, Option<String>>(10)?,
                    }))
                },
            )?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .map_err(|e| e.to_string())
}

/// The Patterns tab: what the engine has mined (doc 08).
#[tauri::command]
pub async fn list_patterns(
    limit: Option<u32>,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let limit = limit.unwrap_or(100).min(500);
    state
        .db
        .with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, signature, n, support, confidence, last_seen, dismiss_decay, muted_until \
                 FROM patterns ORDER BY confidence DESC, support DESC LIMIT ?",
            )?;
            let rows = stmt.query_map([limit], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, i64>(0)?,
                    "signature": r.get::<_, Option<String>>(1)?,
                    "n": r.get::<_, Option<i64>>(2)?,
                    "support": r.get::<_, Option<i64>>(3)?,
                    "confidence": r.get::<_, Option<f64>>(4)?,
                    "last_seen": r.get::<_, Option<i64>>(5)?,
                    "dismiss_decay": r.get::<_, Option<f64>>(6)?,
                    "muted_until": r.get::<_, Option<i64>>(7)?,
                }))
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .map_err(|e| e.to_string())
}

/// The Suggestions tab: every suggestion ever surfaced, newest first, with its
/// lifecycle outcome — unlike `list_suggestions`, which serves only live ones.
#[tauri::command]
pub async fn list_suggestion_history(
    limit: Option<u32>,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let limit = limit.unwrap_or(100).min(500);
    state
        .db
        .with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, title, glyph, confidence, state, shown_ts, resolved_ts, outcome, \
                        useful_rating, source \
                 FROM suggestions ORDER BY id DESC LIMIT ?",
            )?;
            let rows = stmt.query_map([limit], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, i64>(0)?,
                    "title": r.get::<_, Option<String>>(1)?,
                    "glyph": r.get::<_, Option<String>>(2)?,
                    "confidence": r.get::<_, Option<f64>>(3)?,
                    "state": r.get::<_, Option<String>>(4)?,
                    "shown_ts": r.get::<_, Option<i64>>(5)?,
                    "resolved_ts": r.get::<_, Option<i64>>(6)?,
                    "outcome": r.get::<_, Option<String>>(7)?,
                    "useful_rating": r.get::<_, Option<String>>(8)?,
                    "source": r.get::<_, Option<String>>(9)?,
                }))
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Decision #30 — VLM weight install. The weights (~3.3 GB) cannot ship in the
// installer; without them the app runs OCR-only. `vlm_status` makes that state
// visible; `vlm_download` is the ONLY trigger for the fetch — an explicit user
// click, never automatic (the app's promise: no surprise network activity).
// ---------------------------------------------------------------------------

/// VLM install status for the Dashboard notice (decision #30): which weight
/// files are present at the spawner's resolved paths (present = exists at the
/// settings-declared byte size), and whether a download is running right now.
#[tauri::command]
pub async fn vlm_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    use aperture_orchestration::model_fetch::is_present;
    let loadout = crate::vlm_fetch::loadout_section(&state.db);
    let items = crate::vlm_fetch::spec_from_settings(&loadout, &state.vlm_fetch);
    let files: Vec<serde_json::Value> = items
        .iter()
        .map(|i| {
            serde_json::json!({
                "file": i.dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                "expected_bytes": i.expected_bytes,
                "present": is_present(i),
            })
        })
        .collect();
    let missing_bytes: u64 = items
        .iter()
        .filter(|i| !is_present(i))
        .map(|i| i.expected_bytes)
        .sum();
    Ok(serde_json::json!({
        "installed": missing_bytes == 0,
        "downloading": state.vlm_fetch.in_flight.load(std::sync::atomic::Ordering::SeqCst),
        "missing_bytes": missing_bytes,
        "files": files,
    }))
}

/// Start the user-initiated VLM weight download (decision #30). Streams
/// progress on the `vlm_fetch` event and returns immediately.
///
/// Two-emitter rule note (doc 13 §2): the fetch itself runs in
/// `aperture_orchestration::model_fetch` — a sanctioned crate. It is model
/// INGRESS from the settings-declared URL, initiated by this explicit click;
/// the request carries no user data, so the zero-DATA-egress promise holds.
/// The shell opens no socket here.
///
/// Terminal states are honest: `phase: "done"` means every file verified at
/// its expected size and the NEXT VLM spawn uses it (the spawner re-reads the
/// same resolved path — no restart); `phase: "error"` keeps any resumable
/// `.part` so a retry click continues where it stopped.
#[tauri::command]
pub async fn vlm_download(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let fetch = Arc::clone(&state.vlm_fetch);
    // Double-start guard: one download at a time; cleared on any terminal state.
    if fetch
        .in_flight
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("a VLM download is already running".into());
    }
    let loadout = crate::vlm_fetch::loadout_section(&state.db);
    let items = crate::vlm_fetch::spec_from_settings(&loadout, &fetch);
    let total: u64 = items.iter().map(|i| i.expected_bytes).sum();
    if aperture_orchestration::model_fetch::missing(&items).is_empty() {
        // Already installed (e.g. a stale Dashboard) — terminal, no network.
        fetch.in_flight.store(false, Ordering::SeqCst);
        let _ = events::emit_vlm_fetch(
            &app,
            &events::VlmFetchPayload {
                phase: "done".into(),
                file: None,
                received_bytes: total,
                total_bytes: total,
                error: None,
            },
        );
        return Ok(());
    }
    tauri::async_runtime::spawn(async move {
        // Throttle the WebView stream: first snapshot + every ~8 MB + terminal
        // — per-chunk emits would flood the IPC channel over a 3.3 GB fetch.
        const EMIT_STEP_BYTES: u64 = 8 * 1024 * 1024;
        let mut last_emit: Option<u64> = None;
        let progress_app = app.clone();
        let mut on_progress = move |p: aperture_orchestration::model_fetch::FetchProgress| {
            let due = match last_emit {
                None => true,
                Some(last) => p.received_bytes >= last + EMIT_STEP_BYTES,
            } || p.received_bytes == p.total_bytes;
            if !due {
                return;
            }
            last_emit = Some(p.received_bytes);
            let _ = events::emit_vlm_fetch(
                &progress_app,
                &events::VlmFetchPayload {
                    phase: "downloading".into(),
                    file: Some(p.file),
                    received_bytes: p.received_bytes,
                    total_bytes: p.total_bytes,
                    error: None,
                },
            );
        };
        let result =
            aperture_orchestration::model_fetch::fetch_all(&items, &mut on_progress).await;
        fetch.in_flight.store(false, Ordering::SeqCst);
        let payload = match result {
            Ok(()) => {
                tracing::info!("VLM weights installed — live on the next VLM spawn (decision #30)");
                events::VlmFetchPayload {
                    phase: "done".into(),
                    file: None,
                    received_bytes: total,
                    total_bytes: total,
                    error: None,
                }
            }
            Err(e) => {
                tracing::error!(%e, "VLM weight download failed (decision #30)");
                events::VlmFetchPayload {
                    phase: "error".into(),
                    file: None,
                    received_bytes: 0,
                    total_bytes: total,
                    error: Some(e.to_string()),
                }
            }
        };
        let _ = events::emit_vlm_fetch(&app, &payload);
    });
    Ok(())
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
    app: tauri::AppHandle,
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
        // Explicit "Mute this pattern" (doc 11 §3): the row resolves as
        // dismissed; the engine jumps the ladder straight to the 7-day mute.
        "muted" => (
            aperture_pattern_engine::FeedbackEvent::Muted,
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
    // Terminal transitions broadcast so EVERY overlay window converges — a
    // dismissal on one monitor removed the bubble there only, leaving live
    // clones on the others (2026-08-15 review). The originating window applies
    // the same state locally first; re-applying is idempotent.
    if matches!(kind.as_str(), "clicked" | "dismissed" | "muted" | "expired") {
        let lifecycle_state = if kind == "muted" { "dismissed" } else { kind.as_str() };
        let _ = crate::events::emit_suggestion_lifecycle(
            &app,
            &serde_json::json!({ "id": id, "state": lifecycle_state }),
        );
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

/// Read the global snooze deadline (epoch ms; `0` = off, `i64::MAX` = until
/// re-enabled) — backs the HUD's 🔕 control so it can render the truth.
#[tauri::command]
pub async fn get_snooze(state: State<'_, AppState>) -> Result<i64, String> {
    Ok(state.snooze_until.load(std::sync::atomic::Ordering::SeqCst))
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
                "SELECT id, title, glyph, confidence, connector_id, source \
                 FROM suggestions WHERE state IN ('queued','shown') \
                 ORDER BY shown_ts DESC LIMIT 16",
            )?;
            let rows = stmt.query_map([], |row| {
                let id: i64 = row.get(0)?;
                let source = match row.get::<_, Option<String>>(5)?.as_deref() {
                    Some("claude") => SuggestionSource::Claude,
                    _ => SuggestionSource::Local,
                };
                Ok(BubbleSpecEnvelope {
                    id: id.to_string(),
                    spec: BubbleSpec {
                        title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        glyph: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        sublabel: None,
                        action_ref: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                        source,
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
        let now = crate::pipeline::epoch_ms();
        if st.stale_after_ts.is_some_and(|t| t <= now) {
            // The freshness factor should have zeroed this candidate long ago
            // (doc 08 §5); if a stale row is clicked anyway, fail gracefully.
            return Ok(OpenOutcome::Failed {
                reason: "captured state is stale (past TTL)".into(),
            });
        }
        // Path B analog for "switch to X" bubbles (owner decision #15,
        // 2026-08-16): `app_focus` rows are synthesized by the pattern task
        // (pipeline.rs), not captured by a connector, so they resolve here.
        if st.connector_type == "app_focus" {
            return Ok(open_app_focus(&st));
        }
        let Some(connector) = registry.by_type(&st.connector_type) else {
            return Err(format!("unknown connector type: {}", st.connector_type));
        };
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

/// Dispatch a "switch to X" click (decision #15): hand the stored process name
/// to `ShellExecuteW("open", …)` through the connectors crate's one dispatch
/// primitive — the same trust boundary every other bubble click crosses, so the
/// shell gains no process-spawn API (invariant 2 discipline). The name resolves
/// via App Paths / PATH; single-instance apps (Slack, Discord, browsers)
/// self-activate their existing window, others launch fresh. Honest degrade: an
/// unresolvable name returns `Failed` and the bubble swaps to fallback copy.
fn open_app_focus(st: &aperture_contracts::ConnectorState) -> OpenOutcome {
    let Some(process) = st
        .reconstruct_payload
        .get("process")
        .and_then(|v| v.as_str())
        .filter(|p| !p.trim().is_empty())
    else {
        return OpenOutcome::Failed {
            reason: "app_focus state has no process name".into(),
        };
    };
    // `Url` is the bare ShellExecuteW("open", …) rung — no pre-checks, exactly
    // what an exe-name dispatch needs.
    match aperture_connectors::deeplinker::open(&aperture_contracts::ResumeArtifact::Url(
        process.to_string(),
    )) {
        Ok(outcome) => outcome,
        Err(e) => OpenOutcome::Failed {
            reason: format!("could not switch to {process}: {e}"),
        },
    }
}

/// How long an abandoned preview session may linger before the next
/// `request_preview` prunes it (the UI's Cancel path calls `preview_cancel`;
/// this is the backstop for a crashed WebView).
const PREVIEW_TTL_MS: i64 = 60 * 60 * 1000;

/// Build + redact the Context Payload for an intent and return the EXACT wire
/// object the preview renders (doc 03 §4, doc 11 §4, doc 13 §5).
///
/// "Preview == wire": this returns the single object that will later be sent
/// byte-for-byte. It does NOT set `user_approved` (that is
/// [`preview_set_approved`] only) and does NOT touch the network.
///
/// Items gathered, in order (doc 09 §5):
/// - the `answer_query` intent seeds the last voice transcript (`user_addition`);
/// - `seed_action_ref` resolves to the originating connector state;
/// - a recent-events trail (metadata only, EXCLUDED/flagged rows filtered —
///   excluded events can never appear in any payload, doc 13 §2/§4).
#[tauri::command]
pub async fn request_preview(
    intent: Intent,
    seed_action_ref: Option<String>,
    state: State<'_, AppState>,
) -> Result<ContextPayload, String> {
    use aperture_contracts::PayloadItem;

    let mut items: Vec<PayloadItem> = Vec::new();

    // Voice escalation carries the user's actual question (doc 07 §5).
    // `take`, not clone: the transcript seeds exactly ONE preview — a stale
    // utterance must not attach to unrelated later payloads.
    if intent == Intent::AnswerQuery {
        let transcript = state
            .voice
            .last_transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(text) = transcript {
            items.push(PayloadItem::UserAddition { text });
        }
    }

    // The originating bubble/answer's connector context (doc 11 §4-§5).
    if let Some(action_ref) = seed_action_ref.filter(|s| !s.is_empty()) {
        match state.db.read_connector_state(&action_ref) {
            Ok(Some(st)) => items.push(PayloadItem::Connector {
                connector_type: st.connector_type,
                payload: st.reconstruct_payload,
            }),
            Ok(None) => tracing::warn!(%action_ref, "preview seed: connector_state row gone"),
            Err(e) => tracing::error!(%e, "preview seed read failed"),
        }
    }

    // Voice escalation rides with the recent on-screen text (decision #29):
    // "ask claude" was transcript-only, which gave Claude the question but not
    // the screen it was asked about. Same exclusion filter as the trail
    // (`redaction_flags = 0`, doc 13 §4); the builder's redactor masks the text
    // BEFORE preview and approval re-runs redaction (doc 13 §5) — the enriched
    // items ride the identical preview→Send gate, no new egress path. A read
    // failure soft-degrades (log + transcript/trail only): richer context is an
    // enrichment, not a precondition.
    if intent == Intent::AnswerQuery {
        match recent_ocr_items(&state.db) {
            Ok(ocr_items) => items.extend(ocr_items),
            Err(e) => tracing::error!(%e, "escalation OCR context read failed (decision #29)"),
        }
    }

    // Recent-events trail, oldest-first so oversize truncation drops the oldest
    // (doc 09 §6). `redaction_flags = 0` keeps every excluded/private-window
    // event out of the payload — the doc 13 §4 guarantee.
    let trail = state
        .db
        .with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT ts, type, app, window_title FROM events \
                 WHERE redaction_flags = 0 \
                   AND type NOT IN ('capture_toggle','cloud_send','mcp_search') \
                 ORDER BY ts DESC LIMIT 50",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(serde_json::json!({
                    "ts": r.get::<_, i64>(0)?,
                    "type": r.get::<_, String>(1)?,
                    "app": r.get::<_, Option<String>>(2)?,
                    "title": r.get::<_, Option<String>>(3)?,
                }))
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .map_err(|e| e.to_string())?;
    if !trail.is_empty() {
        let events: Vec<serde_json::Value> = trail.into_iter().rev().collect();
        items.push(PayloadItem::EventTrail { events });
    }

    // Redact BEFORE preview (doc 13 §5), with the user's configured terms.
    let user_terms = read_user_redaction_terms(&state.db);
    let redactor = aperture_privacy::redaction::Redactor::new(&user_terms)
        .map_err(|e| format!("redaction rules failed to compile: {e}"))?;

    let (payload, report) = aperture_reasoning_gateway::payload_builder::build(
        intent,
        items,
        state.push_target,
        &redactor,
        crate::pipeline::epoch_ms(),
    )
    .map_err(|e| e.to_string())?;
    tracing::info!(
        payload_id = %payload.payload_id,
        bytes = report.serialized_bytes,
        oversize = report.oversize_warning,
        truncated = report.events_truncated,
        "context payload assembled for preview (doc 09 §5)"
    );

    let mut previews = state.previews.lock().await;
    // Prune sessions an old WebView abandoned without cancelling.
    let now = crate::pipeline::epoch_ms();
    let crate::app_state::PreviewStore { sessions, approved } = &mut *previews;
    sessions.retain(|_, s| now - s.payload().created_ts < PREVIEW_TTL_MS);
    approved.retain(|id, _| sessions.contains_key(id));
    sessions.insert(
        payload.payload_id,
        aperture_reasoning_gateway::preview::PreviewSession::new(payload.clone()),
    );
    Ok(payload)
}

/// The event-trail slice for the preview panel's "Add more history" slider
/// (doc 11 §4, ADR-040/Q71): metadata-only rows from the last `minutes`,
/// oldest-first, same filters + shape as the trail `request_preview` builds,
/// capped at EVENT_TRAIL_MAX (50). The panel swaps its `event_trail` item for
/// this — WYSIWYS: the object on screen is the object that ships, and approval
/// re-runs redaction over it (doc 13 §5).
#[tauri::command]
pub async fn list_trail_events(
    minutes: u32,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let floor = crate::pipeline::epoch_ms() - minutes as i64 * 60_000;
    let trail = state
        .db
        .with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT ts, type, app, window_title FROM events \
                 WHERE redaction_flags = 0 AND ts >= ?1 \
                   AND type NOT IN ('capture_toggle','cloud_send','mcp_search') \
                 ORDER BY ts DESC LIMIT 50",
            )?;
            let rows = stmt.query_map([floor], |r| {
                Ok(serde_json::json!({
                    "ts": r.get::<_, i64>(0)?,
                    "type": r.get::<_, String>(1)?,
                    "app": r.get::<_, Option<String>>(2)?,
                    "title": r.get::<_, Option<String>>(3)?,
                }))
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .map_err(|e| e.to_string())?;
    Ok(trail.into_iter().rev().collect())
}

/// How many recent OCR snapshots ride with a voice escalation, and how much of
/// each (decision #29). 3 × 2000 chars ≈ 6 KB worst case — far under the 50 KB
/// preview warning while still carrying the screen(s) the question was about.
const ESCALATION_OCR_ROWS: u32 = 3;
const ESCALATION_OCR_CHARS: u32 = 2000;

/// The newest on-screen text for a voice escalation (decision #29):
/// `screen_context` OCR joined to non-excluded events (`redaction_flags = 0` —
/// excluded rows can never enter any payload, doc 13 §4), newest first so the
/// screen the user is looking at leads. The text here is UNredacted by design:
/// `payload_builder::build` redacts every text item before the preview renders,
/// and `preview_set_approved` re-runs redaction over the panel's edits
/// (doc 13 §5) — the same machinery every other payload item goes through.
fn recent_ocr_items(
    db: &aperture_db::Db,
) -> Result<Vec<aperture_contracts::PayloadItem>, aperture_db::DbError> {
    db.with_conn(|c| {
        let mut stmt = c.prepare(
            "SELECT e.id, substr(sc.ocr_text, 1, ?1) \
             FROM screen_context sc JOIN events e ON e.id = sc.event_id \
             WHERE e.redaction_flags = 0 AND sc.ocr_text IS NOT NULL \
               AND length(trim(sc.ocr_text)) > 0 \
             ORDER BY e.ts DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![ESCALATION_OCR_CHARS, ESCALATION_OCR_ROWS],
            |r| {
                Ok(aperture_contracts::PayloadItem::OcrText {
                    source_event_id: r.get(0)?,
                    text: r.get(1)?,
                    redacted: false,
                })
            },
        )?;
        rows.collect()
    })
}

/// Health of one transport for the preview footer's dot (doc 11 §4) — was a
/// hardcoded "setup" yellow since M7 (2026-08-15 review). `target` is the
/// kebab-case wire name the payload carries.
#[tauri::command]
pub async fn transport_health(
    target: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    use aperture_contracts::reasoning::Health;
    use aperture_contracts::TransportId;
    fn id_matches(id: TransportId, target: &str) -> bool {
        matches!(
            (id, target),
            (TransportId::ClaudeCli, "claude-cli")
                | (TransportId::ClaudeDesktopMcp, "claude-desktop-mcp")
                | (TransportId::MessagesApi, "messages-api")
        )
    }
    let health = state
        .gateway
        .health_report()
        .await
        .into_iter()
        .find(|(id, _)| id_matches(*id, &target))
        .map(|(_, h)| h);
    Ok(match health {
        Some(Health::Ready) => serde_json::json!({ "kind": "ready" }),
        Some(Health::NeedsSetup(d)) => serde_json::json!({ "kind": "needs_setup", "detail": d }),
        Some(Health::Unavailable(d)) => serde_json::json!({ "kind": "unavailable", "detail": d }),
        None => serde_json::json!({ "kind": "needs_setup", "detail": "transport not configured" }),
    })
}

/// The user's configured redaction terms (doc 13 §5 rule 6), from settings.
/// Each entry is a literal term; invalid regexes cannot arise from literals.
/// `pub(crate)`: the MCP bridge's gated search redacts through the same terms.
pub(crate) fn read_user_redaction_terms(
    db: &aperture_db::Db,
) -> Vec<aperture_privacy::redaction::UserTerm> {
    let Ok(Some(raw)) = db.get_setting("privacy") else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else { return Vec::new() };
    v.get("redaction_user_terms")
        .and_then(|t| t.as_array())
        .map(|terms| {
            terms
                .iter()
                .filter_map(|t| t.as_str())
                .map(|pattern| aperture_privacy::redaction::UserTerm {
                    pattern: pattern.to_string(),
                    is_regex: false,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Approve a previewed payload (doc 15 §2(b), doc 11 §4) — the contract's SOLE
/// approval path, and the point where the panel's edits become the in-process
/// object (WYSIWYS).
///
/// Order is load-bearing (multi-agent review, 2026-08-13): the edits are synced
/// HERE, re-redacted HERE, and the approval is recorded as a SHA-256 over the
/// resulting canonical bytes — so what `preview_send` later ships is exactly
/// the content that passed this gate, never a later client-supplied body.
/// If the re-redaction changed anything (the user typed a note containing a
/// secret/user term), approval is REFUSED and the redacted object is returned
/// for re-review: the user must see the real wire bytes before approving them.
#[tauri::command]
pub async fn preview_set_approved(
    payload: ContextPayload,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    use aperture_contracts::{PayloadItem, EVENT_TRAIL_MAX};

    let payload_id = payload.payload_id;
    let mut previews = state.previews.lock().await;
    let session = previews
        .sessions
        .get_mut(&payload_id)
        .ok_or_else(|| format!("preview_set_approved: unknown payload {payload_id}"))?;

    // Sync the panel's edits onto the core-owned object.
    let new_hits = {
        let p = session.payload_mut();
        p.intent = payload.intent;
        p.items = payload.items;
        // Re-enforce the trail cap (doc 03 §4) — the panel can only remove
        // events today, but the cap must not depend on client behavior.
        for item in &mut p.items {
            if let PayloadItem::EventTrail { events } = item {
                if events.len() > EVENT_TRAIL_MAX {
                    let drop = events.len() - EVENT_TRAIL_MAX;
                    events.drain(0..drop);
                }
            }
        }
        // Re-run redaction over the synced content (doc 13 §5): panel-added
        // text has never seen the redactor. Placeholders from the first pass
        // don't re-match, so this only catches NEW leaks.
        let user_terms = read_user_redaction_terms(&state.db);
        let redactor = aperture_privacy::redaction::Redactor::new(&user_terms)
            .map_err(|e| format!("redaction rules failed to compile: {e}"))?;
        redactor.redact_payload(p)
    };

    if !new_hits.is_empty() {
        // Content changed under redaction — the user has not seen these bytes.
        // Return the redacted object for re-review; no approval recorded.
        return Ok(serde_json::json!({
            "payload": session.payload(),
            "changed": true,
        }));
    }

    let wire = serde_json::to_vec(session.payload())
        .map_err(|e| format!("serialize failed: {e}"))?;
    let hash = aperture_privacy::audit_log::sha256_hex(&wire);
    let response = serde_json::json!({ "payload": session.payload(), "changed": false });
    previews.approved.insert(payload_id, hash);
    Ok(response)
}

/// Cancel a preview: drop the in-process session — zero residue (doc 13 §3).
#[tauri::command]
pub async fn preview_cancel(payload_id: Uuid, state: State<'_, AppState>) -> Result<(), String> {
    let mut previews = state.previews.lock().await;
    previews.sessions.remove(&payload_id);
    previews.approved.remove(&payload_id);
    Ok(())
}

/// The ONLY call that reaches the network (doc 15 §2(c), doc 13 §2) — via the
/// gateway, SHA-256 audit-logged as `cloud_send`.
///
/// Takes only the payload id: the bytes that ship are the CORE-owned session
/// object whose hash was recorded at approval — a client cannot substitute
/// content after the gate (preview == wire, doc 13 §3). On a transport failure
/// the session and approval are RESTORED so Send can honestly be retried, and
/// the error reaches the panel instead of dead-ending.
#[tauri::command]
pub async fn preview_send(
    payload_id: Uuid,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<StructuredSuggestions, String> {
    let (session, approved_hash) = {
        let mut previews = state.previews.lock().await;
        let Some(hash) = previews.approved.remove(&payload_id) else {
            return Err(format!(
                "preview_send: payload {payload_id} was not approved via preview_set_approved (doc 15 §2b)"
            ));
        };
        let session = previews
            .sessions
            .remove(&payload_id)
            .ok_or_else(|| format!("preview_send: no session for payload {payload_id}"))?;
        (session, hash)
    };

    // Content-bound approval: the session must still hash to what was approved.
    let wire = serde_json::to_vec(session.payload()).map_err(|e| e.to_string())?;
    if aperture_privacy::audit_log::sha256_hex(&wire) != approved_hash {
        return Err("preview_send: payload changed after approval — re-approve (doc 13 §3)".into());
    }

    // Keep an unapproved copy so a failed transport leaves Send retryable.
    let backup = session.payload().clone();
    let approved = session
        .approve(aperture_reasoning_gateway::preview::PreviewDecision::Send)
        .ok_or_else(|| "preview_send: session cancelled".to_string())?;

    match state.gateway.send_with_preview(&approved, true).await {
        Ok(outcome) => {
            // Decision #41: an audit-write failure after a successful egress
            // must be VISIBLE — the audit log is the sole record of what left
            // this machine, and this send is now missing from it. The send
            // itself stays non-blocking (the bytes already left).
            if let Some(reason) = &outcome.audit_failure {
                let _ = crate::events::emit_audit_alert(
                    &app,
                    &format!(
                        "This send reached Claude but was NOT recorded in the audit log \
                         ({reason}). The \"what left this machine?\" trail is missing this send."
                    ),
                );
            }
            let result = outcome.suggestions;
            // US3's last leg (doc 09 §4): render the validated response as
            // bubbles/answer core-side — the panel closes right after Send, so
            // the returned value alone reached no surface (2026-08-15 review).
            crate::pipeline::surface_cloud_suggestions(&app, state.inner(), &result);
            Ok(result)
        }
        Err(e) => {
            let mut previews = state.previews.lock().await;
            previews.sessions.insert(
                payload_id,
                aperture_reasoning_gateway::preview::PreviewSession::new(backup),
            );
            previews.approved.insert(payload_id, approved_hash);
            Err(e.to_string())
        }
    }
}

/// PTT key pressed (doc 07, Path C) — forwards to the voice thread.
#[tauri::command]
pub async fn voice_ptt_down(state: State<'_, AppState>) -> Result<(), String> {
    require_voice_consent(&state).await?;
    state
        .voice
        .tx
        .send(crate::voice::VoiceCmd::PttDown)
        .map_err(|_| "voice thread not running".to_string())
}

/// PTT key released (doc 07, Path C) — forwards to the voice thread.
#[tauri::command]
pub async fn voice_ptt_up(state: State<'_, AppState>) -> Result<(), String> {
    state
        .voice
        .tx
        .send(crate::voice::VoiceCmd::PttUp)
        .map_err(|_| "voice thread not running".to_string())
}

/// Confirm-chip "Run" (doc 07 §4.4): re-issue a confirmed transcript through
/// the query path at confidence 1.0. Never re-stores the utterance.
#[tauri::command]
pub async fn voice_run_transcript(
    transcript: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if transcript.trim().is_empty() {
        return Err("voice_run_transcript: empty transcript".into());
    }
    state
        .voice
        .tx
        .send(crate::voice::VoiceCmd::RunTranscript(transcript))
        .map_err(|_| "voice thread not running".to_string())
}

/// Dismiss the current voice surface EVERYWHERE (2026-08-15 review): the
/// surfaces broadcast to every overlay window, so a local-state dismiss left
/// live clones on the other monitors. Re-broadcasting `hidden` converges them.
#[tauri::command]
pub async fn voice_dismiss(app: tauri::AppHandle) -> Result<(), String> {
    crate::events::emit_voice_surface(&app, &serde_json::json!({ "surface": "hidden" }))
        .map_err(|e| e.to_string())
}

/// Give THIS overlay window OS keyboard focus without touching its
/// click-through styles (2026-08-15 review): non-exclusive panels (preview /
/// dashboard / privacy) opened without a click — tray, MCP preview_request —
/// had DOM focus in an unfocused window, so Escape/Tab/typing went to the
/// user's foreground app until they clicked inside.
#[tauri::command]
pub async fn focus_overlay(window: tauri::Window) -> Result<(), String> {
    window.set_focus().map_err(|e| e.to_string())
}

/// Open the Activity & Privacy panel on the CALLING window's monitor (bubble
/// overflow "Exclusions…", HUD 🛡, dashboard link — decision #13: the click
/// happened under the cursor, so the calling window is where the user is).
/// The broadcast closes any copy open on another monitor.
#[tauri::command]
pub async fn open_privacy(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<(), String> {
    crate::events::emit_privacy_open_on(&app, window.label()).map_err(|e| e.to_string())
}

/// Open the Dashboard on the CALLING window's monitor (HUD ◎ — decision #13).
/// Same routing rationale + convergence contract as [`open_privacy`]. The tray
/// and single-instance paths have no calling window and route by cursor
/// instead (`events::emit_dashboard_open`).
#[tauri::command]
pub async fn open_dashboard(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<(), String> {
    crate::events::emit_dashboard_open_on(&app, window.label()).map_err(|e| e.to_string())
}

/// The calling window's Context-Preview panel opened (`open: true`) or closed
/// (`open: false`) — decision #13. Open registers the window as THE preview
/// host (core-staged MCP requests route there and queue, never clobbering a
/// mid-edit review) and broadcasts `preview_claimed` so every other window
/// folds its copy. Close releases the slot only if this window still holds it,
/// so a displaced window's teardown can't un-claim its successor.
#[tauri::command]
pub async fn set_preview_host(
    open: bool,
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    host: State<'_, crate::overlay::PreviewHost>,
) -> Result<(), String> {
    if open {
        host.claim(window.label());
        crate::events::emit_preview_claimed(&app, window.label()).map_err(|e| e.to_string())
    } else {
        host.release(window.label());
        Ok(())
    }
}

/// Voice consent gate (doc 13 §8): PTT requires the explicit mic opt-in AND
/// capture actually running (the live toggle, not the persisted decision —
/// voice rides the mechanism, doc 12 §6).
async fn require_voice_consent(state: &State<'_, AppState>) -> Result<(), String> {
    let opted_in = state.consent.lock().await.state().voice_opt_in;
    if !opted_in {
        return Err(
            "microphone consent not granted (doc 13 §8) — call grant_voice_consent first".into(),
        );
    }
    if !capture_is_live(state).await {
        return Err("capture is off — voice rides the capture toggle (doc 12 §6)".into());
    }
    Ok(())
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

// ---------------------------------------------------------------------------
// Start-at-login (the "it's just on when the laptop turns on" contract).
// The registration lives in HKCU Run (tauri-plugin-autostart); the user's
// CHOICE lives in settings `ui.autostart` so a moved/reinstalled exe can be
// re-registered at startup (`main::sync_autostart`) without re-asking.
// ---------------------------------------------------------------------------

/// Is start-at-login currently registered with Windows?
#[tauri::command]
pub async fn get_autostart(app: tauri::AppHandle) -> Result<bool, String> {
    Ok(autostart_enabled(&app))
}

/// Register/unregister start-at-login and persist the choice as `ui.autostart`.
#[tauri::command]
pub async fn set_autostart(
    on: bool,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    apply_autostart(&app, on)?;
    persist_autostart(&state.db, on)
}

/// The registry truth (not the stored preference). Errors read as "not enabled".
pub fn autostart_enabled(app: &tauri::AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// Drive the OS registration to `on`. Idempotent — safe to call every launch.
pub fn apply_autostart(app: &tauri::AppHandle, on: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let autolaunch = app.autolaunch();
    let registered = autolaunch.is_enabled().unwrap_or(false);
    match (on, registered) {
        (true, _) => autolaunch.enable().map_err(|e| e.to_string()), // re-enable repairs a moved exe path
        (false, true) => autolaunch.disable().map_err(|e| e.to_string()),
        (false, false) => Ok(()),
    }
}

/// Persist the user's start-at-login choice into the `ui` settings section
/// (read-modify-write: the section also carries `hud_anchor` etc.).
pub fn persist_autostart(db: &aperture_db::Db, on: bool) -> Result<(), String> {
    use rusqlite::OptionalExtension;
    db.with_conn(|c| {
        let raw: Option<String> = c
            .query_row("SELECT value FROM settings WHERE key = 'ui'", [], |r| r.get(0))
            .optional()?;
        let mut ui: serde_json::Value = raw
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        ui["autostart"] = serde_json::Value::Bool(on);
        c.execute(
            "INSERT INTO settings (key, value) VALUES ('ui', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![ui.to_string()],
        )
        .map(|_| ())
    })
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_contracts::{Event, EventType, PayloadItem};
    use aperture_db::{Db, ScreenContextInsert};

    fn insert_screen(db: &Db, ts: i64, text: &str, redaction_flags: u32) -> i64 {
        let ev = Event {
            id: 0,
            ts,
            r#type: EventType::WindowFocus,
            app: Some("app".into()),
            process: None,
            window_title: None,
            payload: serde_json::json!({}),
            connector_id: None,
            session_id: None,
            redaction_flags,
        };
        let row = ScreenContextInsert {
            ocr_text: Some(text.to_string()),
            ocr_confidence: Some(0.9),
            vlm_summary: None,
            thumb_phash: None,
        };
        db.insert_event_with_context(&ev, Some(&row), None).unwrap()
    }

    fn texts(items: &[PayloadItem]) -> Vec<&str> {
        items
            .iter()
            .map(|i| match i {
                PayloadItem::OcrText { text, .. } => text.as_str(),
                other => panic!("expected only OCR items, got {other:?}"),
            })
            .collect()
    }

    /// Decision #29: newest first, capped rows, and — non-negotiable — excluded
    /// rows (`redaction_flags != 0`) can never enter a payload (doc 13 §4).
    #[test]
    fn recent_ocr_items_are_newest_first_and_never_excluded_rows() {
        let db = Db::open_in_memory().unwrap();
        for (ts, text) in [(1, "one"), (2, "two"), (3, "three"), (4, "four")] {
            insert_screen(&db, ts, text, 0);
        }
        insert_screen(&db, 5, "EXCLUDED", 1); // newest, but excluded

        let items = recent_ocr_items(&db).unwrap();
        assert_eq!(
            texts(&items),
            vec!["four", "three", "two"],
            "3 newest non-excluded snapshots, newest first"
        );
    }

    /// Decision #29: each snapshot is char-capped and blank OCR rows are skipped
    /// (an empty item would waste one of the 3 slots on nothing).
    #[test]
    fn recent_ocr_items_cap_chars_and_skip_blank_rows() {
        let db = Db::open_in_memory().unwrap();
        insert_screen(&db, 1, &"y".repeat(3000), 0);
        insert_screen(&db, 2, "   ", 0); // whitespace-only: skipped

        let items = recent_ocr_items(&db).unwrap();
        let texts = texts(&items);
        assert_eq!(texts.len(), 1, "the blank row is skipped");
        assert_eq!(
            texts[0].len(),
            ESCALATION_OCR_CHARS as usize,
            "snapshot capped at {ESCALATION_OCR_CHARS} chars"
        );
    }
}
