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
use aperture_orchestration::tier_router::{
    OcrSignal, WakeDecision, LOW_TEXT_DENSITY, WEAK_OCR_CONFIDENCE,
};
use aperture_orchestration::toggle_owner::CaptureState;
use aperture_orchestration::OrchestratedSystem;
use aperture_pattern_engine::{EngineContext, PatternEngine};
use aperture_vision_ocr::vlm_layer::prepare_image;
use aperture_vision_ocr::{FrameProcessor, VlmLayer};

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
    /// The orchestration handle for the M5 VLM enrichment path (doc 06 §3/§5):
    /// the wake gate + single GPU scheduler live here. `None` degrades the sink
    /// to OCR-only (doc 06 §6) — never the case in production, where `main`
    /// always wires it.
    pub orchestration: Option<Arc<tokio::sync::Mutex<OrchestratedSystem>>>,
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

        // M5 VLM enrichment gate (doc 06 §4 branch (b), doc 02 Path A). A *cheap*
        // local pre-filter decides if this frame could earn a VLM wake — "rich
        // frame, weak OCR" is the only proactive-path trigger a frame sink can see
        // (pattern/user triggers arrive via other paths). We encode the single
        // image HERE, before the frame drops (downscale ≤1024 px, JPEG q85 —
        // doc 06 §3), and only for a candidate: the common strong-OCR frame pays
        // nothing. The authoritative stateful gate (per-app debounce + hourly
        // ceiling + budget, ADR-032) runs off the bubble path in maybe_enrich_vlm.
        let ocr = &processed.ocr;
        // Thresholds come from `tier_router` — the SAME source the authoritative
        // gate reads (they were duplicated in `vlm_gating`; a single source keeps
        // the pre-filter and the gate from silently diverging at M5 tuning).
        let vlm_jpeg = (self.orchestration.is_some()
            && ocr.mean_confidence < WEAK_OCR_CONFIDENCE
            && ocr.text_density() as f32 > LOW_TEXT_DENSITY)
            .then(|| prepare_image(frame.bgra(), frame.width, frame.height).ok())
            .flatten();
        let vlm_signal = OcrSignal {
            confidence: ocr.mean_confidence,
            text_density: ocr.text_density() as f32,
        };
        let app_key = ctx
            .identity
            .app
            .clone()
            .or_else(|| ctx.identity.process.clone())
            .unwrap_or_default();

        drop(frame); // explicit: the raw frame dies here (doc 05 §2)

        let row = ScreenContextInsert {
            ocr_text: (!processed.ocr.text.trim().is_empty())
                .then(|| processed.ocr.text.clone()),
            ocr_confidence: Some(processed.ocr.mean_confidence as f64),
            vlm_summary: None,
            thumb_phash: processed.thumb_phash.clone(),
        };
        let embedding = processed.embedding.as_deref();

        // Store, capturing the row's event_id — the VLM summary attaches to it
        // once the (async, off-path) job returns (doc 06 §5 Layer B).
        let event_id: Option<i64> = if ctx.event_id > 0 {
            // Trigger-sampled: the event row already exists (persist-then-notify).
            match self.db.attach_context(ctx.event_id, &row, embedding) {
                Ok(()) => Some(ctx.event_id),
                Err(e) => {
                    tracing::error!(%e, "screen_context store failed");
                    None
                }
            }
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
            match self.db.insert_event_with_context(&ev, Some(&row), embedding) {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::error!(%e, "screen_context store failed");
                    None
                }
            }
        };

        // Off the bubble path (doc 02 Path A: the VLM NEVER gates a bubble). Only
        // when a candidate frame encoded, its row landed, and orchestration is
        // wired — otherwise this frame stays OCR-only (doc 06 §6).
        if let (Some(jpeg), Some(event_id), Some(orch)) =
            (vlm_jpeg, event_id, self.orchestration.clone())
        {
            let db = Arc::clone(&self.db);
            tokio::spawn(maybe_enrich_vlm(
                orch, db, event_id, jpeg, app_key, vlm_signal, ctx.ts,
            ));
        }
    }
}

/// Off-the-bubble-path VLM enrichment (doc 06 §3/§5 Layer B; doc 02 Path A). Runs
/// the authoritative stateful wake gate (per-app debounce + adaptive 3–10/h
/// ceiling that protects voice, ADR-032) and, on a `Wake`, enqueues one `prio:50`
/// pattern-VLM job through the single GPU mutex, attaching the structured scene
/// JSON to the frame's `screen_context` row. Every failure is **soft** — the
/// bubble already fired on OCR; this only improves the *next* pattern cycle.
async fn maybe_enrich_vlm(
    orch: Arc<tokio::sync::Mutex<OrchestratedSystem>>,
    db: Arc<Db>,
    event_id: i64,
    jpeg: Vec<u8>,
    app_key: String,
    signal: OcrSignal,
    now_ms: i64,
) {
    // Run the gate + grab the scheduler under ONE short lock, then release before
    // the GPU wait — the orchestration mutex serializes the toggle + lifecycle,
    // it must never be held across a job (doc 12).
    let scheduler = {
        let mut orch = orch.lock().await;
        // The AUTHORITATIVE capture state, read under the lock. `turn_off` holds
        // this same mutex through sidecar teardown (doc 12 §6), so a task that
        // queued on this lock while an OFF was in flight observes OFF and skips.
        // A hardcoded `true` would let the enqueue below re-spawn vlm-host and
        // reload ~5–6 GB into VRAM *after* the user asked to stop — an SC6 breach
        // (doc 05 §5). The gate maps `!capture_on` to Skip(CaptureOff).
        let capture_on = orch.toggle().state() == CaptureState::On;
        let mutex_free = orch.mutex_likely_free();
        // The gate reads only app/process off the event (its debounce key); a
        // minimal event carries exactly that.
        let ev = Event {
            id: 0,
            ts: now_ms,
            r#type: EventType::WindowFocus,
            app: (!app_key.is_empty()).then_some(app_key),
            process: None,
            window_title: None,
            payload: serde_json::json!({}),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        };
        let decision = orch.wake_gate().should_wake_vlm(
            &ev,
            signal,
            now_ms,
            capture_on, // read under the lock above — never assume ON
            mutex_free,
            false, // pattern_requested: doc-08 disambiguation arrives on another path
            false, // user_explicit: enrichment requests come from the UI, not here
            true, // budget_projection_ok: advisory — the scheduler's enqueue
                  // projection is the final word on the 7.0 GB ceiling (doc 06 §4)
        );
        match decision {
            WakeDecision::Wake(_) => {
                // Telemetry feeds the M5 adaptive 3–10/h band assertion (ADR-032).
                orch.record_vlm_wake(now_ms);
                orch.scheduler()
            }
            WakeDecision::Skip(reason) => {
                tracing::trace!(?reason, "vlm wake skipped (doc 06 §4)");
                return;
            }
        }
    };

    // Wake committed: the prio:50 job runs through the mutex; the scheduler's R1
    // projection is what actually protects the ceiling (doc 12 §4).
    match VlmLayer::new(scheduler).understand(jpeg).await {
        Ok(scene) => match serde_json::to_string(&scene) {
            // doc 06 §1/§5: vlm_summary is the structured scene JSON.
            Ok(summary) => match db.attach_vlm_summary(event_id, &summary) {
                Ok(true) => tracing::debug!(event_id, "vlm scene summary attached"),
                Ok(false) => tracing::debug!(event_id, "vlm summary target row gone (pruned)"),
                Err(e) => tracing::error!(%e, "vlm summary attach failed"),
            },
            Err(e) => tracing::error!(%e, "vlm scene serialize failed"),
        },
        // BudgetRefused / Deadline / SidecarDown are all soft (already logged in
        // VlmLayer::understand): the frame stays OCR-only (doc 06 §6).
        Err(_) => {}
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

        // Decision #17: the engine's tunables come from the `pattern_engine`
        // settings block (seeded from settings.default.json), constants as
        // fallback. Re-read on the daily maintenance tick below — the same
        // per-pass cadence the retention job uses; the shell has no push-style
        // settings-reload path yet.
        let mut engine_cfg = engine_config_from_settings(&db);
        engine.set_config(engine_cfg.clone());

        // CONN-M2: hydrate the decay/mute ladder from the persisted `patterns`
        // table so a dismissed/muted suggestion stays suppressed across a restart
        // (doc 08 §7). Without this the engine re-mines every signature cold
        // (dismiss_decay = 1.0, mute lost) and re-nags. Read-once, before the loop.
        let hydrated = db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, signature, support, confidence, last_seen, dismiss_decay, \
                        muted_until, recent_dismissals \
                 FROM patterns WHERE signature IS NOT NULL",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    let recent_json: Option<String> = r.get(7)?;
                    let recent_dismissals = recent_json
                        .and_then(|s| serde_json::from_str::<Vec<i64>>(&s).ok())
                        .unwrap_or_default();
                    Ok(aperture_pattern_engine::PersistedPattern {
                        pattern_id: r.get(0)?,
                        signature: r.get(1)?,
                        support: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                        confidence: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                        last_seen: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        dismiss_decay: r.get::<_, Option<f64>>(5)?.unwrap_or(1.0),
                        muted_until: r.get(6)?,
                        recent_dismissals,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        });
        match hydrated {
            Ok(rows) => {
                let n = rows.len();
                let unparseable = engine.hydrate(rows);
                if n > 0 {
                    tracing::info!(patterns = n, "hydrated decay/mute ladder from disk (CONN-M2)");
                }
                // Rows the engine could not parse can never fire, take feedback,
                // or decay — and the engine prune (the sole patterns deleter,
                // decision #18) only sees cached rows, so drop them here or they
                // linger forever.
                if !unparseable.is_empty() {
                    let n = unparseable.len();
                    if let Err(e) = delete_pattern_rows(&db, &unparseable) {
                        tracing::error!(%e, "unparseable pattern cleanup failed");
                    } else {
                        tracing::warn!(dropped = n, "unparseable persisted patterns deleted");
                    }
                }
            }
            Err(e) => tracing::error!(%e, "pattern hydrate read failed; engine starts cold"),
        }

        // Pattern-table maintenance (doc 08 §9) — checked daily; the engine's
        // own support-decay math decides what is actually stale. This decay
        // prune + its DB mirror is the ONE owner of `patterns` deletion (owner
        // decision #18, 2026-08-16): the retention job's independent age-based
        // pattern prune was removed (crates/db/src/retention.rs), so the two
        // timers can no longer disagree. The same tick re-reads the engine's
        // settings block (#17).
        let mut prune_tick = tokio::time::interval(std::time::Duration::from_secs(24 * 3600));
        prune_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        prune_tick.tick().await; // consume the immediate first tick

        loop {
            tokio::select! {
                _ = prune_tick.tick() => {
                    let doomed = engine.prune(epoch_ms());
                    if !doomed.is_empty() {
                        // Mirror to the patterns table or they re-hydrate.
                        let n = doomed.len();
                        match delete_pattern_rows(&db, &doomed) {
                            Ok(()) => tracing::info!(pruned = n, "stale patterns pruned (doc 08 §9)"),
                            Err(e) => tracing::error!(%e, "pattern prune DB mirror failed"),
                        }
                    }
                    // Decision #17: pick up settings edits at the daily cadence.
                    let fresh_cfg = engine_config_from_settings(&db);
                    if fresh_cfg != engine_cfg {
                        tracing::info!("pattern_engine settings changed; reconfiguring engine (#17)");
                        engine.set_config(fresh_cfg.clone());
                        engine_cfg = fresh_cfg;
                    }
                }
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
                    // Decay / reinforce / mute (doc 08 §7); flush persists the
                    // ladder (incl. mute state, migration 0002) into the patterns
                    // table, and the startup hydrate above restores it after a
                    // restart — so a dismissed/muted suggestion stays suppressed
                    // rather than re-nagging (CONN-M2, resolved).
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
                    for mut cand in candidates {
                        let pattern_id =
                            remap.get(&cand.pattern_id).copied().unwrap_or(cand.pattern_id);
                        // Resolve the candidate's connector_state row for
                        // rendering (doc 03 §3). A "switch to X" candidate
                        // (decision #15) carries a sentinel ref instead of a row
                        // id — synthesize + persist its `app_focus` state row so
                        // the bubble rides the identical pipeline: FK-valid
                        // suggestions row, queued-resurface via
                        // `list_suggestions`, and Path B click-resolve
                        // (`bubble_click` dispatches `app_focus` rows itself).
                        let state = match aperture_pattern_engine::app_focus_target(
                            &cand.connector_id,
                        ) {
                            Some(process) => {
                                let state = app_focus_state(process, now_ms);
                                if let Err(e) = db.insert_connector_state(&state) {
                                    tracing::error!(%e, "app_focus state persist failed");
                                    continue;
                                }
                                cand.connector_id = state.id.clone();
                                state
                            }
                            None => match db.read_connector_state(&cand.connector_id) {
                                Ok(Some(state)) => state,
                                _ => {
                                    tracing::warn!(connector_id = %cand.connector_id, "candidate without connector_state row");
                                    continue;
                                }
                            },
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
    // Map the consequent token's resource class → (connector type, match filter).
    // youtube is type-level (the token carries no video id). doc/ide match by
    // extension. url matches by HOST: a `url:docs.rs` consequent must resume a
    // docs.rs tab, not merely the newest browser tab of any site (CONN-H1).
    #[derive(Clone, Copy)]
    enum Filter<'a> {
        None,
        Ext(&'a str),
        Host(&'a str),
    }
    let (connector_type, filter): (&str, Filter) = if resource == "youtube" {
        ("youtube", Filter::None)
    } else if let Some(ext) = resource.strip_prefix("doc:") {
        ("document", Filter::Ext(ext))
    } else if let Some(ext) = resource.strip_prefix("ide:") {
        ("ide", Filter::Ext(ext))
    } else if let Some(host) = resource.strip_prefix("url:") {
        ("browser", Filter::Host(host))
    } else {
        return None;
    };
    // Filtered lookups match in Rust after the DB's freshest-N fetch, so pull a
    // generous window — a matching state just past the newest few must not be
    // missed (CONN-M3). Type-level (youtube) only ever needs the newest row.
    let limit = if matches!(filter, Filter::None) { 1 } else { 32 };
    let states = db
        .fresh_connector_states(connector_type, now_ms, limit)
        .unwrap_or_default();
    states.into_iter().find(|st| match filter {
        Filter::None => true,
        Filter::Ext(ext) => st
            .reconstruct_payload
            .get("path")
            .and_then(|p| p.as_str())
            .is_some_and(|p| p.to_ascii_lowercase().ends_with(&format!(".{ext}"))),
        // CONN-H1: reverse the `url:<host>` token through the tokenizer's own
        // `host_of`, so a stored URL's host is normalized (lowercased, `www.`-
        // stripped) identically to the token before comparison.
        Filter::Host(host) => st
            .reconstruct_payload
            .get("url")
            .and_then(|u| u.as_str())
            .and_then(aperture_pattern_engine::normalizer::host_of)
            .is_some_and(|h| h.as_str() == host),
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
        // (connector_type, natural_key) → (row_id, stale_after_ts, captured_ts,
        // position_rank): the coalescing map. In-memory only — a restart just
        // starts new rows and retention prunes the old ones (doc 03 §6).
        // captured_ts + rank close CONN-M1: a later position-less navigation
        // must not clobber a known media position (ADR-027 source hierarchy).
        let mut coalesce: std::collections::HashMap<
            (String, String),
            (String, Option<i64>, i64, u8),
        > = std::collections::HashMap::new();
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
            let new_rank = position_rank(&state.reconstruct_payload);
            let used_id = match key {
                Some(k) => {
                    let map_key = (state.connector_type.clone(), k);
                    match coalesce.get(&map_key) {
                        // Same resource, existing row still fresh. CONN-M1: only
                        // refresh when the NEW capture wins — an equal-or-better
                        // position source, or a same-rank later observation
                        // (freshest position wins, doc 10 §3 / ADR-027). A later
                        // position-LESS navigation over a known position keeps
                        // the old row (and still stamps the event with it).
                        Some((row_id, stale, prev_ts, prev_rank))
                            if stale.is_none_or(|s| s > state.captured_ts) =>
                        {
                            // A lower-ranked capture NEVER overwrites a fresh
                            // higher-ranked row, regardless of timestamps — the
                            // ts comparison only orders captures of equal rank
                            // (out-of-order delivery is real: media ticks and
                            // UIA-derived navigations race). Same-rank older
                            // captures are dropped too.
                            let new_wins = new_rank > *prev_rank
                                || (new_rank == *prev_rank && state.captured_ts >= *prev_ts);
                            if !new_wins {
                                tracing::debug!(
                                    "coalesce kept the higher-ranked/newer position (CONN-M1)"
                                );
                                Some(row_id.clone())
                            } else {
                                match db.refresh_connector_state(
                                    row_id,
                                    &state.reconstruct_payload,
                                    state.captured_ts,
                                    state.stale_after_ts,
                                ) {
                                    Ok(true) => {
                                        let id = row_id.clone();
                                        coalesce.insert(
                                            map_key,
                                            (id.clone(), state.stale_after_ts, state.captured_ts, new_rank),
                                        );
                                        Some(id)
                                    }
                                    Ok(false) | Err(_) => {
                                        insert_state(&db, &state).then(|| {
                                            coalesce.insert(
                                                map_key,
                                                (state.id.clone(), state.stale_after_ts, state.captured_ts, new_rank),
                                            );
                                            state.id.clone()
                                        })
                                    }
                                }
                            }
                        }
                        _ => insert_state(&db, &state).then(|| {
                            if coalesce.len() >= 1024 {
                                coalesce.clear(); // crude bound; repopulates live
                            }
                            coalesce.insert(
                                map_key,
                                (state.id.clone(), state.stale_after_ts, state.captured_ts, new_rank),
                            );
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

/// Rank a capture's position information for the CONN-M1 coalesce guard
/// (ADR-027 source hierarchy): a payload carrying a known playback position
/// (`position_s`, the youtube connector's v1 key) outranks one without.
fn position_rank(payload: &serde_json::Value) -> u8 {
    match payload.get("position_s") {
        Some(v) if !v.is_null() => 1,
        _ => 0,
    }
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
    // Row tuple: (sig, local_id, n, support, confidence, last_seen, decay,
    //             muted_until, recent_dismissals_json).
    let dirty: Vec<(String, i64, i64, i64, f64, i64, f64, Option<i64>, String)> = engine
        .dirty_rows()
        .into_iter()
        .map(|(sig, row)| {
            // Signature shape: "a | b ⇒ c" — antecedent joined by " | ",
            // so n = antecedent count + 1 (gram length, doc 08 §4).
            let n = sig.split(" | ").count() as i64 + 1;
            // CONN-M2: persist the mute half of the ladder so hydrate can restore
            // it. recent_dismissals → a JSON i64 array (empty list on the rare
            // serialize failure — never poison the whole flush).
            let recent = serde_json::to_string(&row.mute.recent_dismissals)
                .unwrap_or_else(|_| "[]".to_string());
            (
                sig.to_string(),
                row.pattern_id,
                n,
                row.stats.weighted_support.round() as i64,
                row.stats.confidence(),
                row.stats.last_updated_ms,
                row.stats.dismiss_decay,
                row.mute.muted_until,
                recent,
            )
        })
        .collect();

    let mut remap = std::collections::HashMap::new();
    for (sig, local_id, n, support, confidence, last_seen, decay, muted_until, recent) in dirty {
        let flushed = db.with_conn(|c| {
            c.query_row(
                "INSERT INTO patterns \
                   (signature, n, support, confidence, last_seen, dismiss_decay, muted_until, recent_dismissals) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT(signature) DO UPDATE SET \
                   support = excluded.support, confidence = excluded.confidence, \
                   last_seen = excluded.last_seen, dismiss_decay = excluded.dismiss_decay, \
                   muted_until = excluded.muted_until, recent_dismissals = excluded.recent_dismissals \
                 RETURNING id",
                rusqlite::params![sig, n, support, confidence, last_seen, decay, muted_until, recent],
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

/// The engine's runtime tunables from the `pattern_engine` settings section
/// (decision #17): seeded defaults with the crate constants as fallback —
/// missing/invalid keys can never weaken the engine (same posture as
/// `retention_policy_from_settings` in main.rs).
fn engine_config_from_settings(db: &Db) -> aperture_pattern_engine::config::EngineConfig {
    let section = db
        .get_setting("pattern_engine")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    aperture_pattern_engine::config::EngineConfig::from_settings(&section)
}

/// Delete pattern rows by signature, detaching suggestion history first
/// (suggestions outlive their pattern). The ONLY policy-driven `patterns`
/// deleter is the engine's decay prune mirrored through here (decision #18);
/// hydrate-time cleanup of unparseable rows shares the path.
fn delete_pattern_rows(db: &Db, signatures: &[String]) -> Result<(), aperture_db::DbError> {
    db.with_conn(|c| {
        for sig in signatures {
            c.execute(
                "UPDATE suggestions SET pattern_id = NULL WHERE pattern_id IN \
                 (SELECT id FROM patterns WHERE signature = ?1)",
                [sig],
            )?;
            c.execute("DELETE FROM patterns WHERE signature = ?1", [sig])?;
        }
        Ok(())
    })
}

/// How long a synthetic `app_focus` state stays clickable. A "switch to X now"
/// prompt is momentary — the row exists so the bubble can ride the normal
/// suggestion pipeline (FK, resurface, Path B); retention reaps it once stale.
const APP_FOCUS_TTL_MS: i64 = 24 * 3_600_000;

/// Synthesize the connector-state row backing one "switch to X" bubble (owner
/// decision #15, 2026-08-16). `connector_type = "app_focus"` is resolved by
/// `bubble_click` itself (Path B analog — commands/mod.rs); the payload carries
/// the raw process name to dispatch and the display name `{app}` the template
/// expands.
fn app_focus_state(process: &str, now_ms: i64) -> aperture_contracts::ConnectorState {
    let stem = process.trim_end_matches(".exe").trim_end_matches(".EXE");
    let mut display = String::with_capacity(stem.len());
    let mut chars = stem.chars();
    if let Some(first) = chars.next() {
        display.extend(first.to_uppercase());
        display.push_str(chars.as_str());
    }
    aperture_contracts::ConnectorState {
        id: uuid::Uuid::new_v4().to_string(),
        connector_type: "app_focus".to_string(),
        reconstruct_payload: serde_json::json!({ "process": process, "app": display }),
        payload_version: 1,
        captured_ts: now_ms,
        stale_after_ts: Some(now_ms + APP_FOCUS_TTL_MS),
    }
}

/// Epoch ms for feedback/snooze timestamps (the event path uses `ev.ts`).
pub(crate) fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Surface cloud-returned suggestions as real bubbles — US3's last leg
/// (doc 09 §4; doc 20's amended acceptance criterion). Shared by the MCP
/// `aperture_submit_suggestions` tool and the push-transport `preview_send`
/// result path, so BOTH transports end in the same on-screen rendering.
///
/// Per suggestion: connector re-validation produces a fresh `ConnectorState`
/// (only connectors act, ADR-035) which is persisted so a click resolves
/// through the same Path B as a local bubble; the durable `suggestions` row is
/// written (`source = 'claude'`); the bubble emits unless snoozed (ADR-040:
/// queued rows surface when the snooze lifts). `connector_type == "none"` is
/// informational — no action, empty `action_ref`. A trailing `answer_text`
/// surfaces on the voice answer card. Returns how many suggestions surfaced.
pub fn surface_cloud_suggestions(
    app: &tauri::AppHandle,
    state: &crate::app_state::AppState,
    parsed: &aperture_contracts::StructuredSuggestions,
) -> usize {
    use aperture_contracts::suggestions::{BubbleSpec, SuggestionSource};
    let now_ms = epoch_ms();
    let snoozed = state.snooze_until.load(std::sync::atomic::Ordering::SeqCst) > now_ms;
    let mut surfaced = 0usize;

    for s in &parsed.suggestions {
        let conn_state = if s.connector_type == "none" {
            None
        } else {
            match state
                .connectors
                .by_type(&s.connector_type)
                .and_then(|c| c.validate(&s.reconstruct_payload))
            {
                Some(st) => Some(st),
                None => {
                    tracing::warn!(
                        connector_type = %s.connector_type,
                        "cloud suggestion rejected by connector re-validation (doc 09 §4)"
                    );
                    continue;
                }
            }
        };
        let action_ref = conn_state.as_ref().map(|st| st.id.clone()).unwrap_or_default();
        if let Some(st) = &conn_state {
            if let Err(e) = state.db.insert_connector_state(st) {
                tracing::error!(%e, "cloud suggestion connector_state persist failed");
                continue;
            }
        }
        // Rationale as the sublabel, clipped to bubble scale.
        let sublabel = {
            let r = s.rationale.trim();
            if r.is_empty() {
                None
            } else if r.chars().count() > 90 {
                Some(format!("{}…", r.chars().take(89).collect::<String>()))
            } else {
                Some(r.to_string())
            }
        };
        let spec = BubbleSpec {
            title: s.title.clone(),
            glyph: aperture_suggestion_generator::glyph_for(&s.connector_type).to_string(),
            sublabel,
            action_ref: action_ref.clone(),
            source: SuggestionSource::Claude,
            // User-requested content, not a mined guess — full confidence tag.
            confidence: 1.0,
        };
        let insert = state.db.with_conn(|c| {
            c.execute(
                "INSERT INTO suggestions (pattern_id, connector_id, source, title, glyph, confidence, state, shown_ts) \
                 VALUES (NULL, ?1, 'claude', ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    (!action_ref.is_empty()).then_some(action_ref.as_str()),
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
            Ok(id) => {
                surfaced += 1;
                if !snoozed {
                    let _ = crate::events::emit_bubble_spec(app, &id.to_string(), &spec);
                }
            }
            Err(e) => tracing::error!(%e, "cloud suggestion persist failed"),
        }
    }

    if let Some(text) = parsed.answer_text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        let _ = crate::events::emit_voice_surface(
            app,
            &serde_json::json!({
                "surface": "answer",
                "title": text,
                "source": "Claude",
                "action_ref": null,
                "can_ask_claude": false,
            }),
        );
    }
    surfaced
}
