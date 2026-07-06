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
    /// The engine's current session id, mirrored by the pattern task (M4:
    /// heartbeat rows bypass the bus, so they stamp the *current* session at
    /// insert time — forward-stamping only, never retroactive (ADR-032).
    /// `0` = no session yet ⇒ NULL). Heartbeats only fire while the user is
    /// active (doc 05 §4), so this can lag the sessionizer by at most one
    /// event-to-heartbeat gap — bounded, honest drift.
    pub current_session: Arc<std::sync::atomic::AtomicI64>,
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
            let session = self
                .current_session
                .load(std::sync::atomic::Ordering::SeqCst);
            let ev = Event {
                id: 0,
                ts: ctx.ts,
                r#type: EventType::WindowFocus,
                app: ctx.identity.app.clone(),
                process: ctx.identity.process.clone(),
                window_title: ctx.identity.window_title.clone(),
                payload: serde_json::json!({ "heartbeat": true }),
                connector_id: None,
                // M4: heartbeats bypass the bus, so the sessionizer never sees
                // them — instead they carry the engine's CURRENT session,
                // mirrored via `current_session` (forward-stamp only, ADR-032).
                session_id: (session > 0).then_some(session),
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
/// pattern flush → suggestion rows → `bubble_spec` events to the overlay.
///
/// The connector lookup (trigger rule 3, doc 08 §6) is DB-backed since M4: the
/// freshest non-stale `connector_state` matching the consequent token's
/// resource class, or `None` (no fresh resumable state ⇒ no bubble — stale
/// bubbles are prevented, not apologized for, doc 08 §5). `capture_rx` mirrors
/// the toggle into the engine (rule 7); `feedback_rx` carries bubble feedback
/// into the decay/mute ladder (doc 08 §7); `snooze_until` gates bubble EMISSION
/// only — capture + learning continue while snoozed (ADR-040/Q95);
/// `current_session` mirrors the sessionizer outward for heartbeat stamping (M4).
pub fn spawn_pattern_task(
    bus: &EventBus,
    db: Arc<Db>,
    mut capture_rx: tokio::sync::broadcast::Receiver<
        aperture_orchestration::toggle_owner::CaptureState,
    >,
    mut feedback_rx: tokio::sync::mpsc::UnboundedReceiver<(
        i64,
        aperture_pattern_engine::FeedbackEvent,
    )>,
    snooze_until: Arc<std::sync::atomic::AtomicI64>,
    current_session: Arc<std::sync::atomic::AtomicI64>,
    app: tauri::AppHandle,
) -> tokio::task::JoinHandle<()> {
    let mut events = bus.subscribe();
    tokio::spawn(async move {
        // Hydrate the session id source past the DB's max (doc 03 §3:
        // session_id is monotonic; ADR-032 forbids retro-sessionizing, so a
        // restart must never reuse persisted ids).
        let next_session = db
            .with_conn(|c| {
                c.query_row(
                    "SELECT COALESCE(MAX(session_id), 0) + 1 FROM events",
                    [],
                    |r| r.get::<_, i64>(0),
                )
            })
            .unwrap_or(1);
        let mut engine = PatternEngine::with_next_session_id(next_session);
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
                fb = feedback_rx.recv() => {
                    let Some((pattern_id, fb)) = fb else { break }; // shell gone
                    // Decay / reinforce / mute (doc 08 §7); flush so the ladder
                    // survives restarts (dismiss_decay is a patterns column).
                    engine.apply_feedback(pattern_id, fb, epoch_ms());
                    flush_patterns(&db, &mut engine);
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
                    // Trigger rule 3 (doc 08 §6): the freshest non-stale
                    // connector_state for the consequent's resource class —
                    // written by spawn_connector_task, read here. Sub-ms
                    // indexed read (idx_conn_type_ts).
                    let now_ms = ev.ts;
                    let lookup_db = Arc::clone(&db);
                    let lookup = move |t: &aperture_pattern_engine::normalizer::Token| -> Option<aperture_contracts::ConnectorState> {
                        lookup_connector_state(&lookup_db, t, now_ms)
                    };
                    let candidates = engine.on_event(&ev, &EngineContext {
                        connector_lookup: &lookup,
                        now_ms,
                    });

                    // Stamp the assigned session onto the durable row (doc 03
                    // §3): the engine sessionizes in-memory; SQLite is the
                    // durable truth (doc 15 §1) — without this every events row
                    // keeps NULL session_id, unrecoverably (ADR-032).
                    if ev.id > 0 {
                        if let Some(session) = engine.last_session() {
                            // Mirror outward for heartbeat stamping (M4).
                            current_session
                                .store(session, std::sync::atomic::Ordering::SeqCst);
                            if let Err(e) = db.with_conn(|c| {
                                c.execute(
                                    "UPDATE events SET session_id = ?1 WHERE id = ?2",
                                    rusqlite::params![session, ev.id],
                                )
                                .map(|_| ())
                            }) {
                                tracing::error!(%e, "session stamp failed");
                            }
                        }
                    }

                    // Flush dirty pattern rows BEFORE the suggestion insert:
                    // candidates carry engine-local negative ids, and
                    // suggestions.pattern_id is an FK into patterns — inserting
                    // an unflushed id is a guaranteed constraint failure.
                    let remap = flush_patterns(&db, &mut engine);
                    for cand in candidates {
                        let pattern_id =
                            remap.get(&cand.pattern_id).copied().unwrap_or(cand.pattern_id);
                        // Resolve the candidate's connector_state row for
                        // rendering (doc 03 §3).
                        let Ok(Some(state)) = db.read_connector_state(&cand.connector_id) else {
                            tracing::warn!(connector_id = %cand.connector_id, "candidate without connector_state row");
                            continue;
                        };
                        let spec = aperture_suggestion_generator::render(&cand, &state, now_ms);
                        // ADR-040/Q95: while snoozed, rows queue (learning
                        // continues) and surface via list_suggestions when the
                        // snooze lifts; only EMISSION is silenced.
                        let snoozed =
                            snooze_until.load(std::sync::atomic::Ordering::SeqCst) > now_ms;
                        // Persist the suggestion row (doc 03 §3) then emit to the overlay.
                        let insert = db.with_conn(|c| {
                            c.execute(
                                "INSERT INTO suggestions (pattern_id, connector_id, source, title, glyph, confidence, state, shown_ts) \
                                 VALUES (?1, ?2, 'local', ?3, ?4, ?5, ?6, ?7)",
                                rusqlite::params![
                                    pattern_id,
                                    cand.connector_id,
                                    spec.title,
                                    spec.glyph,
                                    spec.confidence,
                                    if snoozed { "queued" } else { "shown" },
                                    (!snoozed).then_some(now_ms),
                                ],
                            )?;
                            Ok(c.last_insert_rowid())
                        });
                        match insert {
                            Ok(id) if !snoozed => {
                                let _ = crate::events::emit_bubble_spec(&app, &id.to_string(), &spec);
                            }
                            Ok(_) => {}
                            Err(e) => tracing::error!(%e, "suggestion persist failed"),
                        }
                    }
                }
            }
        }
    })
}

/// Trigger rule 3's lookup (doc 08 §6): map the consequent token's resource
/// class to a connector type, fetch the freshest non-stale states, and — for
/// extension-scoped tokens (`doc:xlsx`, `ide:rs`) — require the payload path to
/// match the extension. Type-level freshest-wins otherwise (`youtube`,
/// `url:<domain>`): the bubble resumes the newest captured resource of that
/// class, which is exactly the doc 08 §6 action_template semantics.
fn lookup_connector_state(
    db: &Db,
    token: &aperture_pattern_engine::normalizer::Token,
    now_ms: i64,
) -> Option<aperture_contracts::ConnectorState> {
    let resource = token.resource_class.as_deref()?;
    let (connector_type, ext_filter): (&str, Option<&str>) = if resource == "youtube" {
        ("youtube", None)
    } else if let Some(ext) = resource.strip_prefix("doc:") {
        ("document", Some(ext))
    } else if let Some(ext) = resource.strip_prefix("ide:") {
        ("ide", Some(ext))
    } else if resource.starts_with("url:") {
        ("browser", None)
    } else {
        return None;
    };
    let states = db
        .fresh_connector_states(connector_type, now_ms, 8)
        .unwrap_or_default();
    states.into_iter().find(|st| match ext_filter {
        None => true,
        Some(ext) => st
            .reconstruct_payload
            .get("path")
            .and_then(|p| p.as_str())
            .is_some_and(|p| p.to_ascii_lowercase().ends_with(&format!(".{ext}"))),
    })
}

/// Spawn the connector-capture consumer (Path A step 4, doc 02 §4; doc 10 §1):
/// bus → (secondary-event derivation) → `registry.capture` → `connector_state`
/// row (+ `events.connector_id` stamp).
///
/// Two responsibilities:
/// 1. **Connector heuristics** (doc 03 §2): focus/title events of known editors
///    derive `document_state` / `ide_state` events (path ladders, doc 10 §4-5),
///    persisted-then-published like any event — the pattern engine then
///    tokenizes them (`doc:xlsx`, `ide:rs`) and this same task captures them.
/// 2. **State capture**: any event a connector claims produces a
///    `connector_state` row. Captures of the *same resource* (natural key —
///    e.g. the same video id) refresh the existing fresh row in place instead
///    of piling up rows per media tick; a stale/pruned row gets a new insert.
///
/// Ladder work (fs existence checks, `.lnk` COM resolution, workspace walks)
/// runs on blocking threads — the bus consumer never stalls the runtime.
pub fn spawn_connector_task(
    bus: &EventBus,
    db: Arc<Db>,
    registry: Arc<aperture_connectors::ConnectorRegistry>,
) -> tokio::task::JoinHandle<()> {
    let mut events = bus.subscribe();
    let bus = bus.clone();
    tokio::spawn(async move {
        let deriver = Arc::new(aperture_connectors::SecondaryDeriver::new());
        // (connector_type, natural_key) → (row_id, stale_after_ts): the
        // coalescing map. In-memory only — a restart just starts new rows and
        // retention prunes the old ones (doc 03 §6).
        let mut coalesce: std::collections::HashMap<(String, String), (String, Option<i64>)> =
            std::collections::HashMap::new();
        loop {
            let ev = match events.recv().await {
                Ok(ev) => ev,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!(missed = n, "connector task lagged the bus");
                    continue;
                }
                Err(_) => break,
            };

            // 1. Secondary derivation (WindowFocus/WindowOpen of known editors
            //    → document_state / ide_state). Heavy (fs/COM/sqlite) ⇒ blocking.
            let d = Arc::clone(&deriver);
            let src = ev.clone();
            let derived = tokio::task::spawn_blocking(move || d.derive(&src))
                .await
                .ok()
                .flatten();
            if let Some(mut sev) = derived {
                sev.session_id = ev.session_id; // same moment, same session
                match db.insert_event(&sev) {
                    Ok(id) => {
                        sev.id = id;
                        let _ = bus.publish(sev); // this task re-receives it below
                    }
                    Err(e) => tracing::error!(%e, "secondary event persist failed"),
                }
            }

            // 2. Connector capture on THIS event (Navigation/MediaState arrive
            //    from capture; DocumentState/IdeState from step 1's publish).
            let reg = Arc::clone(&registry);
            let src = ev.clone();
            let captured = tokio::task::spawn_blocking(move || reg.capture(&src))
                .await
                .ok()
                .flatten();
            let Some(state) = captured else { continue };

            let key = aperture_connectors::natural_key(
                &state.connector_type,
                &state.reconstruct_payload,
            );
            let used_id = match key {
                Some(k) => {
                    let map_key = (state.connector_type.clone(), k);
                    match coalesce.get(&map_key) {
                        // Same resource, existing row still fresh ⇒ refresh in
                        // place (freshest position wins, doc 10 §3).
                        Some((row_id, stale)) if stale.is_none_or(|s| s > state.captured_ts) => {
                            match db.refresh_connector_state(
                                row_id,
                                &state.reconstruct_payload,
                                state.captured_ts,
                                state.stale_after_ts,
                            ) {
                                Ok(true) => {
                                    let id = row_id.clone();
                                    coalesce.insert(map_key, (id.clone(), state.stale_after_ts));
                                    Some(id)
                                }
                                Ok(false) | Err(_) => {
                                    insert_state(&db, &state).then(|| {
                                        coalesce.insert(
                                            map_key,
                                            (state.id.clone(), state.stale_after_ts),
                                        );
                                        state.id.clone()
                                    })
                                }
                            }
                        }
                        _ => insert_state(&db, &state).then(|| {
                            if coalesce.len() >= 1024 {
                                coalesce.clear(); // crude bound; repopulates live
                            }
                            coalesce.insert(map_key, (state.id.clone(), state.stale_after_ts));
                            state.id.clone()
                        }),
                    }
                }
                None => insert_state(&db, &state).then(|| state.id.clone()),
            };

            // 3. Stamp the event row with its resumable handle (doc 03 §3).
            if let Some(connector_id) = used_id {
                if ev.id > 0 {
                    if let Err(e) = db.set_event_connector(ev.id, &connector_id) {
                        tracing::error!(%e, "event connector stamp failed");
                    }
                }
            }
        }
    })
}

fn insert_state(db: &Db, state: &aperture_contracts::ConnectorState) -> bool {
    match db.insert_connector_state(state) {
        Ok(()) => true,
        Err(e) => {
            tracing::error!(%e, "connector_state insert failed");
            false
        }
    }
}

/// Flush the engine's dirty pattern rows to the `patterns` table (doc 03 §3,
/// doc 08 §4/§7) and swap its local negative ids for the DB-assigned ones via
/// `mark_flushed`. Returns the local→DB id remap so in-flight candidates can
/// persist with a real FK target. Rows that fail stay dirty and retry on the
/// next call. Upsert is by `signature` (UNIQUE), so a restarted engine
/// converges onto the same rows it wrote before.
fn flush_patterns(
    db: &Db,
    engine: &mut PatternEngine,
) -> std::collections::HashMap<i64, i64> {
    // Collect owned copies first — dirty_rows() borrows the engine.
    let dirty: Vec<(String, i64, i64, i64, f64, i64, f64)> = engine
        .dirty_rows()
        .into_iter()
        .map(|(sig, row)| {
            // Signature shape: "a | b ⇒ c" — antecedent joined by " | ",
            // so n = antecedent count + 1 (gram length, doc 08 §4).
            let n = sig.split(" | ").count() as i64 + 1;
            (
                sig.to_string(),
                row.pattern_id,
                n,
                row.stats.weighted_support.round() as i64,
                row.stats.confidence(),
                row.stats.last_updated_ms,
                row.stats.dismiss_decay,
            )
        })
        .collect();

    let mut remap = std::collections::HashMap::new();
    for (sig, local_id, n, support, confidence, last_seen, decay) in dirty {
        let flushed = db.with_conn(|c| {
            c.query_row(
                "INSERT INTO patterns (signature, n, support, confidence, last_seen, dismiss_decay) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(signature) DO UPDATE SET \
                   support = excluded.support, confidence = excluded.confidence, \
                   last_seen = excluded.last_seen, dismiss_decay = excluded.dismiss_decay \
                 RETURNING id",
                rusqlite::params![sig, n, support, confidence, last_seen, decay],
                |r| r.get::<_, i64>(0),
            )
        });
        match flushed {
            Ok(db_id) => {
                engine.mark_flushed(&sig, db_id);
                if local_id != db_id {
                    remap.insert(local_id, db_id);
                }
            }
            Err(e) => {
                tracing::error!(%e, signature = %sig, "pattern flush failed; will retry")
            }
        }
    }
    remap
}

/// Epoch ms for feedback/snooze timestamps (the event path uses `ev.ts`).
pub(crate) fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
