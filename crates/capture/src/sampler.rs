//! Event-driven sampling policy (doc 05 §4) — not FPS-based.
//!
//! Two sampling clocks:
//! - **Trigger samples** on focus change / window open / navigation / title
//!   change of the foreground, **debounced 300 ms** so focus storms coalesce
//!   (doc 05 §4 [ASSUMPTION]).
//! - **Heartbeat samples** on an **adaptive ~5–20 s** cadence (ADR-032/Q41:
//!   modulated by input activity + event density, 10 s default) while the user
//!   is *active* (input within 60 s); suspended while idle (doc 05 §4).
//!
//! Per-sample work (doc 05 §4): pull one frame of the primary monitor → crop to
//! the foreground window rect when cheap → **pHash near-duplicate gate**
//! (ADR-032/Q72 — skip OCR/embed on a static screen; the gate only removes
//! work) → hand the ephemeral frame to the OCR sink → **frame dropped**
//! (doc 05 §2) — invariant (2) at the source.
//!
//! **Exclusion is enforced HERE, before any frame is pulled or OCR runs**
//! (doc 05 §4, the earliest gate, doc 13). If the foreground context is
//! excluded, no frame is captured: the caller emits a metadata-only event with
//! `redaction_flags |= EXCLUDED` and skips OCR + connector capture entirely.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::exclusion::{ExclusionList, ExclusionVerdict};
use crate::hooks::WindowIdentity;
use crate::phash::{dhash64_bgra, to_hex, NearDuplicateGate};
use crate::wgc::{EphemeralFrame, WgcSampler, WindowRect};
use crate::{CaptureConfig, CaptureError, SampleTrigger};

/// Context handed to the OCR sink with each frame: which foreground identity it
/// belongs to and the frame's `thumb_phash` (doc 03 §3 — the only frame-derived
/// artifact that persists).
#[derive(Debug, Clone)]
pub struct FrameContext {
    pub identity: WindowIdentity,
    pub trigger: SampleTrigger,
    /// 16-hex-char dHash of the (cropped) frame (doc 03 §3 `thumb_phash`).
    pub thumb_phash: String,
    /// epoch ms of the sample.
    pub ts: i64,
    /// The DB id of the event this frame belongs to (`0` = none, e.g. a
    /// heartbeat sample — the store sink then writes event+context atomically).
    pub event_id: i64,
}

/// Where sampled frames go (M2 wires `aperture-vision-ocr::FrameProcessor` here;
/// tests wire a collector). The frame is MOVED in and dropped by the consumer —
/// never stored (doc 05 §2).
pub trait FrameSink: Send + Sync {
    fn submit(&self, frame: EphemeralFrame, ctx: FrameContext);
}

/// A no-op sink for M1 bring-up (frames are hashed for the gate, then dropped
/// immediately — OCR lands at M2).
pub struct DropSink;
impl FrameSink for DropSink {
    fn submit(&self, _frame: EphemeralFrame, _ctx: FrameContext) {}
}

/// Coalesces trigger samples (300 ms) and runs the adaptive heartbeat (doc 05
/// §4, ADR-032). The single funnel through which every sample request reaches
/// the WGC sampler.
pub struct Sampler {
    config: CaptureConfig,
    wgc: Mutex<WgcSampler>,
    exclusion: ExclusionList,
    gate: Mutex<NearDuplicateGate>,
    sink: Arc<dyn FrameSink>,
    /// Latest debounce-pending identity (focus storms overwrite; one sample fires).
    pending: Mutex<Option<(SampleTrigger, WindowIdentity, i64)>>,
    /// Whether a debounce timer is armed.
    debounce_armed: AtomicBool,
    /// Suspended (toggle OFF / idle) — heartbeat + debounce both stop.
    suspended: AtomicBool,
    /// Events observed in the trailing minute (heartbeat density modulation).
    recent_events: Mutex<Vec<i64>>,
    /// Suppressed-by-pHash counter (M2 tuning telemetry).
    pub phash_suppressed: AtomicU64,
    /// Frames delivered to the sink.
    pub frames_delivered: AtomicU64,
    /// Last observed input activity, epoch ms (fed by [`Sampler::note_activity`]
    /// or `GetLastInputInfo` on Windows).
    last_activity_ms: AtomicI64,
}

impl Sampler {
    /// Build the sampler. Holds the WGC sampler and the exclusion list so the
    /// exclusion gate can run before any frame pull (doc 05 §4).
    pub fn new(
        config: CaptureConfig,
        wgc: WgcSampler,
        exclusion: ExclusionList,
        sink: Arc<dyn FrameSink>,
    ) -> Arc<Self> {
        let threshold = config.phash_hamming_threshold;
        Arc::new(Self {
            config,
            wgc: Mutex::new(wgc),
            exclusion,
            gate: Mutex::new(NearDuplicateGate::new(threshold)),
            sink,
            pending: Mutex::new(None),
            debounce_armed: AtomicBool::new(false),
            suspended: AtomicBool::new(true), // starts suspended; toggle ON resumes
            recent_events: Mutex::new(Vec::new()),
            phash_suppressed: AtomicU64::new(0),
            frames_delivered: AtomicU64::new(0),
            last_activity_ms: AtomicI64::new(0),
        })
    }

    /// Acquire WGC resources (STARTING transition, doc 05 §5) and resume.
    pub fn resume(self: &Arc<Self>) -> Result<(), CaptureError> {
        self.wgc.lock().expect("wgc lock").acquire()?;
        self.gate.lock().expect("gate lock").reset(); // fresh eyes after OFF→ON
        self.suspended.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Stop the heartbeat and drop any pending debounced sample, then release
    /// WGC (doc 05 §5 steps 1-2). Idempotent.
    pub fn suspend(&self) {
        self.suspended.store(true, Ordering::SeqCst);
        *self.pending.lock().expect("pending lock") = None;
        self.wgc.lock().expect("wgc lock").release_all();
    }

    /// Note user/system activity at `now_ms` (drives heartbeat gating + the
    /// adaptive cadence, ADR-032). The pipeline calls this on every hook event.
    pub fn note_activity(&self, now_ms: i64) {
        self.last_activity_ms.store(now_ms, Ordering::Relaxed);
        let mut ev = self.recent_events.lock().expect("events lock");
        ev.push(now_ms);
        let floor = now_ms - 60_000;
        ev.retain(|&t| t > floor);
    }

    /// True if activity was seen within `active_window_ms` (doc 05 §4).
    pub fn user_is_active(&self, now_ms: i64) -> bool {
        now_ms - self.last_activity_ms.load(Ordering::Relaxed)
            < self.config.active_window_ms as i64
    }

    /// The adaptive heartbeat interval (ADR-032/Q41): default 10 s, pulled
    /// toward 5 s when event density is high (≥12 events/min) and toward 20 s
    /// when the minute was quiet (≤2 events). `[ASSUMPTION — tuned at M1/M2]`.
    pub fn heartbeat_interval_ms(&self) -> u64 {
        let density = self.recent_events.lock().expect("events lock").len();
        if density >= 12 {
            self.config.heartbeat_min_ms
        } else if density <= 2 {
            self.config.heartbeat_max_ms
        } else {
            self.config.heartbeat_default_ms
        }
    }

    /// Enqueue a sample request. Trigger samples are debounced by
    /// `config.debounce_ms` (latest identity wins); a heartbeat trigger bypasses
    /// the debounce but is suppressed while the user is idle (doc 05 §4).
    ///
    /// `event_id` is the durable row this frame will attach to (`0` for
    /// heartbeat samples — the sink then writes event+context atomically).
    pub fn request(
        self: &Arc<Self>,
        trigger: SampleTrigger,
        identity: WindowIdentity,
        event_id: i64,
        now_ms: i64,
    ) {
        if self.suspended.load(Ordering::SeqCst) {
            return;
        }
        if trigger == SampleTrigger::Heartbeat {
            if !self.user_is_active(now_ms) {
                return; // idle ⇒ heartbeat suspended (doc 05 §4)
            }
            self.sample_once(trigger, identity, event_id, now_ms);
            return;
        }

        // Debounce: remember the latest identity; arm one timer.
        *self.pending.lock().expect("pending lock") = Some((trigger, identity, event_id));
        if self
            .debounce_armed
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let me = Arc::clone(self);
            let debounce = std::time::Duration::from_millis(self.config.debounce_ms);
            tokio::spawn(async move {
                tokio::time::sleep(debounce).await;
                me.debounce_armed.store(false, Ordering::SeqCst);
                let fired = me.pending.lock().expect("pending lock").take();
                if let Some((trigger, identity, event_id)) = fired {
                    if !me.suspended.load(Ordering::SeqCst) {
                        let now = epoch_ms();
                        me.sample_once(trigger, identity, event_id, now);
                    }
                }
            });
        }
    }

    /// Perform exactly one sample for `identity` (doc 05 §4).
    ///
    /// 1. **Exclusion gate FIRST** — if excluded, return silently (the caller
    ///    already emitted the metadata-only event; no frame, no OCR).
    /// 2. Pull one frame, crop to the foreground rect when known.
    /// 3. **pHash gate** — near-duplicate frames stop here (ADR-032/Q72).
    /// 4. Hand the ephemeral frame to the sink; it is moved + dropped there.
    ///    WGC failure for this window → event-only mode (doc 05 §7).
    pub fn sample_once(
        &self,
        trigger: SampleTrigger,
        identity: WindowIdentity,
        event_id: i64,
        now_ms: i64,
    ) {
        // 1. exclusion gate (earliest, doc 05 §4 / doc 13 §4).
        if self
            .exclusion
            .is_excluded(
                identity.process.as_deref(),
                identity.window_class.as_deref(),
                identity.window_title.as_deref(),
                None,
            )
            .is_excluded()
        {
            return;
        }

        // 2. one frame, on demand.
        let frame = {
            let wgc = self.wgc.lock().expect("wgc lock");
            let Some(monitor) = wgc.primary_monitor() else { return };
            match wgc.pull_foreground(monitor) {
                Ok(f) => f,
                Err(e) => {
                    // Event-only mode for this context (doc 05 §7) — never fatal.
                    tracing::debug!(%e, "frame pull failed; event-only");
                    return;
                }
            }
        };
        let frame = match foreground_rect() {
            Some(rect) => frame.crop_to(rect),
            None => frame,
        };

        // 3. pHash near-duplicate gate (before OCR — it only removes work).
        let hash = dhash64_bgra(frame.bgra(), frame.width as usize, frame.height as usize);
        if self.gate.lock().expect("gate lock").is_duplicate(hash) {
            self.phash_suppressed.fetch_add(1, Ordering::Relaxed);
            return;
        }

        // 4. hand off; the frame is moved and dropped by the sink (doc 05 §2).
        self.frames_delivered.fetch_add(1, Ordering::Relaxed);
        self.sink.submit(
            frame,
            FrameContext {
                identity,
                trigger,
                thumb_phash: to_hex(hash),
                ts: now_ms,
                event_id,
            },
        );
    }

    /// Drive the adaptive heartbeat while active; suspend while idle (doc 05 §4,
    /// ADR-032). Runs as a background task; exits when `stop` resolves (the
    /// toggle's STOPPING transition aborts it).
    pub async fn run_heartbeat(self: Arc<Self>, current_identity: Arc<Mutex<WindowIdentity>>) {
        loop {
            let interval = self.heartbeat_interval_ms();
            tokio::time::sleep(std::time::Duration::from_millis(interval)).await;
            if self.suspended.load(Ordering::SeqCst) {
                continue; // parked; resume flips the flag
            }
            let now = epoch_ms();
            let identity = current_identity.lock().expect("identity lock").clone();
            // Heartbeats carry no pre-existing event row (event_id 0): the sink
            // writes event+context in one transaction (doc 02 §4 step 5).
            self.request(SampleTrigger::Heartbeat, identity, 0, now);
        }
    }
}

/// The foreground window's rect in screen physical px (doc 05 §4), when cheap.
#[cfg(windows)]
fn foreground_rect() -> Option<WindowRect> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        Some(WindowRect {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        })
    }
}

#[cfg(not(windows))]
fn foreground_rect() -> Option<WindowRect> {
    None
}

pub(crate) fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_heartbeat_band_follows_density() {
        let sampler = Sampler::new(
            CaptureConfig::default(),
            WgcSampler::new(),
            ExclusionList::shipped_defaults(),
            Arc::new(DropSink),
        );
        // Quiet minute → ceiling.
        assert_eq!(sampler.heartbeat_interval_ms(), 20_000);
        // Moderate activity → default.
        for i in 0..5 {
            sampler.note_activity(1_000_000 + i);
        }
        assert_eq!(sampler.heartbeat_interval_ms(), 10_000);
        // Storm → floor (never below 5 s, ADR-032).
        for i in 0..12 {
            sampler.note_activity(1_000_100 + i);
        }
        assert_eq!(sampler.heartbeat_interval_ms(), 5_000);
    }

    #[test]
    fn idle_gates_the_heartbeat() {
        let sampler = Sampler::new(
            CaptureConfig::default(),
            WgcSampler::new(),
            ExclusionList::shipped_defaults(),
            Arc::new(DropSink),
        );
        sampler.note_activity(1_000_000);
        assert!(sampler.user_is_active(1_030_000), "30 s after input: active");
        assert!(!sampler.user_is_active(1_070_000), "70 s idle: heartbeat suspends");
    }
}
