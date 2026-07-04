//! Tier-0 capture subsystem (doc 05): the always-on, CPU-only sensory layer.
//!
//! Pipeline (doc 05 §6):
//! `hook thread → debouncer → sampler → (frame → OCR) + (event → normalizer → bus)`.
//!
//! Three invariants this crate must honor:
//! 1. **VRAM ceiling** — capture is CPU-only and costs ~0 VRAM (doc 05 §1). It
//!    never touches the GPU; it only *signals* orchestration to start/kill the
//!    GPU sidecars across the toggle (doc 12, doc 05 §5 step 4).
//! 2. **Transparency gate** — capture opens **no** network sockets and spawns
//!    **no** Claude CLI; only `aperture-reasoning-gateway` may (doc 13 §2). Frames
//!    are ephemeral (downscale → OCR → drop), never written to disk (doc 05 §2).
//! 3. **Capture toggle** — OFF releases the WGC session, unhooks WinEvents, and
//!    signals sidecar kill so VRAM → ~0 in <3 s (doc 05 §5, [`toggle`]).
//!
//! Boundary discipline (doc 15): capture depends on `aperture-contracts` (the
//! [`Event`] envelope), `aperture-event-bus` (transport), and
//! `aperture-orchestration` (the single toggle writer) — nothing else.

pub mod exclusion;
pub mod hooks;
pub mod normalizer;
pub mod phash;
pub mod sampler;
pub mod toggle;
pub mod uia;
pub mod wgc;

use std::sync::{Arc, Mutex};

use aperture_event_bus::EventBus;

use crate::exclusion::ExclusionList;
use crate::hooks::{HookEvent, HookThread, WindowIdentity};
use crate::normalizer::Normalizer;
use crate::sampler::{epoch_ms, FrameSink, Sampler};
use crate::toggle::CaptureToggle;
use crate::wgc::WgcSampler;

/// Errors surfaced by the capture subsystem. Most failure modes (doc 05 §7) are
/// *recoverable* by design — capture degrades to event-only rather than dying.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// WGC unsupported, or capture denied for a window (DRM / secure desktop).
    /// Degrade that context to event-only mode (doc 05 §7).
    #[error("screen capture unavailable: {0}")]
    CaptureUnavailable(String),
    /// A WinEvent hook or UIA handler could not be installed (doc 05 §3).
    #[error("hook installation failed: {0}")]
    HookFailed(String),
    /// The Direct3D11 frame-pool device was lost; recreate, else degrade (doc 05 §7).
    #[error("frame pool device lost")]
    DeviceLost,
    /// The OFF release path exceeded its 3 s SLA (doc 05 §7); WGC + hooks were
    /// force-released and the breach is surfaced once.
    #[error("toggle OFF exceeded 3s SLA")]
    ToggleSlaBreach,
}

/// Configuration for the capture subsystem. Durations are the doc 05 §4 values
/// (R2: heartbeat adaptive per ADR-032); user-adjustable via settings (doc 13 §7).
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Coalesce focus storms (doc 05 §4 [ASSUMPTION] 300 ms).
    pub debounce_ms: u64,
    /// Heartbeat cadence while the user is active — **adaptive ~5–20 s**
    /// (ADR-032/Q41: modulated by input activity + event density), 10 s default.
    pub heartbeat_default_ms: u64,
    /// Adaptive heartbeat floor (ADR-032). Never sample faster than this.
    pub heartbeat_min_ms: u64,
    /// Adaptive heartbeat ceiling (ADR-032).
    pub heartbeat_max_ms: u64,
    /// "User is active" window — input seen within this span (doc 05 §4, 60 s).
    pub active_window_ms: u64,
    /// Longest edge after downscale before handing a frame to OCR (doc 05 §4).
    pub max_frame_edge_px: u32,
    /// pHash near-duplicate gate (ADR-032/Q72): skip OCR/embed when a new
    /// frame's pHash is within this Hamming distance of the last frame's.
    /// [ASSUMPTION — start at 4, tuned at M2]. The gate only *removes* work;
    /// it can never delay a bubble (Doc 21 §2).
    pub phash_hamming_threshold: u32,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        // TODO(M9): load overrides from settings (doc 13 §7) instead of hardcoding.
        Self {
            debounce_ms: 300,
            heartbeat_default_ms: 10_000,
            heartbeat_min_ms: 5_000,
            heartbeat_max_ms: 20_000,
            active_window_ms: 60_000,
            max_frame_edge_px: 1600,
            phash_hamming_threshold: 4,
        }
    }
}

/// What prompted a sample (doc 05 §4). Trigger samples are debounced; the
/// heartbeat is independent and suspended while the user is idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleTrigger {
    FocusChange,
    WindowOpen,
    Navigation,
    TitleChange,
    /// Periodic adaptive heartbeat while active (doc 05 §4, ADR-032).
    Heartbeat,
}

/// The capture subsystem facade. Owns the hook thread (via the toggle), the WGC
/// sampler, the debouncer/heartbeat, and the normalizer, wiring them to the bus.
///
/// Lifecycle: construct with [`CaptureSubsystem::new`]; orchestration drives
/// [`CaptureSubsystem::start`] / [`CaptureSubsystem::stop`] across the toggle —
/// this struct never flips the toggle itself (doc 05 §5). The shell's
/// composition root subscribes to orchestration's `ToggleOwner` broadcast and
/// calls start/stop accordingly (doc 12 §6).
pub struct CaptureSubsystem {
    sampler: Arc<Sampler>,
    toggle: Arc<CaptureToggle>,
    normalizer: Arc<Normalizer>,
    /// The most recent foreground identity (heartbeat samples re-use it).
    current_identity: Arc<Mutex<WindowIdentity>>,
    /// Drain + heartbeat tasks, aborted on drop.
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl CaptureSubsystem {
    /// Wire the subsystem. Does **not** start capture — capture begins only when
    /// orchestration flips the toggle ON (doc 05 §5).
    ///
    /// `bus` is the publish handle for normalized [`Event`]s (doc 15 §1);
    /// `sink` is where sampled frames go (M2: the vision-ocr FrameProcessor;
    /// M1 bring-up: [`sampler::DropSink`]).
    pub fn new(
        config: CaptureConfig,
        bus: EventBus,
        exclusion: ExclusionList,
        sink: Arc<dyn FrameSink>,
    ) -> Arc<Self> {
        let sampler = Sampler::new(config, WgcSampler::new(), exclusion.clone(), sink);
        let normalizer = Arc::new(Normalizer::new(bus.clone(), exclusion));
        let current_identity = Arc::new(Mutex::new(WindowIdentity::default()));

        // The hook→pipeline channel: std mpsc out of the C callback, bridged to
        // tokio by a blocking drain task spawned in `start`.
        let (hook_tx, hook_rx) = std::sync::mpsc::channel::<HookEvent>();
        let hook_rx = Arc::new(Mutex::new(hook_rx));

        let subsystem = Arc::new(Self {
            sampler: Arc::clone(&sampler),
            toggle: Arc::new(CaptureToggle::new(
                Arc::clone(&sampler),
                Box::new(move || HookThread::install(hook_tx.clone())),
                bus,
            )),
            normalizer,
            current_identity,
            tasks: Mutex::new(Vec::new()),
        });

        // Spawn the drain task once; it idles (blocking recv) while capture is
        // OFF because the hook thread only exists between acquire/release.
        let me = Arc::clone(&subsystem);
        let drain = tokio::task::spawn_blocking(move || {
            let rx = hook_rx.lock().expect("hook rx lock");
            while let Ok(raw) = rx.recv() {
                me.on_hook_event(raw);
            }
        });
        // Heartbeat task (parks itself while suspended).
        let hb = tokio::spawn(
            Arc::clone(&subsystem.sampler).run_heartbeat(Arc::clone(&subsystem.current_identity)),
        );
        subsystem.tasks.lock().expect("tasks lock").extend([drain, hb]);

        subsystem
    }

    /// One raw hook event through the pipeline: identity → normalize/publish →
    /// (maybe) frame sample (doc 05 §6).
    fn on_hook_event(&self, raw: HookEvent) {
        let now = epoch_ms();
        self.sampler.note_activity(now);

        let hwnd = match raw {
            HookEvent::ForegroundChanged { hwnd }
            | HookEvent::WindowOpened { hwnd }
            | HookEvent::WindowClosed { hwnd }
            | HookEvent::TitleChanged { hwnd } => hwnd,
        };
        let identity = hooks::window_identity(hwnd);
        if matches!(raw, HookEvent::ForegroundChanged { .. } | HookEvent::TitleChanged { .. }) {
            *self.current_identity.lock().expect("identity lock") = identity.clone();
        }

        let normalized = self.normalizer.normalize_hook(&raw, identity, now);
        for n in normalized {
            if n.capture_frame {
                let trigger = match (&raw, n.event.r#type) {
                    (_, aperture_contracts::EventType::Navigation) => SampleTrigger::Navigation,
                    (HookEvent::WindowOpened { .. }, _) => SampleTrigger::WindowOpen,
                    (HookEvent::TitleChanged { .. }, _) => SampleTrigger::TitleChange,
                    _ => SampleTrigger::FocusChange,
                };
                self.sampler.request(trigger, n.identity, now);
            }
        }
    }

    /// STARTING → ON: re-acquire the WGC item/pool, re-register hooks, resume the
    /// sampler, emit `capture_toggle(on)` (doc 05 §5).
    ///
    /// Invariant (3): only ever called in response to orchestration's ToggleOwner.
    pub async fn start(&self) -> Result<(), CaptureError> {
        self.toggle.acquire().await
    }

    /// ON → STOPPING → OFF: run the capture-side release steps of doc 05 §5
    /// within the 3 s SLA (WGC + hooks; orchestration kills the sidecars).
    ///
    /// Invariant (3): OFF ⇒ no events, no frames; VRAM release completes when
    /// orchestration's kill lands (doc 12 §6).
    pub async fn stop(&self) -> Result<(), CaptureError> {
        self.toggle.release().await
    }

    /// Current toggle-mechanism state (diagnostics; the authoritative state is
    /// orchestration's ToggleOwner).
    pub fn state(&self) -> toggle::CaptureState {
        self.toggle.state()
    }

    /// pHash-gate + delivery counters (M2 tuning telemetry).
    pub fn frame_counters(&self) -> (u64, u64) {
        (
            self.sampler
                .phash_suppressed
                .load(std::sync::atomic::Ordering::Relaxed),
            self.sampler
                .frames_delivered
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }
}

impl Drop for CaptureSubsystem {
    fn drop(&mut self) {
        for t in self.tasks.lock().expect("tasks lock").drain(..) {
            t.abort();
        }
    }
}
