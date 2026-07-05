//! Shared application state, managed by Tauri and injected into every command
//! (doc 02 §7 single-writer ownership; doc 11 §1 inputs).
//!
//! `AppState` is the shell's handle bag: the event bus, the encrypted DB
//! handle, the capture subsystem, and the orchestration system (which owns the
//! capture-toggle single writer + the GPU scheduler). It holds *handles*,
//! never logic — the shell composes the subsystems, it does not reimplement
//! them.
//!
//! Invariant reminders carried in the field docs:
//! - the GPU scheduler is the ONLY path to VRAM (doc 12 §1, the 8 GB ceiling);
//! - the gateway is the ONLY network/Claude-CLI emitter (doc 13 §2) — it is
//!   wired at M7 and deliberately absent here until then;
//! - the toggle owner (inside `orchestration`) is the SINGLE writer of capture
//!   state (doc 02 §7).

use std::sync::Arc;

use aperture_capture::CaptureSubsystem;
use aperture_db::Db;
use aperture_event_bus::EventBus;
use aperture_orchestration::OrchestratedSystem;

/// Injected into every `#[tauri::command]` via `tauri::State<AppState>`.
///
/// Clonable: every field is an `Arc` so commands share one set of handles.
/// Constructed once in `main.rs` and `Builder::manage`d.
#[derive(Clone)]
pub struct AppState {
    /// The in-process event bus (doc 15 §1). SQLite is the durable form; the
    /// bus is at-most-once notify.
    pub bus: EventBus,

    /// Encrypted DB handle (doc 03). Read by `list_suggestions`, connector-state
    /// lookups, settings; written by the Tier-0 pipeline (single writer).
    pub db: Arc<Db>,

    /// The capture subsystem mechanism (doc 05). Driven by the orchestration
    /// toggle broadcast — commands never call start/stop directly. Read by the
    /// M2-tuning/diagnostics surfaces (frame counters); held from M0 so the
    /// composition is complete.
    #[allow(dead_code)]
    pub capture: Arc<CaptureSubsystem>,

    /// Orchestration: the toggle single-writer + GPU scheduler (doc 12).
    /// `toggle_capture` routes here so capture state has exactly one writer
    /// (doc 02 §7); the tokio Mutex covers the `&mut` turn_on/turn_off surface.
    pub orchestration: Arc<tokio::sync::Mutex<OrchestratedSystem>>,
    // gateway: wired at M7 (doc 09) — the ONLY field that may reach the network.
}

impl AppState {
    /// Assemble the handle bag at startup (doc 16 M0). Called from `main.rs`
    /// after the bus, DB, orchestration, and capture subsystem exist.
    pub fn new(
        bus: EventBus,
        db: Arc<Db>,
        capture: Arc<CaptureSubsystem>,
        orchestration: Arc<tokio::sync::Mutex<OrchestratedSystem>>,
    ) -> Self {
        Self { bus, db, capture, orchestration }
    }
}
