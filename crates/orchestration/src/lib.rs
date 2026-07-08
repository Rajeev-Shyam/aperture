//! Orchestration & resource manager — "the brain" (doc 12).
//!
//! Owns the capture toggle (single writer, doc 02 §7), the single-permit GPU
//! mutex + priority queue, sidecar lifecycles, the R1 VRAM projection check, and
//! tier routing. It is the only crate that touches the GPU or a sidecar; **no
//! component but the gateway touches the network** (doc 12 §1, doc 02 §8).
//!
//! The three invariants this crate enforces:
//! 1. **8 GB VRAM ceiling / single GPU mutex** — [`gpu_scheduler`] holds one
//!    `Semaphore` permit; [`budget_enforcer`] admits only `<= 7.0 GB` projected,
//!    **counting co-resident weights** (doc 04 §4, R1, ADR-030).
//! 2. **Two-emitter transparency gate** — this crate opens **no** network
//!    sockets; the *only* `std::process::Command` it runs is the local sidecar
//!    spawn in [`model_lifecycle`] (doc 13 §2). Explicit reasoning is *routed*
//!    to the gateway, never executed here (doc 12 §2).
//! 3. **Capture toggle** — OFF releases capture + kills both sidecars,
//!    VRAM -> ~0 in < 3 s (doc 12 §6, SC6), driven by [`toggle_owner`].

pub mod budget_enforcer;
pub mod gpu_scheduler;
pub mod model_lifecycle;
pub mod telemetry;
pub mod tier_router;
pub mod toggle_owner;
pub mod vram_table;
pub mod warm_keep;

use std::sync::Arc;

use aperture_contracts::{GpuJob, GpuScheduler as GpuSchedulerContract, JobError, JobOutput};
use tokio::sync::{broadcast, Mutex as TokioMutex};

use crate::budget_enforcer::BudgetEnforcer;
use crate::gpu_scheduler::{GpuScheduler, JobRunner, SidecarRunner};
use crate::model_lifecycle::ModelLifecycle;
use crate::telemetry::Telemetry;
use crate::toggle_owner::ToggleOwner;
use crate::vram_table::VramTable;

/// Which sanctioned loadout is active (doc 04 §3). No third loadout exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loadout {
    /// L1 default: Qwen2.5-VL **3B** + **faster-whisper** *conditionally*
    /// co-resident (ADR-030: co-resident when memory allows; STT is the swap
    /// victim under image-VLM pressure); the mutex still serializes execution
    /// (doc 04 §3).
    L1,
    /// L2 opt-in: Qwen2.5-VL **7B** *exclusive*; STT forces an unload->load swap
    /// (doc 04 §3).
    L2,
}

impl Default for Loadout {
    fn default() -> Self {
        Loadout::L1 // commit to L1; 7B stays a feature flag (doc 16 staged rec. 2)
    }
}

/// The facade that wires the six subcomponents (doc 12 §2) together and is what
/// the Tauri shell holds. Implements [`aperture_contracts::GpuScheduler`] so
/// callers (Docs 06/07) only ever see the contract-4 surface.
pub struct OrchestratedSystem {
    scheduler: Arc<GpuScheduler>,
    toggle: ToggleOwner,
    lifecycle: Arc<TokioMutex<ModelLifecycle>>,
    telemetry: Arc<Telemetry>,
    router: tier_router::TierRouter,
    loadout: Loadout,
}

impl OrchestratedSystem {
    /// Build the system under `loadout`, in the given initial capture state, with
    /// the production sidecar runner ([`SidecarRunner`] + the default
    /// [`ModelLifecycle`], which spawns the real `vlm-host`). Sidecars are **not**
    /// spawned here — ON is lazy (doc 12 §6).
    pub fn new(loadout: Loadout, initial: toggle_owner::CaptureState) -> Self {
        Self::with_runner(loadout, initial, Arc::new(SidecarRunner::new()), None)
    }

    /// Build with an injected runner (+ optional lifecycle) — the seam the M5
    /// gate + tests use to exercise the mutex/priority/preempt/deadline machinery
    /// without a GPU (doc 15 §7). Production calls [`Self::new`].
    pub fn with_runner(
        loadout: Loadout,
        initial: toggle_owner::CaptureState,
        runner: Arc<dyn JobRunner>,
        lifecycle: Option<Arc<TokioMutex<ModelLifecycle>>>,
    ) -> Self {
        let telemetry = Arc::new(Telemetry::new());
        let lifecycle =
            lifecycle.unwrap_or_else(|| Arc::new(TokioMutex::new(ModelLifecycle::default())));
        let enforcer = BudgetEnforcer::new(VramTable::seeded());
        let scheduler = Arc::new(GpuScheduler::new(
            enforcer,
            Arc::clone(&lifecycle),
            runner,
            Arc::clone(&telemetry),
            loadout,
        ));
        let mut toggle = ToggleOwner::new(initial);
        toggle.wire_resources(
            Arc::clone(&scheduler),
            Arc::clone(&lifecycle),
            Arc::clone(&telemetry),
        );
        Self {
            scheduler,
            toggle,
            lifecycle,
            telemetry,
            router: tier_router::TierRouter::new(),
            loadout,
        }
    }

    /// The active loadout (doc 04 §3).
    pub fn loadout(&self) -> Loadout {
        self.loadout
    }

    /// A telemetry snapshot for the M-gate harnesses (doc 16 M5/M6). `now_ms`
    /// bounds the trailing-hour wake window.
    pub fn telemetry_snapshot(&self, now_ms: i64) -> telemetry::TelemetrySnapshot {
        self.telemetry.snapshot(now_ms)
    }

    /// The VLM wake gate (doc 06 §4). The shell calls this after OCR to decide
    /// whether to enqueue a `prio:50` enrichment job (VLM never gates a bubble,
    /// doc 02 Path A). Mutable: it owns the per-app debounce + wake-rate ledger.
    pub fn wake_gate(&mut self) -> &mut tier_router::TierRouter {
        &mut self.router
    }

    /// Record a VLM wake in telemetry (the M5 3–10/h band assertion, ADR-032).
    pub fn record_vlm_wake(&self, now_ms: i64) {
        self.telemetry.record_vlm_wake(now_ms);
    }

    /// Whether the GPU mutex is *likely* free right now — advisory input to the
    /// wake gate (doc 06 §4). The scheduler's admission is the real word.
    pub fn mutex_likely_free(&self) -> bool {
        self.scheduler.available_permits() > 0
    }

    /// The single GPU scheduler handle (contract 4), for callers that enqueue
    /// VLM jobs off the bubble path (doc 06 §3, e.g. `aperture-vision-ocr`).
    pub fn scheduler(&self) -> Arc<GpuScheduler> {
        Arc::clone(&self.scheduler)
    }

    /// The sidecar lifecycle handle (for the idle-unload sweep timer the shell
    /// drives, doc 04 §5).
    pub fn lifecycle(&self) -> Arc<TokioMutex<ModelLifecycle>> {
        Arc::clone(&self.lifecycle)
    }

    /// The `gpu_busy` observable (doc 11 §6, doc 14 §5): `true` while the single
    /// execution permit is held. Deliberately **not** on the `GpuScheduler`
    /// contract trait so `contracts` stays free of an async-runtime dep
    /// (doc 15 §4) — exposed here directly as a broadcast receiver.
    pub fn gpu_busy(&self) -> broadcast::Receiver<bool> {
        self.scheduler.subscribe_busy()
    }

    /// The capture toggle owner (single writer, doc 02 §7). The tray/UI drives
    /// `turn_on`/`turn_off` through this; readers subscribe via
    /// [`toggle_owner::ToggleOwner::subscribe`].
    pub fn toggle(&mut self) -> &mut ToggleOwner {
        &mut self.toggle
    }

    /// Subscribe to capture-state changes (Capture subsystem, UI indicator,
    /// sidecars — doc 02 §7).
    pub fn subscribe_capture(&self) -> broadcast::Receiver<toggle_owner::CaptureState> {
        self.toggle.subscribe()
    }
}

/// Contract 4 (doc 15 §4): the single entry point to GPU execution. The
/// orchestration crate is the only implementor; callers never touch the GPU,
/// a sidecar, or VRAM accounting (doc 12 §1).
#[async_trait::async_trait]
impl GpuSchedulerContract for OrchestratedSystem {
    async fn enqueue(&self, job: GpuJob) -> Result<JobOutput, JobError> {
        // Delegates to the priority-queue + mutex scheduler (doc 12 §3).
        self.scheduler.enqueue(job).await
    }
}
