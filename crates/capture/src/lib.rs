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

// TODO(M0): contracts/schema/shell land first; capture is the M1 milestone.

pub mod exclusion;
pub mod hooks;
pub mod normalizer;
pub mod sampler;
pub mod toggle;
pub mod uia;
pub mod wgc;

use aperture_contracts::Event;

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
    /// The OFF release path exceeded its 3 s SLA (doc 05 §7); sidecars are
    /// hard-killed and WGC force-released.
    #[error("toggle OFF exceeded 3s SLA")]
    ToggleSlaBreach,
}

/// Configuration for the capture subsystem. Durations are the doc 05 §4
/// assumptions; values are user-adjustable via settings (doc 13 §7).
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
        // TODO(M1): load overrides from settings (doc 13 §7) instead of hardcoding.
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

/// The capture subsystem facade. Owns the hook thread, the WGC sampler, the
/// debouncer/heartbeat, the normalizer, and the toggle state machine, wiring
/// them to the bus and to orchestration's single toggle writer.
///
/// Lifecycle: construct with [`CaptureSubsystem::new`]; orchestration drives
/// [`CaptureSubsystem::start`] / [`CaptureSubsystem::stop`] across the toggle —
/// this struct never flips the toggle itself (doc 05 §5).
pub struct CaptureSubsystem {
    // config: CaptureConfig,
    // bus: aperture_event_bus::Sender<Event>,
    // toggle: toggle::CaptureToggle,
    // exclusion: exclusion::ExclusionList,
    // hooks: hooks::HookThread,
    // sampler: sampler::Sampler,
    // wgc: wgc::WgcSampler,
}

impl CaptureSubsystem {
    /// Wire the subsystem. Does **not** start capture — capture begins only when
    /// orchestration flips the toggle ON (doc 05 §5).
    ///
    /// `bus` is the publish handle for normalized [`Event`]s (doc 15 §1);
    /// `toggle_owner` is orchestration's single-writer toggle (doc 12).
    pub fn new(
        _config: CaptureConfig,
        // bus: aperture_event_bus::Sender<Event>,
        // toggle_owner: aperture_orchestration::ToggleOwner,
        _exclusion: exclusion::ExclusionList,
    ) -> Self {
        // TODO(M1): construct hook thread, WGC sampler, debouncer; subscribe to
        //   orchestration's ToggleOwner so STARTING/STOPPING transitions drive
        //   start()/stop() here (doc 05 §5).
        todo!("M1: wire hooks + WGC sampler + normalizer to the bus")
    }

    /// STARTING → ON: re-acquire the WGC item/pool, re-register hooks, resume the
    /// sampler, flip the indicator ▶, emit `capture_toggle(on)` (doc 05 §5).
    ///
    /// Invariant (3): only ever called in response to orchestration's ToggleOwner.
    pub async fn start(&self) -> Result<(), CaptureError> {
        // TODO(M1): hooks::install + wgc::acquire + sampler::resume; emit audit event.
        todo!("M1: STARTING → ON")
    }

    /// ON → STOPPING → OFF: run release steps 1–6 of doc 05 §5 within the 3 s SLA.
    /// Delegates the state machine to [`toggle::CaptureToggle`].
    ///
    /// Invariant (3): OFF ⇒ no events, no frames, sidecars dead, VRAM → ~0.
    pub async fn stop(&self) -> Result<(), CaptureError> {
        // TODO(M1): toggle::CaptureToggle::release() (steps 1–6); verify <3 s.
        todo!("M1: ON → STOPPING → OFF (steps 1–6, <3 s)")
    }

    /// Best-effort foreground sample request (debounced upstream). Exposed so the
    /// heartbeat and hook handlers share one entry point into the sampler.
    pub fn request_sample(&self, _trigger: SampleTrigger) {
        // TODO(M1): forward to sampler::Sampler::request (debounce applies, doc 05 §4).
        todo!("M1: enqueue a debounced sample")
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
    /// Periodic 10 s heartbeat while active (doc 05 §4).
    Heartbeat,
}

/// A normalized observation ready for the bus, paired with whether the sampler
/// should also pull a frame for OCR. Excluded contexts yield metadata-only
/// events with no frame (doc 05 §4).
#[doc(hidden)]
pub(crate) struct Observation {
    pub event: Event,
    pub capture_frame: bool,
}
