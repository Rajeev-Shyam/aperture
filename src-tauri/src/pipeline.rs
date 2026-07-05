//! The Tier-0 glue the shell owns (doc 02 §4, doc 16 M2/M3): the store adapter
//! (persist-then-notify, doc 15 §1), the frame→OCR→store sink, and the
//! bus→pattern-engine→suggestion task. The shell composes; it never
//! reimplements subsystem logic.

use std::sync::Arc;

use aperture_capture::normalizer::EventStore;
use aperture_capture::sampler::{FrameContext, FrameSink};
use aperture_capture::wgc::EphemeralFrame;
use aperture_contracts::{Event, EventType};
use aperture_db::{Db, ScreenContextInsert};
use aperture_event_bus::EventBus;
use aperture_pattern_engine::{EngineContext, PatternEngine};
use aperture_vision_ocr::FrameProcessor;

/// [`EventStore`] over the encrypted history DB (doc 15 §1: "SQLite is the
/// durable form"). Failures degrade to notify-only with a log — capture keeps
/// running (doc 05 §7 resilience).
pub struct DbEventStore {
    pub db: Arc<Db>,
}

impl EventStore for DbEventStore {
    fn persist(&self, ev: &Event) -> Option<i64> {
        match self.db.insert_event(ev) {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::error!(%e, "event persist failed; continuing notify-only");
                None
            }
        }
    }
}

/// The M2 frame sink (doc 02 §4 steps 3–5): OCR + embed the frame, then attach
/// the results to the frame's event row — or, for heartbeat samples (no event),
/// write event + context + embedding in **one transaction** (doc 16 M2).
/// The frame is consumed here and dropped; raw pixels never persist (doc 05 §2).
pub struct OcrStoreSink {
    pub db: Arc<Db>,
    pub processor: FrameProcessor,
}

impl FrameSink for OcrStoreSink {
    fn submit(&self, frame: EphemeralFrame, ctx: FrameContext) {
        let processed = match self.processor.process(
            frame.bgra(),
            frame.width,
            frame.height,
            Some(ctx.thumb_phash.clone()),
        ) {
            Ok(p) => p,
            Err(e) => {
                // Soft failure: event-only mode for this frame (doc 06 §6).
                tracing::debug!(%e, "frame processing failed; event-only");
                return;
            }
        };
        drop(frame); // explicit: the raw frame dies here (doc 05 §2)

        let row = ScreenContextInsert {
            ocr_text: (!processed.ocr.text.trim().is_empty())
                .then(|| processed.ocr.text.clone()),
            ocr_confidence: Some(processed.ocr.mean_confidence as f64),
            vlm_summary: None,
            thumb_phash: processed.thumb_phash.clone(),
        };
        let embedding = processed.embedding.as_deref();

        let result = if ctx.event_id > 0 {
            // Trigger-sampled: the event row already exists (persist-then-notify).
            self.db.attach_context(ctx.event_id, &row, embedding)
        } else {
            // Heartbeat: no event yet — write event + context + vec atomically.
            let ev = Event {
                id: 0,
                ts: ctx.ts,
                r#type: EventType::WindowFocus,
                app: ctx.identity.app.clone(),
                process: ctx.identity.process.clone(),
                window_title: ctx.identity.window_title.clone(),
                payload: serde_json::json!({ "heartbeat": true }),
                connector_id: None,
                session_id: None,
                redaction_flags: 0,
            };
            self.db
                .insert_event_with_context(&ev, Some(&row), embedding)
                .map(|_| ())
        };
        if let Err(e) = result {
            tracing::error!(%e, "screen_context store failed");
        }
    }
}

/// Spawn the pattern-engine consumer (doc 02 §4 steps 6–8): bus → engine →
/// suggestion rows → `bubble_spec` events to the overlay.
///
/// The connector lookup is the doc 10 seam: **`None` until M4** — trigger rule 3
/// (fresh resumable state) therefore keeps the live engine silent, exactly as
/// specced (no connector, no bubble); the SC2 gate exercises the full path with
/// the contracts fakes. `capture_rx` mirrors the toggle into the engine (rule 7).
pub fn spawn_pattern_task(
    bus: &EventBus,
    db: Arc<Db>,
    mut capture_rx: tokio::sync::broadcast::Receiver<
        aperture_orchestration::toggle_owner::CaptureState,
    >,
    app: tauri::AppHandle,
) -> tokio::task::JoinHandle<()> {
    let mut events = bus.subscribe();
    tokio::spawn(async move {
        let mut engine = PatternEngine::new();
        loop {
            tokio::select! {
                state = capture_rx.recv() => {
                    match state {
                        Ok(s) => engine.set_capture(matches!(
                            s,
                            aperture_orchestration::toggle_owner::CaptureState::On
                        )),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break, // owner gone: shutdown
                    }
                }
                ev = events.recv() => {
                    let ev = match ev {
                        Ok(ev) => ev,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            // At-most-once bus (doc 15 §1): lag is tolerated; the
                            // DB holds the durable history.
                            tracing::debug!(missed = n, "pattern task lagged the bus");
                            continue;
                        }
                        Err(_) => break,
                    };
                    // M4 seam: no connectors yet ⇒ rule 3 keeps the engine silent.
                    let lookup = |_t: &aperture_pattern_engine::normalizer::Token| -> Option<aperture_contracts::ConnectorState> { None };
                    let now_ms = ev.ts;
                    let candidates = engine.on_event(&ev, &EngineContext {
                        connector_lookup: &lookup,
                        now_ms,
                    });
                    for cand in candidates {
                        // Resolve the candidate's connector_state row for
                        // rendering (doc 03 §3; written by connectors at M4 —
                        // with the None-lookup above, candidates cannot fire
                        // before then, so this read is M4-ready, not dead).
                        let state = db.with_conn(|c| {
                            c.query_row(
                                "SELECT id, connector_type, reconstruct_payload, payload_version, captured_ts, stale_after_ts \
                                 FROM connector_state WHERE id = ?1",
                                [&cand.connector_id],
                                |row| {
                                    let payload_text: String = row.get(2)?;
                                    Ok(aperture_contracts::ConnectorState {
                                        id: row.get(0)?,
                                        connector_type: row.get(1)?,
                                        reconstruct_payload: serde_json::from_str(&payload_text)
                                            .unwrap_or(serde_json::Value::Null),
                                        payload_version: row.get(3)?,
                                        captured_ts: row.get(4)?,
                                        stale_after_ts: row.get(5)?,
                                    })
                                },
                            )
                        });
                        let Ok(state) = state else {
                            tracing::warn!(connector_id = %cand.connector_id, "candidate without connector_state row");
                            continue;
                        };
                        let spec = aperture_suggestion_generator::render(&cand, &state, now_ms);
                        // Persist the suggestion row (doc 03 §3) then emit to the overlay.
                        let insert = db.with_conn(|c| {
                            c.execute(
                                "INSERT INTO suggestions (pattern_id, connector_id, source, title, glyph, confidence, state, shown_ts) \
                                 VALUES (?1, ?2, 'local', ?3, ?4, ?5, 'shown', ?6)",
                                rusqlite::params![
                                    cand.pattern_id,
                                    cand.connector_id,
                                    spec.title,
                                    spec.glyph,
                                    spec.confidence,
                                    now_ms
                                ],
                            )?;
                            Ok(c.last_insert_rowid())
                        });
                        match insert {
                            Ok(id) => {
                                let _ = crate::events::emit_bubble_spec(&app, &id.to_string(), &spec);
                            }
                            Err(e) => tracing::error!(%e, "suggestion persist failed"),
                        }
                    }
                }
            }
        }
    })
}
