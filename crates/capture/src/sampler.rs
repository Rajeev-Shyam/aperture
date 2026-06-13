//! Event-driven sampling policy (doc 05 §4) — not FPS-based.
//!
//! Two sampling clocks:
//! - **Trigger samples** on focus change / window open / navigation / title
//!   change of the foreground, **debounced 300 ms** so focus storms coalesce
//!   (doc 05 §4 [ASSUMPTION]).
//! - **Heartbeat samples** every **10 s** while the user is *active* (input within
//!   60 s); suspended while idle (doc 05 §4 [ASSUMPTION]).
//!
//! Per-sample work (doc 05 §4): capture one frame of the foreground monitor →
//! crop to the foreground window rect when cheap → **downscale to ≤ 1600 px** →
//! hand the ephemeral frame to vision-ocr. The frame is dropped immediately after
//! (doc 05 §2) — invariant (2) at the source.
//!
//! **Exclusion is enforced HERE, before any frame is pulled or OCR runs** (doc 05
//! §4, the earliest gate, doc 13). If the foreground context is excluded, no
//! frame is captured: the sampler emits a metadata-only observation with
//! `redaction_flags |= EXCLUDED` and skips OCR + connector capture entirely.

// TODO(M1): the sampler lands in the M1 capture milestone.

use crate::exclusion::ExclusionList;
use crate::hooks::WindowIdentity;
use crate::wgc::{EphemeralFrame, WgcSampler};
use crate::{CaptureConfig, CaptureError, SampleTrigger};

/// Coalesces trigger samples (300 ms) and runs the heartbeat (10 s while active).
/// The single funnel through which every sample request reaches the WGC sampler
/// (doc 05 §4, §6).
pub struct Sampler {
    // config: CaptureConfig,
    // wgc: WgcSampler,
    // exclusion: ExclusionList,
    // ocr: aperture_vision_ocr::OcrHandle,            // not a dep yet; wired M2.
    // last_trigger_at: tokio::time::Instant,
    // last_input_at: std::sync::Arc<std::sync::atomic::AtomicI64>,
}

impl Sampler {
    /// Build the sampler. Holds the WGC sampler and the exclusion list so the
    /// exclusion gate can run before any frame pull (doc 05 §4).
    pub fn new(_config: CaptureConfig, _wgc: WgcSampler, _exclusion: ExclusionList) -> Self {
        // TODO(M1): init debounce/heartbeat timers; subscribe to input-activity.
        todo!("M1: construct the sampler")
    }

    /// Enqueue a sample request. Trigger samples are debounced by
    /// `config.debounce_ms`; a heartbeat trigger bypasses the debounce but is
    /// suppressed while the user is idle (doc 05 §4).
    pub fn request(&self, _trigger: SampleTrigger) {
        // TODO(M1): if trigger != Heartbeat, reset the 300 ms debounce timer;
        //   on fire, call sample_once. Heartbeat path: skip if idle > active_window_ms.
        todo!("M1: debounce trigger samples / gate the heartbeat on activity")
    }

    /// Perform exactly one sample for `identity` (doc 05 §4).
    ///
    /// 1. **Exclusion gate FIRST** — if excluded, return a metadata-only
    ///    observation (`EXCLUDED`, no frame, no OCR, no connector capture).
    /// 2. Otherwise pull one foreground frame, crop, downscale ≤ 1600 px.
    /// 3. Hand the ephemeral frame to vision-ocr; drop it immediately after.
    pub async fn sample_once(&self, _identity: &WindowIdentity) -> Result<(), CaptureError> {
        // TODO(M1):
        //   1. if self.exclusion.is_excluded(process, class, title): emit metadata
        //      -only observation w/ redaction_flags::EXCLUDED and RETURN (doc 05 §4).
        //   2. wgc.pull_foreground → crop_to(window rect) → downscale_for_ocr.
        //   3. ocr.submit(frame) — frame is moved + dropped (doc 05 §2).
        //      WGC failure for this window → event-only mode (doc 05 §7).
        todo!("M1: exclusion gate → frame → downscale → OCR (frame ephemeral)")
    }

    /// Drive the 10 s heartbeat while active; suspend while idle (doc 05 §4).
    /// Runs as a background task; stopped on the toggle's STOPPING transition.
    pub async fn run_heartbeat(&self) {
        // TODO(M1): loop { sleep(heartbeat_ms); if active { request(Heartbeat) } }.
        todo!("M1: heartbeat loop (suspend while idle)")
    }

    /// Stop the heartbeat and drop any pending debounced sample (doc 05 §5 step 1:
    /// "stop sampler thread"). Idempotent.
    pub fn suspend(&self) {
        // TODO(M1): cancel debounce timer; signal the heartbeat task to exit.
        todo!("M1: suspend the sampler (OFF path step 1)")
    }
}

/// Downscale an ephemeral frame so its longest edge is ≤ `max_edge_px` before OCR
/// (doc 05 §4; OCR-side budget doc 06). The input frame is consumed and dropped.
pub fn downscale_for_ocr(_frame: EphemeralFrame, _max_edge_px: u32) -> EphemeralFrame {
    // TODO(M2): bilinear/area downscale on the staging texture or CPU buffer;
    //   keep aspect ratio; never up-scale.
    todo!("M2: downscale ephemeral frame to <=max_edge_px longest edge")
}

/// True if the user has produced input within `active_window_ms` (doc 05 §4).
/// Drives heartbeat suspension. Implemented via `GetLastInputInfo` [VERIFY].
pub fn user_is_active(_active_window_ms: u64) -> bool {
    // TODO(M1): GetLastInputInfo → idle = now - dwTime; active = idle < window.
    todo!("M1: input-activity check for heartbeat gating")
}
