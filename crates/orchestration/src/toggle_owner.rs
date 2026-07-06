//! The capture toggle: single writer of capture state (doc 02 §7, doc 12 §6).
//!
//! Invariant (3): the resource manager is the **single writer** of the capture
//! toggle (doc 02 §7); Capture, the UI indicator, and the sidecars are readers.
//! Toggle-OFF executes the 6-step release sequence and meets the **3 s SLA**:
//! capture released, sidecars killed, VRAM -> ~0 in < 3 s (doc 12 §6, SC6).
//!
//! ON reverses *lazily*: hooks + sampler come up immediately, but sidecars stay
//! down until the first job demands one (doc 12 §6).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, Mutex as TokioMutex};

use crate::gpu_scheduler::GpuScheduler;
use crate::model_lifecycle::ModelLifecycle;
use crate::telemetry::Telemetry;

/// The 3 s end-to-end release SLA for toggle-OFF (doc 12 §6, SC6 / doc 16 M1).
pub const OFF_SLA: Duration = Duration::from_secs(3);
/// The running-job grace before the sidecar kill proceeds (step 3, doc 12 §6).
pub const RUNNING_JOB_GRACE: Duration = Duration::from_secs(1);

/// Capture state — the single value this owner writes (doc 02 §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureState {
    /// Hooks + WGC sampler running; sidecars demand-loaded (doc 12 §6).
    On,
    /// Everything released; VRAM ~0 (doc 12 §6).
    Off,
}

/// The single writer of capture state (doc 02 §7). Holds the broadcast channel
/// every reader (Capture subsystem, UI indicator, sidecars) subscribes to, and
/// drives the §6 OFF sequence. Owned by [`crate::OrchestratedSystem`].
pub struct ToggleOwner {
    /// The current state — only this struct mutates it (single-writer rule).
    state: CaptureState,
    /// Readers subscribe here (doc 02 §7: Capture, UI indicator, sidecars).
    state_tx: broadcast::Sender<CaptureState>,
    /// Step 3 (cancel queued + running-job grace) and step 4 (kill both sidecars)
    /// of the OFF sequence (doc 12 §6). `None` in bare test harnesses; wired by
    /// [`crate::OrchestratedSystem::new`].
    scheduler: Option<Arc<GpuScheduler>>,
    lifecycle: Option<Arc<TokioMutex<ModelLifecycle>>>,
    /// Step 6 SLA-breach counter (doc 12 §6).
    telemetry: Option<Arc<Telemetry>>,
}

impl ToggleOwner {
    /// Construct in the given initial state (first-run consent decides ON/OFF,
    /// doc 13). Capture defaults OFF until consent (doc 13). GPU resources are
    /// wired separately via [`Self::wire_resources`] (the scheduler + lifecycle
    /// are built alongside this owner in [`crate::OrchestratedSystem::new`]).
    pub fn new(initial: CaptureState) -> Self {
        let (state_tx, _) = broadcast::channel(16);
        Self {
            state: initial,
            state_tx,
            scheduler: None,
            lifecycle: None,
            telemetry: None,
        }
    }

    /// Wire the GPU resources the OFF sequence must release (doc 12 §6 steps 3-4,
    /// 6). Called once by [`crate::OrchestratedSystem::new`] after the scheduler +
    /// lifecycle exist.
    pub fn wire_resources(
        &mut self,
        scheduler: Arc<GpuScheduler>,
        lifecycle: Arc<TokioMutex<ModelLifecycle>>,
        telemetry: Arc<Telemetry>,
    ) {
        self.scheduler = Some(scheduler);
        self.lifecycle = Some(lifecycle);
        self.telemetry = Some(telemetry);
    }

    /// The current capture state (readers may poll; most subscribe instead).
    pub fn state(&self) -> CaptureState {
        self.state
    }

    /// Subscribe to capture-state changes (doc 02 §7 readers).
    pub fn subscribe(&self) -> broadcast::Receiver<CaptureState> {
        self.state_tx.subscribe()
    }

    /// Turn capture **ON** lazily: flip state + broadcast so hooks and the WGC
    /// sampler come up immediately (the capture subsystem reacts to the
    /// broadcast and runs its STARTING path, incl. the `capture_toggle{on}`
    /// audit row — doc 05 §5); sidecars stay down until first demanded
    /// (doc 12 §6). This struct is the single writer of the state.
    pub async fn turn_on(&mut self) {
        if self.state == CaptureState::On {
            return; // idempotent
        }
        self.state = CaptureState::On;
        let _ = self.state_tx.send(CaptureState::On); // readers react (doc 02 §7)
    }

    /// Turn capture **OFF** — the doc 12 §6 release sequence under the 3 s SLA.
    ///
    /// Step map: (1) flip + broadcast here (single writer); (2) the capture
    /// subsystem reacts with its release steps incl. the `capture_toggle{off}`
    /// audit row (doc 05 §5); (3) scheduler cancels queued jobs + gives a running
    /// job `RUNNING_JOB_GRACE` to exit; (4) the lifecycle kills **both** sidecars
    /// — process death is the guaranteed VRAM-release primitive (invariant 3,
    /// doc 02 §2); (5) the indicator is the shell's reaction to this broadcast;
    /// (6) an OFF that overran the SLA increments the breach counter (surfaced
    /// once). Steps 3-4 run in parallel with the capture-side release.
    pub async fn turn_off(&mut self) {
        if self.state == CaptureState::Off {
            return; // idempotent
        }
        let started = tokio::time::Instant::now();
        self.state = CaptureState::Off;
        let _ = self.state_tx.send(CaptureState::Off);

        // (3) cancel queued + running-job grace; (4) kill both sidecars.
        if let Some(scheduler) = &self.scheduler {
            scheduler.cancel_all_for_off(RUNNING_JOB_GRACE).await;
        }
        if let Some(lifecycle) = &self.lifecycle {
            if let Err(e) = lifecycle.lock().await.kill_all_sidecars().await {
                tracing::error!(%e, "sidecar kill failed on OFF (doc 12 §6 step 4)");
            }
        }

        // (6) SLA watchdog: an OFF that overran 3 s is a breach (surfaced once).
        if started.elapsed() > OFF_SLA {
            if let Some(telemetry) = &self.telemetry {
                telemetry.record_toggle_sla_breach();
            }
            tracing::error!("toggle OFF exceeded {OFF_SLA:?} SLA (doc 12 §6)");
        }
    }

    /// L1<->L2 settings flip mid-session: "unload all -> admit the next job under
    /// the new loadout's rules" (doc 12 §7). Does **not** change the ON/OFF state.
    pub async fn apply_loadout_change(&mut self, loadout: crate::Loadout) {
        if let Some(scheduler) = &self.scheduler {
            scheduler.set_loadout(loadout);
        }
        if let Some(lifecycle) = &self.lifecycle {
            // Unload all; the next enqueue re-admits under the new L1/L2 rules.
            let _ = lifecycle.lock().await.kill_all_sidecars().await;
        }
    }
}
