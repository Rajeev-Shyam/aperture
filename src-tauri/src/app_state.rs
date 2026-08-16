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

use aperture_capture::exclusion::ExclusionList;
use aperture_capture::CaptureSubsystem;
use aperture_db::Db;
use aperture_event_bus::EventBus;
use aperture_orchestration::OrchestratedSystem;
use aperture_reasoning_gateway::preview::PreviewSession;
use aperture_reasoning_gateway::Gateway;

/// In-flight preview sessions (doc 13 §3), keyed by `payload_id`.
///
/// `request_preview` inserts; `preview_set_approved` marks; `preview_send`
/// consumes; `preview_cancel` drops (zero residue). Sessions the UI abandoned
/// without telling us are pruned by age on the next `request_preview`.
#[derive(Default)]
pub struct PreviewStore {
    pub sessions: std::collections::HashMap<uuid::Uuid, PreviewSession>,
    /// payload_id → SHA-256 of the approved payload's canonical serialization.
    /// Approval is bound to CONTENT, not just the id: `preview_send` refuses a
    /// session whose bytes no longer hash to what was approved (preview == wire,
    /// doc 13 §3).
    pub approved: std::collections::HashMap<uuid::Uuid, String>,
}

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

    /// Bubble feedback into the pattern task's engine (doc 08 §7, Q81):
    /// `(pattern_id, signal)` pairs sent by `record_feedback`.
    pub feedback_tx:
        tokio::sync::mpsc::UnboundedSender<(i64, aperture_pattern_engine::FeedbackEvent)>,

    /// Global bubble snooze deadline, epoch ms (ADR-040/Q95): `0` = off,
    /// `i64::MAX` = until re-enabled. Written by `set_snooze`; read by the
    /// pattern task's emit gate and `list_suggestions`.
    pub snooze_until: Arc<std::sync::atomic::AtomicI64>,

    /// The connector registry (doc 10 §1, M4): read by `bubble_click`
    /// (Path B resolve → reconstruct → open) and the connector-capture task.
    /// Only connectors act (ADR-035) — commands never dispatch directly.
    pub connectors: Arc<aperture_connectors::ConnectorRegistry>,

    /// Consent state (doc 13 §8, M9): the source of truth for whether capture
    /// may run, and the writer of the `capture_toggle` audit trail. A tokio
    /// Mutex because every mutation persists to the encrypted DB.
    pub consent: Arc<tokio::sync::Mutex<aperture_privacy::consent::ConsentManager>>,

    /// The reasoning gateway (doc 09) — the ONLY field that may reach the
    /// network, and only via `preview_send` with an approved payload (doc 13 §2).
    /// Carries the DB-backed `AuditLog` via `Gateway::with_audit`.
    pub gateway: Arc<Gateway>,

    /// In-flight preview sessions (doc 13 §3): built by `request_preview`,
    /// approved by `preview_set_approved`, consumed by `preview_send`.
    pub previews: Arc<tokio::sync::Mutex<PreviewStore>>,

    /// The intended transport line the preview shows: the first *push* transport
    /// in the settings order (MCP is pull-only and serves the handoff path).
    pub push_target: aperture_contracts::TransportTarget,

    /// Voice subsystem handle (doc 07, M6): commands + the last completed
    /// transcript (seeds the `answer_query` preview's `user_addition`).
    pub voice: crate::voice::VoiceHandle,

    /// The live exclusion matcher handle (doc 13 §4) — the SAME shared handle
    /// the capture sampler/normalizer hold, so `add_exclusion`/`set_exclusion`
    /// hot-swap the compiled rules without a restart.
    pub exclusions: ExclusionList,

    /// VLM weight install state (decision #30): the resolved weight paths the
    /// spawner uses (= where a download lands) + the in-flight guard. Read by
    /// `vlm_status`; `vlm_download` runs the user-initiated fetch against it.
    pub vlm_fetch: Arc<crate::vlm_fetch::VlmFetchState>,
}

impl AppState {
    /// Assemble the handle bag at startup (doc 16 M0). Called from `main.rs`
    /// after the bus, DB, orchestration, capture, gateway, and voice exist.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bus: EventBus,
        db: Arc<Db>,
        capture: Arc<CaptureSubsystem>,
        orchestration: Arc<tokio::sync::Mutex<OrchestratedSystem>>,
        feedback_tx: tokio::sync::mpsc::UnboundedSender<(
            i64,
            aperture_pattern_engine::FeedbackEvent,
        )>,
        snooze_until: Arc<std::sync::atomic::AtomicI64>,
        connectors: Arc<aperture_connectors::ConnectorRegistry>,
        consent: Arc<tokio::sync::Mutex<aperture_privacy::consent::ConsentManager>>,
        gateway: Arc<Gateway>,
        push_target: aperture_contracts::TransportTarget,
        voice: crate::voice::VoiceHandle,
        exclusions: ExclusionList,
        vlm_fetch: Arc<crate::vlm_fetch::VlmFetchState>,
    ) -> Self {
        Self {
            bus,
            db,
            capture,
            orchestration,
            feedback_tx,
            snooze_until,
            connectors,
            consent,
            gateway,
            previews: Arc::new(tokio::sync::Mutex::new(PreviewStore::default())),
            push_target,
            voice,
            exclusions,
            vlm_fetch,
        }
    }
}
