//! Shared application state, managed by Tauri and injected into every command
//! (doc 02 §7 single-writer ownership; doc 11 §1 inputs).
//!
//! `AppState` is the shell's handle bag: the event-bus sender, the encrypted DB
//! handle, the GPU scheduler, the reasoning gateway, and the capture-toggle
//! owner. It holds *handles*, never logic — the shell composes the subsystems,
//! it does not reimplement them.
//!
//! Invariant reminders carried in the field docs:
//! - the GPU scheduler is the ONLY path to VRAM (doc 12 §1, the 8 GB ceiling);
//! - the gateway is the ONLY network/Claude-CLI emitter (doc 13 §2);
//! - the toggle owner is the SINGLE writer of capture state (doc 02 §7).

use std::sync::Arc;

use aperture_contracts::Event;
use tokio::sync::broadcast;

// NOTE: the concrete subsystem types below are owned by sibling crates that are
// scaffolded in parallel. The shell references them through these aliases so the
// public surface here is stable while those crates land (doc 16 M0). Swap each
// alias for the real type as the crate's public API is finalized.
//
// TODO(M0:) replace these placeholders with the real re-exports, e.g.
//   use aperture_db::Db;
//   use aperture_orchestration::{GpuSchedulerHandle, ToggleOwnerHandle};
//   use aperture_reasoning_gateway::ReasoningGateway;

/// Encrypted history DB handle (doc 03, doc 13 §6). `aperture_db::Db`.
pub type DbHandle = Arc<()>;
/// The single GPU scheduler (doc 12 §2). Implements `aperture_contracts::GpuScheduler`.
pub type SchedulerHandle = Arc<()>;
/// The reasoning gateway (doc 09) — the sole network / Claude-CLI emitter (doc 13 §2).
pub type GatewayHandle = Arc<()>;
/// The capture-toggle owner (doc 12 §2) — the single writer of capture state (doc 02 §7).
pub type ToggleOwnerHandle = Arc<()>;

/// Injected into every `#[tauri::command]` via `tauri::State<AppState>`.
///
/// Clonable: every field is an `Arc`/`Sender` so commands share one set of
/// handles. Constructed once in `main.rs` setup and `Builder::manage`d.
#[derive(Clone)]
pub struct AppState {
    /// Publish-side of the in-process event bus (doc 15 §1). Commands that
    /// produce events (e.g. `suggestion_*`, `voice_ptt_*`) send here; SQLite is
    /// the durable form, the bus is at-most-once.
    pub bus_tx: broadcast::Sender<Event>,

    /// Encrypted DB handle (doc 03). Read by `list_suggestions`, the payload
    /// builder behind `request_preview`, and connector-state lookups for
    /// `bubble_click`. Only the Tier-0 pipeline writes.
    pub db: DbHandle,

    /// GPU scheduler (doc 12 §2). Enqueue-only; callers never touch VRAM. The
    /// preview's "add screen summary" enrichment routes a VLM job through this.
    pub scheduler: SchedulerHandle,

    /// Reasoning gateway (doc 09). The ONLY field that may reach the network;
    /// only `preview_send` touches it, and only with an approved payload (doc 13 §2).
    pub gateway: GatewayHandle,

    /// Capture-toggle owner (doc 12 §2). `toggle_capture` routes here so capture
    /// state has exactly one writer (doc 02 §7); OFF runs the <3 s release SLA.
    pub toggle_owner: ToggleOwnerHandle,
}

impl AppState {
    /// Assemble the handle bag at startup (doc 16 M0). Called from `main.rs`
    /// setup after the bus, DB, scheduler, gateway, and toggle owner exist.
    pub fn new(
        bus_tx: broadcast::Sender<Event>,
        db: DbHandle,
        scheduler: SchedulerHandle,
        gateway: GatewayHandle,
        toggle_owner: ToggleOwnerHandle,
    ) -> Self {
        Self { bus_tx, db, scheduler, gateway, toggle_owner }
    }
}
