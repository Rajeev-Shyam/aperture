//! Aperture Tauri v2 shell — the composition root (doc 02 §2, doc 16 M0).
//!
//! Responsibilities, in startup order:
//! 1. init tracing (local-only; never logs payload contents, doc 13).
//! 2. open the history DB (doc 03). At-rest encryption keys on
//!    `aperture-privacy::key_manager` at **M9** — until then the DB opens with
//!    the M9-shaped call and an empty key, exactly as `Db::open_encrypted`
//!    documents (the file sits under the user profile with default ACLs).
//! 3. `EventBus` — the in-process notify channel (doc 15 §1); SQLite stays the
//!    durable form (persist-then-notify, wired via the capture EventStore seam).
//! 4. `OrchestratedSystem` — ToggleOwner (single capture writer), GpuScheduler
//!    (single-permit mutex; jobs land at M5) (doc 12 §2).
//! 5. the Tier-0 pipeline: capture subsystem + OCR/store frame sink + the
//!    pattern-engine consumer (doc 02 §4, Critical Path A).
//! 6. `tauri::Builder.manage(AppState).invoke_handler(commands).setup(...).run()`.
//!
//! Three invariants this root preserves:
//! - 8 GB VRAM ceiling / single GPU mutex: only the OrchestratedSystem touches
//!   the GPU; the shell never spawns a sidecar itself (doc 12 §1).
//! - two-emitter transparency gate: the shell opens NO sockets and spawns NO
//!   Claude CLI — only the reasoning gateway does (doc 13 §2, wired M7).
//! - capture toggle: capture starts OFF and stays off until the user opts in
//!   (doc 13 §8); OFF releases capture, VRAM -> ~0 in < 3 s (doc 12 §6).

// Hide the console window on Windows release builds (overlay app, no terminal).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_state;
mod commands;
mod events;
mod overlay;
mod pipeline;

use std::sync::Arc;

use app_state::AppState;

fn main() {
    init_tracing();

    // Our subsystems spawn tokio tasks (capture drain/heartbeat, pattern task):
    // give them a runtime that outlives `main`'s scope and enter it so plain
    // `tokio::spawn` works during composition. Tauri's own loop runs on the
    // main thread alongside.
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let _rt_guard = rt.enter();

    // 2. history DB (doc 03). M9 wires the DPAPI-wrapped key (doc 13 §6).
    let db = Arc::new(
        aperture_db::Db::open_encrypted(aperture_db::default_db_path(), &[])
            .expect("open history DB"),
    );

    // Retention: enforce TTLs on startup + daily (doc 03 §6, doc 16 M2).
    spawn_retention(Arc::clone(&db));

    // 3. the bus (doc 15 §1).
    let bus = aperture_event_bus::EventBus::new();

    // 4. orchestration — capture starts OFF until consent (doc 13 §8).
    let orchestration = Arc::new(tokio::sync::Mutex::new(
        aperture_orchestration::OrchestratedSystem::new(
            aperture_orchestration::Loadout::L1,
            aperture_orchestration::toggle_owner::CaptureState::Off,
        ),
    ));

    // 5. Tier-0 pipeline: store seam + OCR sink + capture subsystem.
    let store: Arc<dyn aperture_capture::normalizer::EventStore> =
        Arc::new(pipeline::DbEventStore { db: Arc::clone(&db) });
    let sink = build_frame_sink(Arc::clone(&db));
    let capture = aperture_capture::CaptureSubsystem::new(
        aperture_capture::CaptureConfig::default(),
        bus.clone(),
        // ADR-029/Q15: defaults ship EMPTY; user rules merge from settings at M9.
        aperture_capture::exclusion::ExclusionList::shipped_defaults(),
        sink,
        Some(store),
    );

    // Drive capture off the toggle broadcast (single writer, doc 02 §7).
    let capture_rx = rt.block_on(async { orchestration.lock().await.subscribe_capture() });
    spawn_capture_driver(Arc::clone(&capture), capture_rx);

    let state = AppState::new(bus, db, capture, orchestration);
    run_tauri(state, &rt);
}

/// Local-only structured logging (doc 13). Never logs payload contents or wire
/// bytes — only metadata + audit summaries.
fn init_tracing() {
    tracing_subscriber::fmt().with_env_filter("aperture=info").init();
}

/// The M2 frame sink: OCR (Windows.Media.Ocr, en fallback) + embedding
/// (HashEmbedder by default; the real nomic backend is the opt-in `nomic`
/// feature — see `aperture-embedding`'s module doc on the download cost).
/// If no OCR engine is constructible (missing language packs), degrade to
/// event-only capture (doc 06 §6) with a one-time notice.
fn build_frame_sink(db: Arc<aperture_db::Db>) -> Arc<dyn aperture_capture::sampler::FrameSink> {
    #[cfg(feature = "nomic")]
    let embedder: Arc<dyn aperture_embedding::Embedder> = {
        let models_dir = std::path::PathBuf::from("models");
        match aperture_embedding::NomicEmbedder::load(models_dir) {
            Ok(e) => Arc::new(e),
            Err(e) => {
                tracing::error!(%e, "nomic backend failed; falling back to HashEmbedder");
                Arc::new(aperture_embedding::HashEmbedder)
            }
        }
    };
    #[cfg(not(feature = "nomic"))]
    let embedder: Arc<dyn aperture_embedding::Embedder> =
        Arc::new(aperture_embedding::HashEmbedder);
    tracing::info!(backend = embedder.id(), "embedding backend");

    match aperture_vision_ocr::windows_media_ocr::WindowsMediaOcr::new("en-US") {
        Ok(engine) => Arc::new(pipeline::OcrStoreSink {
            db,
            processor: aperture_vision_ocr::FrameProcessor::new(Box::new(engine), embedder),
        }),
        Err(e) => {
            tracing::error!(%e, "OCR engine unavailable — event-only capture (doc 06 §6)");
            Arc::new(aperture_capture::sampler::DropSink)
        }
    }
}

/// React to the orchestration toggle broadcast: ON → capture STARTING path,
/// OFF → the ≤3 s release (doc 05 §5, doc 12 §6).
fn spawn_capture_driver(
    capture: Arc<aperture_capture::CaptureSubsystem>,
    mut rx: tokio::sync::broadcast::Receiver<aperture_orchestration::toggle_owner::CaptureState>,
) {
    tokio::spawn(async move {
        use aperture_orchestration::toggle_owner::CaptureState;
        loop {
            match rx.recv().await {
                Ok(CaptureState::On) => {
                    if let Err(e) = capture.start().await {
                        tracing::error!(%e, "capture start failed (event-only until retry)");
                    }
                }
                Ok(CaptureState::Off) => {
                    if let Err(e) = capture.stop().await {
                        // ToggleSlaBreach: force path already ran; surface once.
                        tracing::error!(%e, "capture stop breached the SLA (doc 05 §7)");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
}

/// Retention on startup + a daily timer (doc 03 §6, doc 16 M2).
fn spawn_retention(db: Arc<aperture_db::Db>) {
    tokio::spawn(async move {
        let policy = aperture_db::retention::RetentionPolicy::default();
        loop {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            match aperture_db::retention::run_nightly_prune(&db, now, &policy) {
                Ok(report) => tracing::info!(?report, "retention prune"),
                Err(e) => tracing::error!(%e, "retention prune failed"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(24 * 3600)).await;
        }
    });
}

/// Build and run the Tauri app: manage [`AppState`], register the IPC command
/// contract, create the overlay + spawn the WebView forwarders in `setup`, run.
fn run_tauri(state: AppState, _rt: &tokio::runtime::Runtime) {
    let setup_state = state.clone();
    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::toggle_capture,
            commands::list_suggestions,
            commands::bubble_click,
            commands::request_preview,
            commands::preview_set_approved,
            commands::preview_send,
            commands::voice_ptt_down,
            commands::voice_ptt_up,
            commands::get_settings,
            commands::set_settings,
        ])
        .setup(move |app| {
            use tauri::Manager;
            // Overlay hardening (doc 11 §2): click-through + capture-exclusion.
            if let Some(window) = app.get_webview_window(overlay::OVERLAY_LABEL) {
                if let Err(e) = overlay::make_click_through(&window) {
                    tracing::error!(%e, "overlay click-through failed (doc 11 §2)");
                }
                if let Err(e) = overlay::exclude_from_capture(&window) {
                    tracing::error!(%e, "overlay capture-exclusion failed (doc 05 §2)");
                }
            }
            // Capture starts OFF (doc 13 §8) — the indicator must say so.
            let _ = events::emit_capture_indicator(
                app.handle(),
                events::CaptureIndicator::Off,
            );
            // The pattern-engine consumer (doc 02 §4 steps 6-8) needs the app
            // handle to emit bubble_spec events; spawned here.
            let capture_rx = tauri::async_runtime::block_on(async {
                setup_state.orchestration.lock().await.subscribe_capture()
            });
            pipeline::spawn_pattern_task(
                &setup_state.bus,
                std::sync::Arc::clone(&setup_state.db),
                capture_rx,
                app.handle().clone(),
            );
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Aperture overlay shell");
}
