//! The capture enable/disable toggle (G8 / SC6) — state machine (doc 05 §5).
//!
//! Invariant (3): when capture is OFF the system guarantees **no events written,
//! no frames taken, sidecars dead, VRAM released** — verified at the M1 gate with
//! `nvidia-smi` (doc 05 §5). The OFF release must complete within a **3 s SLA**;
//! on breach we force-release WGC + hooks regardless (doc 05 §7); the sidecar
//! hard-kill is orchestration's step (doc 12 §6 step 4).
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
//! transition that orchestration has already decided. R2 note (FIX 2.1): once
//! the browser extension ships at M4, the OFF path also signals the
//! native-messaging host to stop forwarding — a step slot is reserved below.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aperture_contracts::{Event, EventType};
use aperture_event_bus::EventBus;

use crate::hooks::HookThread;
use crate::normalizer::EventStore;
use crate::sampler::{epoch_ms, Sampler};
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

impl CaptureState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => CaptureState::Off,
            1 => CaptureState::Starting,
            2 => CaptureState::On,
            _ => CaptureState::Stopping,
        }
    }
    fn as_u8(self) -> u8 {
        match self {
            CaptureState::Off => 0,
            CaptureState::Starting => 1,
            CaptureState::On => 2,
            CaptureState::Stopping => 3,
        }
    }
}

/// The 3 s OFF-release SLA (doc 05 §5, §7). Breach ⇒ force path.
pub const TOGGLE_OFF_SLA_MS: u64 = 3_000;

/// Runs the toggle *mechanism* under orchestration's single-writer ownership
/// (doc 05 §5, doc 12). Holds the things a transition must touch: the sampler
/// (which owns WGC), the hook thread, and the bus (for the audit event). The
/// GPU-sidecar kill is orchestration's own step 4 — this struct handles the
/// capture-side steps and reports timing so the SLA watchdog can act.
pub struct CaptureToggle {
    state: AtomicU8,
    sampler: Arc<Sampler>,
    hooks: Mutex<Option<HookThread>>,
    /// Installs the hook thread on demand (kept as a factory so ON can
    /// re-register after OFF released the previous one).
    hook_factory: Mutex<Box<dyn FnMut() -> Result<HookThread, CaptureError> + Send>>,
    bus: EventBus,
    /// The durable store for the `capture_toggle` audit rows — these MUST
    /// persist (they survive Purge All 30 d, doc 03 §6/doc 13 §7).
    store: Mutex<Option<Arc<dyn EventStore>>>,
}

impl CaptureToggle {
    /// Bind the mechanism to its parts. The toggle starts logically `Off`;
    /// nothing is acquired until orchestration drives [`Self::acquire`].
    pub fn new(
        sampler: Arc<Sampler>,
        hook_factory: Box<dyn FnMut() -> Result<HookThread, CaptureError> + Send>,
        bus: EventBus,
    ) -> Self {
        Self {
            state: AtomicU8::new(CaptureState::Off.as_u8()),
            sampler,
            hooks: Mutex::new(None),
            hook_factory: Mutex::new(hook_factory),
            bus,
            store: Mutex::new(None),
        }
    }

    /// Attach the durable store for audit rows (doc 13 §7). Wired by the shell.
    pub fn set_store(&self, store: Arc<dyn EventStore>) {
        *self.store.lock().expect("store lock") = Some(store);
    }

    /// Current observed state (doc 05 §5).
    pub fn state(&self) -> CaptureState {
        CaptureState::from_u8(self.state.load(Ordering::SeqCst))
    }

    /// STARTING → ON (doc 05 §5): re-acquire the WGC item/pool, re-register
    /// hooks, resume the sampler, then emit `capture_toggle(on)`. Indicator
    /// flips are the shell's reaction to the orchestration broadcast.
    /// Invoked only when orchestration's ToggleOwner has decided ON.
    /// A failed acquire rolls back fully: hooks must not keep feeding events
    /// while the mechanism reports a failed start, and the state returns to
    /// `Off` so the owner can honestly re-drive a retry (doc 05 §7).
    pub async fn acquire(&self) -> Result<(), CaptureError> {
        self.state.store(CaptureState::Starting.as_u8(), Ordering::SeqCst);

        let hooks = match (self.hook_factory.lock().expect("factory lock"))() {
            Ok(h) => h,
            Err(e) => {
                self.state.store(CaptureState::Off.as_u8(), Ordering::SeqCst);
                return Err(e);
            }
        };
        *self.hooks.lock().expect("hooks lock") = Some(hooks);
        if let Err(e) = self.sampler.resume() {
            // Partial acquisition: unhook before surfacing the failure.
            if let Some(mut hooks) = self.hooks.lock().expect("hooks lock").take() {
                hooks.uninstall();
            }
            self.state.store(CaptureState::Off.as_u8(), Ordering::SeqCst);
            return Err(e);
        }

        self.emit_toggle_event(true);
        self.state.store(CaptureState::On.as_u8(), Ordering::SeqCst);
        Ok(())
    }

    /// ON → STOPPING → OFF (doc 05 §5): run the capture-side release steps
    /// within [`TOGGLE_OFF_SLA_MS`]. The teardown is synchronous, potentially
    /// blocking work (WGC `Close()` can stall on a lost device; the hook
    /// uninstall joins the hook thread), so it runs on a blocking thread and
    /// the SLA timer races it — polled inline, the whole block would complete
    /// (or hang) inside a single poll and the timeout could never fire. On
    /// breach, [`Self::force_release`] runs on a *separate* blocking thread
    /// (it may contend on the same WGC lock the hung teardown holds) and the
    /// breach is surfaced as [`CaptureError::ToggleSlaBreach`] (doc 05 §7) —
    /// the caller's loop is never wedged. Invoked only when orchestration's
    /// ToggleOwner has decided OFF (orchestration itself kills the sidecars —
    /// its step 4 — in parallel).
    pub async fn release(self: &Arc<Self>) -> Result<(), CaptureError> {
        if self.state() == CaptureState::Off {
            // Idempotent: nothing acquired, nothing to release — and no
            // spurious capture_toggle(off) audit row (e.g. the shell's
            // revert-after-failed-start broadcasts Off to a toggle that
            // never left Off).
            return Ok(());
        }
        self.state.store(CaptureState::Stopping.as_u8(), Ordering::SeqCst);

        let this = Arc::clone(self);
        let result = race_release_sla(Duration::from_millis(TOGGLE_OFF_SLA_MS), move || {
            // 1. stop sampler thread + drop pending debounce; 2. Close() WGC +
            //    release D3D refs (both inside Sampler::suspend).
            this.sampler.suspend();
            // 3. UnhookWinEvent / stop the hook message loop.
            if let Some(mut hooks) = this.hooks.lock().expect("hooks lock").take() {
                hooks.uninstall();
            }
            // 3b. [M4 slot — FIX 2.1]: signal the native-messaging host to stop
            //     forwarding extension data (the extension is not built yet).
            // 4. sidecar kill: orchestration's step (doc 12 §6), not ours.
            // 5. indicator flip: the shell reacts to the state broadcast.
        })
        .await;

        // 6. audit event — emitted exactly once, breach or not (the breached
        //    OFF is still an OFF; the row survives Purge All 30 d, doc 03 §6).
        self.emit_toggle_event(false);
        self.state.store(CaptureState::Off.as_u8(), Ordering::SeqCst);

        if result.is_err() {
            // The orderly teardown is still limping on its blocking thread;
            // force-release what can be reclaimed without ever blocking the
            // caller (doc 05 §7).
            let this = Arc::clone(self);
            tokio::task::spawn_blocking(move || this.force_release());
        }
        result
    }
}

/// Race the blocking release steps against the OFF SLA (doc 05 §5, §7). The
/// work runs on a `spawn_blocking` thread so the timer is real: a stalled
/// `Close()`/thread-join cannot hold the caller past `sla`. A panicked
/// teardown is reported as a breach too — either way the force path is the
/// backstop.
async fn race_release_sla(
    sla: Duration,
    work: impl FnOnce() + Send + 'static,
) -> Result<(), CaptureError> {
    let handle = tokio::task::spawn_blocking(work);
    match tokio::time::timeout(sla, handle).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(join_err)) => {
            tracing::error!("toggle OFF teardown panicked: {join_err}");
            Err(CaptureError::ToggleSlaBreach)
        }
        Err(_elapsed) => Err(CaptureError::ToggleSlaBreach),
    }
}

impl CaptureToggle {
    /// Force path on SLA breach (doc 05 §7): force-release WGC and the hooks,
    /// regardless of the orderly path's progress. The guaranteed VRAM-release
    /// primitive — the sidecar process kill — is orchestration's (doc 12 §5).
    pub fn force_release(&self) {
        tracing::error!("toggle OFF exceeded {TOGGLE_OFF_SLA_MS} ms SLA — forcing release (doc 05 §7)");
        self.sampler.suspend();
        if let Some(mut hooks) = self.hooks.lock().expect("hooks lock").take() {
            hooks.uninstall();
        }
    }

    /// Emit the `capture_toggle` audit event (doc 05 §5 step 6; doc 12 §6):
    /// persisted first (the audit row survives Purge All 30 d, doc 03 §6),
    /// then notified on the bus.
    fn emit_toggle_event(&self, on: bool) {
        let mut ev = Event {
            id: 0,
            ts: epoch_ms(),
            r#type: EventType::CaptureToggle,
            app: None,
            process: None,
            window_title: None,
            payload: serde_json::json!({ "on": on, "reason": "user_action" }),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        };
        if let Some(store) = self.store.lock().expect("store lock").as_ref() {
            if let Some(id) = store.persist(&ev) {
                ev.id = id;
            }
        }
        let _ = self.bus.publish(ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exclusion::ExclusionList;
    use crate::sampler::DropSink;
    use crate::wgc::WgcSampler;
    use crate::CaptureConfig;

    fn toggle() -> Arc<CaptureToggle> {
        let sampler = Sampler::new(
            CaptureConfig::default(),
            WgcSampler::new(),
            ExclusionList::shipped_defaults(),
            Arc::new(DropSink),
        );
        Arc::new(CaptureToggle::new(
            sampler,
            // Test factory: pretend hook install fails cleanly off a desktop
            // session; state transitions are what this test asserts.
            Box::new(|| Err(CaptureError::HookFailed("test".into()))),
            EventBus::new(),
        ))
    }

    #[tokio::test]
    async fn release_emits_audit_event_and_lands_off_within_sla() {
        let t = toggle();
        let mut rx = t.bus.subscribe();

        // Releasing while already Off is a no-op: no audit row (e.g. the
        // shell's revert-after-failed-start broadcasts Off to a toggle that
        // never left Off).
        t.release().await.expect("no-op release");
        assert!(rx.try_recv().is_err(), "no audit event for a no-op release");

        // Simulate an acquired toggle (the test hook factory cannot succeed),
        // then release for real.
        t.state.store(CaptureState::On.as_u8(), Ordering::SeqCst);
        let started = std::time::Instant::now();
        t.release().await.expect("release path");
        assert!(started.elapsed() < Duration::from_millis(TOGGLE_OFF_SLA_MS));
        assert_eq!(t.state(), CaptureState::Off);
        let ev = rx.try_recv().expect("audit event");
        assert_eq!(ev.r#type, EventType::CaptureToggle);
        assert_eq!(ev.payload["on"], serde_json::json!(false));
    }

    /// The SLA race must be real: a teardown that outlives the SLA is reported
    /// as a breach (doc 05 §7) instead of silently blocking the caller.
    #[tokio::test]
    async fn sla_race_flags_breach_when_teardown_stalls() {
        let r = race_release_sla(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_millis(400));
        })
        .await;
        assert!(matches!(r, Err(CaptureError::ToggleSlaBreach)));
    }

    #[tokio::test]
    async fn sla_race_passes_fast_teardown() {
        race_release_sla(Duration::from_millis(TOGGLE_OFF_SLA_MS), || {})
            .await
            .expect("fast teardown is within SLA");
    }

    #[tokio::test]
    async fn acquire_failure_leaves_a_diagnosable_state() {
        let t = toggle();
        let err = t.acquire().await.expect_err("test factory fails");
        assert!(matches!(err, CaptureError::HookFailed(_)));
        // A failed start rolls back to Off (never stuck in Starting) so the
        // owner can re-drive a retry; the error return is the shell's signal
        // to surface it (doc 05 §7).
        assert_eq!(t.state(), CaptureState::Off);
    }
}
