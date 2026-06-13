//! Aperture Tauri v2 shell — the composition root (doc 02 §2, doc 16 M0).
//!
//! Responsibilities, in startup order:
//! 1. init tracing (local-only; never logs payload contents, doc 13).
//! 2. `aperture-privacy::key_manager::get_or_create_key` — DPAPI-wrapped DB key
//!    (doc 13 §6). Loss of the key => DB unreadable, by design.
//! 3. `aperture-db::Db::open_encrypted` — encrypted history DB + sqlite-vec +
//!    migrations (doc 03, doc 13 §6).
//! 4. `aperture-event-bus::init` — the in-process `tokio::broadcast` of `Event`
//!    (doc 15 §1); SQLite remains the durable form.
//! 5. build the `OrchestratedSystem` — ToggleOwner (single capture writer),
//!    GpuScheduler (single-permit mutex), ModelLifecycle (sidecar spawn/kill),
//!    TierRouter (doc 12 §2). This is the ONLY owner of the GPU + sidecars.
//! 6. spawn the Tier-0 subsystems subscribed to the bus: capture, pattern
//!    engine, voice (doc 02 §3, Critical Path A).
//! 7. `tauri::Builder.manage(AppState).invoke_handler(commands).setup(overlays).run()`.
//!
//! Three invariants this root must preserve:
//! - 8 GB VRAM ceiling / single GPU mutex: only the OrchestratedSystem touches
//!   the GPU; the shell never spawns a sidecar itself (doc 12 §1).
//! - two-emitter transparency gate: the shell opens NO sockets and spawns NO
//!   Claude CLI — only the reasoning gateway does (doc 13 §2).
//! - capture toggle: capture starts OFF and stays off until the user opts in;
//!   OFF releases capture + kills sidecars, VRAM -> ~0 in < 3 s (doc 12 §6).

// Hide the console window on Windows release builds (overlay app, no terminal).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_state;
mod commands;
mod events;
mod overlay;

use app_state::AppState;

fn main() {
    init_tracing();

    // --- M0 composition (see module doc for the ordered call sequence) -------
    //
    // The concrete subsystem constructors below land as their crates are
    // finalized (doc 16 M0). Each line names the intended call; bodies are
    // stubbed so the shell's structure and the IPC/event contracts are pinned
    // first. Replace the `todo!()` with the real wiring crate-by-crate.
    //
    // 2. let wrapped_key = aperture_privacy::key_manager::get_or_create_key()?;   // doc 13 §6
    // 3. let db = aperture_db::Db::open_encrypted(aperture_db::default_db_path(), &wrapped_key)?;
    // 4. let bus_tx = aperture_event_bus::init();                                  // doc 15 §1
    // 5. let system = aperture_orchestration::OrchestratedSystem::build(
    //        bus_tx.clone(), &db, /* settings */ );                               // doc 12 §2
    //        // -> { toggle_owner, scheduler, model_lifecycle, tier_router }
    // 6. aperture_capture::spawn(bus_tx.clone(), system.toggle_owner.subscribe());// Tier 0, default OFF
    //    aperture_pattern_engine::spawn(bus_tx.subscribe(), &db);                 // Critical Path A
    //    aperture_voice::spawn(bus_tx.clone(), system.scheduler.clone());         // Path C
    //
    // 7. let state = AppState::new(bus_tx, db_handle, scheduler, gateway, toggle_owner);
    //    run_tauri(state, system);

    // TODO(M0:) execute steps 2-6 above, then call `run_tauri`. Until the sibling
    //   crates expose their constructors, the shell cannot boot end-to-end.
    todo!("M0: build OrchestratedSystem + Tier-0 subscribers, then run_tauri (doc 16 M0)")
}

/// Local-only structured logging (doc 13). Never logs payload contents or wire
/// bytes — only metadata + audit summaries.
fn init_tracing() {
    // TODO(M0:) tracing_subscriber with an EnvFilter (default e.g. "aperture=info").
    //   Keep it local; no remote sinks (the two-emitter rule, doc 13 §2).
    tracing_subscriber::fmt().with_env_filter("aperture=info").init();
}

/// Build and run the Tauri app: manage [`AppState`], register the IPC command
/// contract, create the per-monitor overlays in `setup`, and run.
///
/// Kept as a separate fn so `main` reads as a linear composition and so the
/// `Builder` wiring is reviewable against the doc 11 / doc 02 contracts.
#[allow(dead_code)]
fn run_tauri(state: AppState) {
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
        .setup(|_app| {
            // TODO(M3:) overlay::create_overlays(app.handle())?; for each window
            //   make_click_through + exclude_from_capture (doc 11 §2).
            // TODO(M3:) spawn the bus->WebView forwarder: subscribe to the
            //   orchestration gpu_busy broadcast and emit events::emit_gpu_busy;
            //   subscribe to suggestion-generator output and emit_bubble_spec.
            // TODO(M0:) emit the initial CaptureIndicator::Off (capture starts OFF).
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Aperture overlay shell");
}
