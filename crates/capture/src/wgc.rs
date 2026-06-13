//! Windows.Graphics.Capture (WGC) frame sampler (doc 05 §2).
//!
//! Design rules from the spec:
//! - One [`GraphicsCaptureItem`] **per monitor**; a `Direct3D11CaptureFramePool`
//!   with only **1–2 buffers**; frames pulled **on demand** — never a continuous
//!   stream (doc 05 §2). The heavy resource policy here is what keeps capture at
//!   < 2 % idle CPU and ~0 VRAM (doc 05 §1).
//! - **Self-exclusion:** the overlay window sets
//!   `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` so our own bubbles never
//!   appear in captured frames — no feedback loop into the model (doc 05 §2).
//! - Frames are **ephemeral**: downscale → OCR → drop. Raw frames are **never**
//!   written to disk (doc 05 §2, doc 13 §4). This is invariant (2) at the source.
//!
//! Known caveat (RK5): stock Windows 11 draws a yellow border on actively
//! captured items. Mitigation is per-monitor capture + the honest tray indicator;
//! border-suppression for non-UWP callers is unproven.
//! [VERIFY — `GraphicsCaptureSession::IsBorderRequired(false)` support, else fall
//! back to the Desktop Duplication API (doc 05 §2, doc 05 §7).]

// TODO(M1): the WGC sampler lands in the M1 capture milestone.

use crate::CaptureError;

/// A raw, **ephemeral** captured frame. Lives only long enough to be downscaled
/// and handed to OCR, then dropped (doc 05 §2). Deliberately holds no `Clone` and
/// no serialization derive: a frame must not be cheaply copied or persisted.
pub struct EphemeralFrame {
    /// Pixel width of the captured surface.
    pub width: u32,
    /// Pixel height of the captured surface.
    pub height: u32,
    // GPU-backed surface staged to a CPU-readable texture; BGRA8.
    // texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    // The bytes are intentionally NOT exposed as an owned Vec until the sampler
    // explicitly stages them (doc 05 §2). See `sampler::downscale_for_ocr`.
}

impl EphemeralFrame {
    /// Crop to the foreground window rect when cheap (doc 05 §4). Returns a new
    /// ephemeral frame; the source is dropped.
    pub fn crop_to(self, _rect: WindowRect) -> EphemeralFrame {
        // TODO(M1): blit the sub-rect into a smaller staging texture.
        todo!("M1: crop ephemeral frame to foreground window rect")
    }
}

/// A device-independent window rectangle (logical px). Sourced from
/// `GetWindowRect`/DWM extended frame bounds at sample time (doc 05 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Identifies a monitor; one [`MonitorCapture`] is acquired per monitor (doc 05 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MonitorId(pub isize);

/// One monitor's capture resources: a `GraphicsCaptureItem`, a
/// `Direct3D11CaptureFramePool` (1–2 buffers), and the session. Closing this
/// releases all D3D refs — step 2 of the OFF path (doc 05 §5).
pub struct MonitorCapture {
    pub monitor: MonitorId,
    // item: windows::Graphics::Capture::GraphicsCaptureItem,
    // frame_pool: windows::Graphics::Capture::Direct3D11CaptureFramePool,
    // session: windows::Graphics::Capture::GraphicsCaptureSession,
}

impl MonitorCapture {
    /// Pull **one** frame on demand from this monitor's pool (doc 05 §2). Blocks
    /// briefly for the next frame; returns `CaptureUnavailable` if the context is
    /// unprotected/denied (DRM, secure desktop — doc 05 §7).
    pub fn pull_frame(&self) -> Result<EphemeralFrame, CaptureError> {
        // TODO(M1): TryGetNextFrame on the pool; copy surface to a staging texture;
        //   handle device-lost → CaptureError::DeviceLost (doc 05 §7).
        todo!("M1: on-demand single-frame pull")
    }

    /// Close the session + frame pool and release D3D references (doc 05 §5 step 2).
    /// Idempotent; safe to call during the STOPPING transition.
    pub fn close(&mut self) {
        // TODO(M1): session.Close(); frame_pool.Close(); drop D3D device refs.
        todo!("M1: Close() WGC session + frame pool, release D3D refs")
    }
}

/// Owns the D3D11 device and the per-monitor capture items. The single place WGC
/// resources are acquired and released across the toggle (doc 05 §2, §5).
pub struct WgcSampler {
    // d3d_device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    // monitors: std::collections::HashMap<MonitorId, MonitorCapture>,
}

impl WgcSampler {
    /// Probe WGC support without acquiring resources (doc 05 §7: WGC unsupported
    /// ⇒ event-only mode).
    pub fn is_supported() -> bool {
        // TODO(M1): GraphicsCaptureSession::IsSupported() [VERIFY API path].
        todo!("M1: probe GraphicsCaptureSession::IsSupported")
    }

    /// Acquire a `GraphicsCaptureItem` + frame pool per monitor and start the
    /// sessions. STARTING transition (doc 05 §5).
    pub fn acquire(&mut self) -> Result<(), CaptureError> {
        // TODO(M1): create D3D11 device; enumerate monitors; per monitor
        //   GraphicsCaptureItem::CreateFromMonitor + Create FramePool (1–2 buffers).
        todo!("M1: acquire per-monitor WGC items + frame pools")
    }

    /// Pull one frame from the monitor that hosts the foreground window (doc 05 §4).
    pub fn pull_foreground(&self, _monitor: MonitorId) -> Result<EphemeralFrame, CaptureError> {
        // TODO(M1): dispatch to the right MonitorCapture::pull_frame.
        todo!("M1: pull a frame from the foreground monitor")
    }

    /// Release every monitor's WGC resources (doc 05 §5 step 2). Called by the
    /// toggle's OFF path; also the force-release used on an SLA breach (doc 05 §7).
    pub fn release_all(&mut self) {
        // TODO(M1): close each MonitorCapture; drop the D3D device.
        todo!("M1: release all WGC resources")
    }
}

/// Mark a window to be excluded from **our own** captures via
/// `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` (doc 05 §2). The
/// overlay/bubble window calls this so it never appears in captured frames.
///
/// `hwnd` is the raw window handle (`HWND` as `isize`) to keep the public surface
/// free of a `windows` type leak across the crate boundary.
pub fn exclude_window_from_capture(_hwnd: isize) -> Result<(), CaptureError> {
    // TODO(M1): SetWindowDisplayAffinity(HWND(hwnd), WDA_EXCLUDEFROMCAPTURE) [VERIFY].
    todo!("M1: self-exclude the overlay window from capture")
}
