//! The capture toggle: single writer of capture state (doc 02 §7, doc 12 §6).
//!
//! Invariant (3): the resource manager is the **single writer** of the capture
//! toggle (doc 02 §7); Capture, the UI indicator, and the sidecars are readers.
//! Toggle-OFF executes the 6-step release sequence and meets the **3 s SLA**:
//! capture released, sidecars killed, VRAM -> ~0 in < 3 s (doc 12 §6, SC6).
//!
//! ON reverses *lazily*: hooks + sampler come up immediately, but sidecars stay
//! down until the first job demands one (doc 12 §6).

use std::time::Duration;

use tokio::sync::broadcast;

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
    // scheduler: Arc<GpuScheduler>,        // for step 3 (cancel + grace)
    // lifecycle: Arc<Mutex<ModelLifecycle>>, // for step 4 (kill both)
    // telemetry: Arc<Telemetry>,           // step 6 SLA-breach counter
}

impl ToggleOwner {
    /// Construct in the given initial state (first-run consent decides ON/OFF,
    /// doc 13). Capture defaults OFF until consent (doc 13).
    pub fn new(initial: CaptureState) -> Self {
        let (state_tx, _) = broadcast::channel(16);
        Self {
            state: initial,
            state_tx,
        }
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
    /// sampler come up immediately; sidecars stay down until first demanded
    /// (doc 12 §6).
    pub async fn turn_on(&mut self) {
        // TODO(M1:) set state=On; broadcast On; write a capture_toggle{on} audit
        //   event (doc 12 §1 outputs). Do NOT spawn sidecars here (lazy, doc 12 §6).
        todo!("M1: ON reverses lazily — hooks/sampler up now, sidecars on demand (doc 12 §6)")
    }

    /// Turn capture **OFF** — the 6-step release sequence under the 3 s SLA
    /// (doc 12 §6). The watchdog samples VRAM at the end; an SLA breach is logged
    /// and surfaced once.
    pub async fn turn_off(&mut self) {
        // TODO(M1:) execute, racing the whole thing against OFF_SLA (doc 12 §6):
        //   1. flip state -> Off; broadcast capture_off (this struct is the single writer).
        //   2. Capture subsystem runs its release steps (doc 05 §5) — it reacts to the broadcast.
        //   3. scheduler.cancel_all_for_off(): cancel queued; RUNNING_JOB_GRACE (1 s) for the
        //      running job's cancel point, else proceed (a running STT is uncancellable).
        //   4. lifecycle.kill_all_sidecars(): kill BOTH — no graceful drain, the SLA wins
        //      (process death = guaranteed VRAM release, invariant 3).
        //   5. flip the indicator (tray + overlay dot); write capture_toggle{off} audit event
        //      (survives Purge All 30 d, doc 13).
        //   6. watchdog samples nvidia-smi-equivalent; if release took > OFF_SLA ->
        //      telemetry.record_toggle_sla_breach() + surface once (doc 12 §6, doc 04 §9).
        todo!("M1: 6-step toggle-OFF, VRAM->~0 in < 3 s (doc 12 §6, SC6)")
    }

    /// L1<->L2 settings flip mid-session: treated as "unload all -> admit the
    /// next job under the new loadout's rules" (doc 12 §7). Does **not** change
    /// the ON/OFF capture state.
    pub async fn apply_loadout_change(&mut self) {
        // TODO(M5:) lifecycle.kill_all_sidecars(); the next enqueue re-admits
        //   under the new L1/L2 rules (doc 12 §7).
        todo!("M5: loadout flip = unload all, re-admit next under new rules (doc 12 §7)")
    }
}
