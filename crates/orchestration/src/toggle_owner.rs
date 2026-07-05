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
    /// audit row (doc 05 §5); (3) scheduler cancel + (4) sidecar kill are wired
    /// when the GPU stack exists (M5 — no sidecar can be resident before then,
    /// so "sidecars dead" holds vacuously at M1); (5) the indicator is the
    /// shell's reaction to this broadcast; (6) the on-target VRAM watchdog
    /// lands with the M5 telemetry.
    pub async fn turn_off(&mut self) {
        if self.state == CaptureState::Off {
            return; // idempotent
        }
        self.state = CaptureState::Off;
        let _ = self.state_tx.send(CaptureState::Off);
        // TODO(M5): scheduler.cancel_all_for_off() with RUNNING_JOB_GRACE, then
        //   lifecycle.kill_all_sidecars() (process death = guaranteed VRAM
        //   release, invariant 3) + the telemetry SLA watchdog (doc 12 §6).
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
