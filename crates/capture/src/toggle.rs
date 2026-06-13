//! The capture enable/disable toggle (G8 / SC6) — state machine (doc 05 §5).
//!
//! Invariant (3): when capture is OFF the system guarantees **no events written,
//! no frames taken, sidecars dead, VRAM released** — verified at the M1 gate with
//! `nvidia-smi` (doc 05 §5). The OFF release must complete within a **3 s SLA**;
//! on breach we hard-kill the sidecars and force-release WGC (doc 05 §7).
//!
//! ```text
//!             user toggles OFF
//!    ON ───────────────────────► STOPPING ───────► OFF
//!    ▲   1. stop sampler thread     (≤3 s SLA)      │
//!    │   2. Close() WGC session, frame pool,        │ user toggles ON
//!    │      release D3D refs                        ▼
//!    └── 3. UnhookWinEvent / remove UIA handlers   STARTING: re-acquire WGC
//!        4. signal Doc 12 → kill vlm-host/stt-host  item/pool, re-register
//!        5. flip tray + overlay indicator to ⏸      hooks, resume sampler;
//!        6. emit capture_toggle(off) audit event    indicator ▶, emit
//!                                                    capture_toggle(on)
//! ```
//!
//! **Single writer:** the toggle *state* is owned by the Orchestration Manager
//! (`orchestration::ToggleOwner`); this subsystem **obeys** it and never flips the
//! state itself (doc 05 §5, doc 12). The methods here run the *mechanism* of a
//! transition that orchestration has already decided.

// TODO(M1): the toggle state machine lands in the M1 capture milestone.

use aperture_contracts::{Event, EventType};

use crate::CaptureError;

/// The capture lifecycle state (doc 05 §5). `Starting`/`Stopping` are transient;
/// the steady states are `On` and `Off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureState {
    /// Capturing: hooks installed, WGC acquired, sampler running.
    On,
    /// Transitioning OFF → ON (re-acquire WGC, re-register hooks, resume sampler).
    Starting,
    /// Transitioning ON → OFF (release steps 1–6, ≤ 3 s).
    Stopping,
    /// Fully released: no events, no frames, sidecars dead, VRAM → ~0.
    Off,
}

/// The 3 s OFF-release SLA (doc 05 §5, §7). Breach ⇒ force path.
pub const TOGGLE_OFF_SLA_MS: u64 = 3_000;

/// Runs the toggle *mechanism* under orchestration's single-writer ownership
/// (doc 05 §5, doc 12). Holds references to the things a transition must touch:
/// the sampler, the WGC sampler, the hook thread, and the orchestration handle
/// used to kill/start the GPU sidecars.
pub struct CaptureToggle {
    // state: CaptureState,
    // owner: aperture_orchestration::ToggleOwner,  // single writer (doc 12).
    // sampler: crate::sampler::Sampler,
    // wgc: crate::wgc::WgcSampler,
    // hooks: crate::hooks::HookThread,
    // bus: aperture_event_bus::Sender<Event>,
}

impl CaptureToggle {
    /// Bind to orchestration's single-writer toggle owner (doc 12). The toggle
    /// starts logically `Off`; nothing is acquired until orchestration drives
    /// [`Self::acquire`].
    pub fn new(
        // owner: aperture_orchestration::ToggleOwner,
        // bus: aperture_event_bus::Sender<Event>,
    ) -> Self {
        // TODO(M1): subscribe to the ToggleOwner; map its ON/OFF intents to
        //   acquire()/release() here (this crate obeys, never writes the state).
        todo!("M1: bind to orchestration::ToggleOwner")
    }

    /// Current observed state (doc 05 §5).
    pub fn state(&self) -> CaptureState {
        // TODO(M1): return the cached state mirror.
        todo!("M1: report current capture state")
    }

    /// STARTING → ON (doc 05 §5): re-acquire the WGC item/pool, re-register hooks,
    /// resume the sampler, flip the indicator ▶, then emit `capture_toggle(on)`.
    /// Invoked only when orchestration's ToggleOwner has decided ON.
    pub async fn acquire(&self) -> Result<(), CaptureError> {
        // TODO(M1):
        //   - state = Starting
        //   - wgc.acquire(); hooks.install(...); sampler resume (heartbeat task)
        //   - signal orchestration to (re)start vlm-host/stt-host on demand (doc 12)
        //   - flip tray + overlay indicator ▶ (doc 05 §5)
        //   - emit_toggle_event(true)
        //   - state = On
        todo!("M1: STARTING → ON acquire path")
    }

    /// ON → STOPPING → OFF (doc 05 §5): run release steps 1–6 within
    /// [`TOGGLE_OFF_SLA_MS`]. On timeout, escalate to [`Self::force_release`]
    /// (doc 05 §7). Invoked only when orchestration's ToggleOwner has decided OFF.
    pub async fn release(&self) -> Result<(), CaptureError> {
        // TODO(M1): wrap steps 1–6 in tokio::time::timeout(TOGGLE_OFF_SLA_MS).
        //   On Elapsed → force_release() + return CaptureError::ToggleSlaBreach
        //   (still completing the release; the error only flags the breach, doc 05 §7).
        //
        //   state = Stopping;
        //   1. self.sampler.suspend();                       // stop sampler thread
        //   2. self.wgc.release_all();                       // Close() WGC + D3D refs
        //   3. self.hooks.uninstall();                       // UnhookWinEvent / UIA
        //   4. orchestration: kill vlm-host + stt-host (process kill = VRAM release, doc 12 §5)
        //   5. flip tray + overlay indicator to ⏸ (doc 05 §5)
        //   6. self.emit_toggle_event(false);                // capture_toggle(off) audit
        //   state = Off;
        todo!("M1: ON → STOPPING → OFF release steps 1–6 (<3 s)")
    }

    /// Force path on SLA breach (doc 05 §7): **hard-kill** sidecars (process kill)
    /// and **force-release** WGC, regardless of the orderly path's progress. The
    /// guaranteed VRAM-release primitive is the process kill (doc 02 §2, doc 12 §5).
    pub fn force_release(&self) {
        // TODO(M1): orchestration.kill_sidecars_now(); wgc.release_all();
        //   hooks.uninstall(); log SLA breach (doc 05 §7).
        todo!("M1: hard-kill sidecars + force-release WGC on SLA breach")
    }

    /// Emit the `capture_toggle` audit event (doc 05 §5 steps 6 / on-ON; doc 12 §6).
    /// This audit row survives Purge All for 30 d (doc 03 §6, contracts:
    /// [`EventType::CaptureToggle`]).
    fn emit_toggle_event(&self, _on: bool) {
        // TODO(M1): publish Event { type: CaptureToggle, payload: { "on": <bool> } }
        //   on the bus → DB (the durable audit form, doc 15 §1).
        let _ = EventType::CaptureToggle;
        todo!("M1: emit capture_toggle audit event")
    }
}
