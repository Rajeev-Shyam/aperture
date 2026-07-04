//! Windows.Graphics.Capture (WGC) frame sampler (doc 05 §2).
//!
//! Design rules from the spec:
//! - One [`MonitorCapture`] **per monitor** (v1: primary only, Q42); a
//!   `Direct3D11CaptureFramePool` with only **2 buffers**; frames pulled
//!   **on demand** — the session runs but unpulled buffers are simply
//!   overwritten in the tiny ring, so idle cost is ~0 CPU / ~0 VRAM (doc 05 §1-2).
//! - **Self-exclusion:** the overlay window sets
//!   `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` so our own bubbles never
//!   appear in captured frames — no feedback loop into the model (doc 05 §2).
//! - Frames are **ephemeral**: downscale → OCR → drop. Raw frames are **never**
//!   written to disk (doc 05 §2, doc 13 §4). This is invariant (2) at the source.
//!
//! Known caveat (RK5): stock Windows 11 draws a yellow border on actively
//! captured items. We attempt `IsBorderRequired(false)` and log the outcome
//! (support varies by build); mitigation stays per-monitor capture + the honest
//! tray indicator. [VERIFY at the M1 hardware gate.]

use crate::CaptureError;

/// A raw, **ephemeral** captured frame: a CPU-side BGRA8 buffer staged from the
/// GPU surface. Lives only long enough to be downscaled and handed to OCR, then
/// dropped (doc 05 §2). Deliberately holds no `Clone` and no serialization
/// derive: a frame must not be cheaply copied or persisted.
pub struct EphemeralFrame {
    /// Pixel width of the captured surface.
    pub width: u32,
    /// Pixel height of the captured surface.
    pub height: u32,
    /// Row-major BGRA8 pixels, `width * height * 4` bytes (pitch removed).
    bgra: Vec<u8>,
}

impl EphemeralFrame {
    /// Construct from staged bytes (crate-internal; frames enter only via WGC or
    /// test helpers).
    pub(crate) fn from_bgra(width: u32, height: u32, bgra: Vec<u8>) -> Self {
        debug_assert_eq!(bgra.len(), (width * height * 4) as usize);
        Self { width, height, bgra }
    }

    /// Borrow the BGRA8 bytes (OCR pre-processing + pHash). No owned copies —
    /// the frame is consumed by the pipeline and dropped (doc 05 §2).
    pub fn bgra(&self) -> &[u8] {
        &self.bgra
    }

    /// Crop to the foreground window rect when cheap (doc 05 §4). Returns a new
    /// ephemeral frame; the source is consumed. Rect is clamped to the frame; a
    /// degenerate intersection returns the original frame unchanged.
    pub fn crop_to(self, rect: WindowRect) -> EphemeralFrame {
        let x0 = rect.left.clamp(0, self.width as i32) as u32;
        let y0 = rect.top.clamp(0, self.height as i32) as u32;
        let x1 = rect.right.clamp(0, self.width as i32) as u32;
        let y1 = rect.bottom.clamp(0, self.height as i32) as u32;
        if x1 <= x0 || y1 <= y0 {
            return self; // degenerate: keep the full frame (doc 05 §4 "when cheap")
        }
        let (w, h) = (x1 - x0, y1 - y0);
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in y0..y1 {
            let row_start = ((y * self.width + x0) * 4) as usize;
            let row_end = row_start + (w * 4) as usize;
            out.extend_from_slice(&self.bgra[row_start..row_end]);
        }
        EphemeralFrame { width: w, height: h, bgra: out }
    }
}

/// A device-independent window rectangle (physical px). Sourced from
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

#[cfg(windows)]
mod imp {
    use super::*;

    use windows::core::Interface;
    use windows::Graphics::Capture::{
        Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
    };
    use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
    use windows::Graphics::DirectX::DirectXPixelFormat;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
        D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    };
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY};
    use windows::Win32::System::WinRT::Direct3D11::{
        CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
    };
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

    /// One monitor's capture resources: item + a 2-buffer free-threaded frame
    /// pool + the running session. Closing releases all D3D refs — step 2 of the
    /// OFF path (doc 05 §5).
    ///
    /// SAFETY (Send): windows-rs leaves COM wrappers `!Send` and defers thread
    /// correctness to the caller. Every WinRT object here is **free-threaded**:
    /// the frame pool is created via `CreateFreeThreaded` (agile by contract),
    /// GraphicsCaptureItem/Session are agile WinRT objects, and `ID3D11Device`
    /// is documented thread-safe. The one thread-affine member — the immediate
    /// `ID3D11DeviceContext` — is only ever touched behind the enclosing
    /// `Mutex<WgcSampler>` in [`super::super::sampler::Sampler`], which
    /// serializes all access. [VERIFY at the M1 hardware gate.]
    pub struct MonitorCapture {
        pub monitor: MonitorId,
        item: GraphicsCaptureItem,
        frame_pool: Direct3D11CaptureFramePool,
        session: GraphicsCaptureSession,
        d3d_device: ID3D11Device,
        d3d_context: ID3D11DeviceContext,
        closed: bool,
    }

    // SAFETY: see the struct-level Send note above (free-threaded WinRT objects;
    // context access serialized by the owning Mutex).
    unsafe impl Send for MonitorCapture {}

    impl MonitorCapture {
        /// Pull **one** frame on demand from this monitor's pool (doc 05 §2).
        /// Polls briefly for the next buffer (the session runs continuously; the
        /// ring holds the latest); returns `CaptureUnavailable` if none arrives
        /// (protected content / secure desktop — doc 05 §7).
        pub fn pull_frame(&self) -> Result<EphemeralFrame, CaptureError> {
            if self.closed {
                return Err(CaptureError::CaptureUnavailable("capture closed".into()));
            }
            // Drain to the LATEST available frame; poll up to ~250 ms for one.
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
            let mut latest = None;
            loop {
                match self.frame_pool.TryGetNextFrame() {
                    Ok(frame) => latest = Some(frame),
                    Err(_) => {
                        if latest.is_some() || std::time::Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(8));
                    }
                }
            }
            let frame = latest.ok_or_else(|| {
                CaptureError::CaptureUnavailable("no frame within 250 ms".into())
            })?;

            // Surface → ID3D11Texture2D via the DXGI interface access shim.
            let surface = frame
                .Surface()
                .map_err(|e| CaptureError::CaptureUnavailable(e.to_string()))?;
            let access: IDirect3DDxgiInterfaceAccess = surface
                .cast()
                .map_err(|e| CaptureError::CaptureUnavailable(e.to_string()))?;
            let texture: ID3D11Texture2D = unsafe { access.GetInterface() }
                .map_err(|e| CaptureError::CaptureUnavailable(e.to_string()))?;

            unsafe {
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                texture.GetDesc(&mut desc);

                // Staging copy: CPU-readable, no bind flags (doc 05 §2).
                let mut staging_desc = desc;
                staging_desc.Usage = D3D11_USAGE_STAGING;
                staging_desc.BindFlags = 0;
                staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                staging_desc.MiscFlags = 0;

                let mut staging: Option<ID3D11Texture2D> = None;
                self.d3d_device
                    .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                    .map_err(|e| CaptureError::CaptureUnavailable(e.to_string()))?;
                let staging = staging.expect("CreateTexture2D succeeded");

                self.d3d_context.CopyResource(&staging, &texture);

                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                self.d3d_context
                    .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    // Map failure ≈ device lost: the recreate-or-degrade signal (doc 05 §7).
                    .map_err(|_| CaptureError::DeviceLost)?;

                let (w, h) = (desc.Width, desc.Height);
                let mut bgra = vec![0u8; (w * h * 4) as usize];
                let src_base = mapped.pData as *const u8;
                for row in 0..h {
                    let src =
                        std::slice::from_raw_parts(src_base.add((row * mapped.RowPitch) as usize), (w * 4) as usize);
                    let dst_start = (row * w * 4) as usize;
                    bgra[dst_start..dst_start + (w * 4) as usize].copy_from_slice(src);
                }
                self.d3d_context.Unmap(&staging, 0);

                Ok(EphemeralFrame::from_bgra(w, h, bgra))
            }
        }

        /// Close the session + frame pool and release D3D references (doc 05 §5
        /// step 2). Idempotent; safe to call during the STOPPING transition.
        pub fn close(&mut self) {
            if self.closed {
                return;
            }
            let _ = self.session.Close();
            let _ = self.frame_pool.Close();
            self.closed = true;
        }
    }

    impl Drop for MonitorCapture {
        fn drop(&mut self) {
            self.close();
        }
    }

    /// Owns the D3D11 device and the per-monitor capture items. The single place
    /// WGC resources are acquired and released across the toggle (doc 05 §2, §5).
    #[derive(Default)]
    pub struct WgcSampler {
        state: Option<SamplerState>,
    }

    struct SamplerState {
        _winrt_device: IDirect3DDevice,
        primary: MonitorCapture,
    }

    // SAFETY: IDirect3DDevice wraps the free-threaded D3D11 device (see the
    // MonitorCapture Send note); held only for lifetime management.
    unsafe impl Send for SamplerState {}

    impl WgcSampler {
        pub fn new() -> Self {
            Self::default()
        }

        /// Probe WGC support without acquiring resources (doc 05 §7: WGC
        /// unsupported ⇒ event-only mode).
        pub fn is_supported() -> bool {
            GraphicsCaptureSession::IsSupported().unwrap_or(false)
        }

        /// Acquire a `GraphicsCaptureItem` + frame pool for the **primary**
        /// monitor (Q42: primary-only in v1; M8 fans out) and start the session.
        /// STARTING transition (doc 05 §5).
        pub fn acquire(&mut self) -> Result<(), CaptureError> {
            if self.state.is_some() {
                return Ok(()); // idempotent
            }
            unsafe {
                // 1. D3D11 device (BGRA for WGC surfaces).
                let mut device: Option<ID3D11Device> = None;
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_HARDWARE,
                    windows::Win32::Foundation::HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    None,
                )
                .map_err(|e| CaptureError::CaptureUnavailable(format!("D3D11CreateDevice: {e}")))?;
                let device = device.expect("device out param");
                let context = device
                    .GetImmediateContext()
                    .map_err(|e| CaptureError::CaptureUnavailable(e.to_string()))?;

                // 2. WinRT IDirect3DDevice from the DXGI device.
                let dxgi: IDXGIDevice = device
                    .cast()
                    .map_err(|e| CaptureError::CaptureUnavailable(e.to_string()))?;
                let inspectable = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)
                    .map_err(|e| CaptureError::CaptureUnavailable(e.to_string()))?;
                let winrt_device: IDirect3DDevice = inspectable
                    .cast()
                    .map_err(|e| CaptureError::CaptureUnavailable(e.to_string()))?;

                // 3. Capture item for the primary monitor (Q42).
                let hmon = MonitorFromPoint(
                    windows::Win32::Foundation::POINT { x: 0, y: 0 },
                    MONITOR_DEFAULTTOPRIMARY,
                );
                let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                    .map_err(|e| CaptureError::CaptureUnavailable(e.to_string()))?;
                let item: GraphicsCaptureItem = interop
                    .CreateForMonitor(hmon)
                    .map_err(|e| CaptureError::CaptureUnavailable(format!("CreateForMonitor: {e}")))?;

                // 4. 2-buffer free-threaded pool + session (doc 05 §2).
                let size = item
                    .Size()
                    .map_err(|e| CaptureError::CaptureUnavailable(e.to_string()))?;
                let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                    &winrt_device,
                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    2,
                    size,
                )
                .map_err(|e| CaptureError::CaptureUnavailable(e.to_string()))?;
                let session = frame_pool
                    .CreateCaptureSession(&item)
                    .map_err(|e| CaptureError::CaptureUnavailable(e.to_string()))?;

                // No cursor in frames (cleaner OCR input).
                let _ = session.SetIsCursorCaptureEnabled(false);
                // RK5: attempt yellow-border suppression; support varies. Log only —
                // the truthful tray indicator remains the disclosure surface (Q40).
                match session.SetIsBorderRequired(false) {
                    Ok(()) => tracing::info!("WGC border suppression: supported"),
                    Err(e) => tracing::info!(%e, "WGC border suppression unavailable (RK5)"),
                }

                session
                    .StartCapture()
                    .map_err(|e| CaptureError::CaptureUnavailable(format!("StartCapture: {e}")))?;

                self.state = Some(SamplerState {
                    _winrt_device: winrt_device,
                    primary: MonitorCapture {
                        monitor: MonitorId(hmon.0 as isize),
                        item,
                        frame_pool,
                        session,
                        d3d_device: device,
                        d3d_context: context,
                        closed: false,
                    },
                });
                Ok(())
            }
        }

        /// Pull one frame from the monitor hosting the foreground window
        /// (doc 05 §4). v1: always the primary monitor (Q42).
        pub fn pull_foreground(&self, _monitor: MonitorId) -> Result<EphemeralFrame, CaptureError> {
            match &self.state {
                Some(s) => s.primary.pull_frame(),
                None => Err(CaptureError::CaptureUnavailable("WGC not acquired".into())),
            }
        }

        /// The primary monitor id once acquired.
        pub fn primary_monitor(&self) -> Option<MonitorId> {
            self.state.as_ref().map(|s| s.primary.monitor)
        }

        /// Release every monitor's WGC resources (doc 05 §5 step 2). Called by
        /// the toggle's OFF path; also the force-release on an SLA breach
        /// (doc 05 §7). Idempotent.
        pub fn release_all(&mut self) {
            if let Some(mut s) = self.state.take() {
                s.primary.close();
            }
        }
    }

    /// Mark a window to be excluded from **our own** captures via
    /// `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` (doc 05 §2). The
    /// overlay/bubble window calls this so it never appears in captured frames.
    pub fn exclude_window_from_capture(hwnd: isize) -> Result<(), CaptureError> {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
        };
        unsafe {
            SetWindowDisplayAffinity(HWND(hwnd as *mut _), WDA_EXCLUDEFROMCAPTURE)
                .map_err(|e| CaptureError::CaptureUnavailable(format!("SetWindowDisplayAffinity: {e}")))
        }
    }
}

#[cfg(windows)]
pub use imp::{exclude_window_from_capture, MonitorCapture, WgcSampler};

// Non-Windows stubs (locked decision 1: Windows 11 only; these keep
// cross-platform type-checks alive).
#[cfg(not(windows))]
#[derive(Default)]
pub struct WgcSampler;
#[cfg(not(windows))]
impl WgcSampler {
    pub fn new() -> Self {
        Self
    }
    pub fn is_supported() -> bool {
        false
    }
    pub fn acquire(&mut self) -> Result<(), CaptureError> {
        Err(CaptureError::CaptureUnavailable("windows-only".into()))
    }
    pub fn pull_foreground(&self, _m: MonitorId) -> Result<EphemeralFrame, CaptureError> {
        Err(CaptureError::CaptureUnavailable("windows-only".into()))
    }
    pub fn primary_monitor(&self) -> Option<MonitorId> {
        None
    }
    pub fn release_all(&mut self) {}
}
#[cfg(not(windows))]
pub fn exclude_window_from_capture(_hwnd: isize) -> Result<(), CaptureError> {
    Err(CaptureError::CaptureUnavailable("windows-only".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_clamps_and_handles_degenerate_rects() {
        let frame = EphemeralFrame::from_bgra(4, 4, vec![7u8; 4 * 4 * 4]);
        let cropped = frame.crop_to(WindowRect { left: 1, top: 1, right: 3, bottom: 3 });
        assert_eq!((cropped.width, cropped.height), (2, 2));
        assert_eq!(cropped.bgra().len(), 2 * 2 * 4);

        let frame2 = EphemeralFrame::from_bgra(4, 4, vec![7u8; 64]);
        let same = frame2.crop_to(WindowRect { left: 5, top: 5, right: 2, bottom: 2 });
        assert_eq!((same.width, same.height), (4, 4), "degenerate rect keeps frame");
    }
}
