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

use std::sync::Arc;

use aperture_contracts::{GpuJob, GpuScheduler as GpuSchedulerContract, JobError, JobOutput};
use tokio::sync::broadcast;

use crate::gpu_scheduler::GpuScheduler;
use crate::toggle_owner::ToggleOwner;

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
    // lifecycle: Arc<tokio::sync::Mutex<model_lifecycle::ModelLifecycle>>,
    // enforcer: budget_enforcer::BudgetEnforcer,
    // router: tier_router::TierRouter,
    // telemetry: Arc<telemetry::Telemetry>,
    // loadout: Loadout,
}

impl OrchestratedSystem {
    /// Build the system under `loadout`, in the given initial capture state.
    /// Sidecars are **not** spawned here — ON is lazy (doc 12 §6).
    pub fn new(_loadout: Loadout, initial: toggle_owner::CaptureState) -> Self {
        // TODO(M5:) seed BudgetEnforcer with VramTable::seeded(), build the
        //   ModelLifecycle + TierRouter + Telemetry, and hand the scheduler the
        //   lifecycle/enforcer/telemetry handles it needs to run jobs (doc 12 §2).
        Self {
            scheduler: Arc::new(GpuScheduler::new()),
            toggle: ToggleOwner::new(initial),
        }
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
