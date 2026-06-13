//! Overlay windowing (doc 11 §2): one transparent, always-on-top,
//! taskbar-skipped, **click-through** window per monitor.
//!
//! Three Win32 facts make this an overlay rather than a window:
//! 1. `WS_EX_LAYERED | WS_EX_TRANSPARENT` => click-through by default; hit-test
//!    is re-enabled ONLY over live bubble rects so the overlay never steals input
//!    from the user's work (doc 11 §2, §7 watchdog).
//! 2. `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` => our own bubbles
//!    never enter our own (or anyone's) capture (doc 11 §2, doc 05 §2).
//! 3. Per-monitor instances, re-anchored on `WM_DPICHANGED` / display-change
//!    (doc 11 §7). v1 ships primary-monitor-only; multi-monitor lands at M8.

use tauri::{AppHandle, WebviewWindow};

/// The overlay window label declared in `tauri.conf.json`. The per-monitor
/// clones derive their labels from this (e.g. `overlay`, `overlay-1`, ...).
pub const OVERLAY_LABEL: &str = "overlay";

/// A live, hit-testable bubble rectangle in physical (device) pixels, relative
/// to its monitor's overlay window. The set of these is the ONLY region where
/// the click-through window accepts input (doc 11 §2).
#[derive(Debug, Clone, Copy)]
pub struct BubbleRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("monitor enumeration failed: {0}")]
    MonitorEnum(String),
    #[error("failed to create overlay window: {0}")]
    WindowCreate(String),
    #[error("win32 call failed: {0}")]
    Win32(String),
}

/// Create one overlay window per monitor at startup (doc 11 §2). Called from
/// `main.rs` `setup`. In v1 this creates only the primary-monitor overlay; M8
/// fans out to every monitor and wires display-change re-anchoring.
pub fn create_overlays(_app: &AppHandle) -> Result<Vec<WebviewWindow>, OverlayError> {
    // TODO(M3:) primary-monitor overlay only (the config window labeled "overlay");
    //   for each, call make_click_through + exclude_from_capture.
    // TODO(M8:) enumerate monitors (EnumDisplayMonitors), clone the overlay per
    //   monitor sized to that monitor's work area, and re-anchor on WM_DPICHANGED
    //   / display-change (doc 11 §7).
    todo!("M3: create the primary overlay; M8: per-monitor + DPI re-anchor (doc 11 §2,§7)")
}

/// Apply `WS_EX_LAYERED | WS_EX_TRANSPARENT` so the window is click-through
/// (doc 11 §2). With this set, all input falls through to the apps beneath;
/// hit-testing is selectively restored by [`set_hit_test_rects`].
pub fn make_click_through(_window: &WebviewWindow) -> Result<(), OverlayError> {
    // TODO(M3:) hwnd = window.hwnd(); GetWindowLongPtrW(GWL_EXSTYLE) |=
    //   WS_EX_LAYERED | WS_EX_TRANSPARENT; SetWindowLongPtrW back. (windows crate,
    //   Win32_UI_WindowsAndMessaging.)
    todo!("M3: set WS_EX_LAYERED|WS_EX_TRANSPARENT for click-through (doc 11 §2)")
}

/// `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` — keep our overlay
/// out of all screen capture, including our own WGC sampler (doc 11 §2, doc 05 §2).
pub fn exclude_from_capture(_window: &WebviewWindow) -> Result<(), OverlayError> {
    // TODO(M3:) hwnd = window.hwnd(); SetWindowDisplayAffinity(hwnd,
    //   WDA_EXCLUDEFROMCAPTURE). Verify the affinity took (some GPUs ignore it on
    //   older drivers) and log if not (doc 11 §2).
    todo!("M3: SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE) (doc 11 §2, doc 05 §2)")
}

/// Re-enable hit-testing ONLY over the given live bubble rects (doc 11 §2). With
/// an empty slice the overlay is fully click-through. The §7 watchdog asserts the
/// inverse: a hit-test region with no visible bubble resets to full click-through.
pub fn set_hit_test_rects(
    _window: &WebviewWindow,
    _rects: &[BubbleRect],
) -> Result<(), OverlayError> {
    // TODO(M3:) when rects is empty, keep WS_EX_TRANSPARENT (full click-through);
    //   when non-empty, clear WS_EX_TRANSPARENT and let the WebView's own hit-test
    //   (CSS pointer-events on bubble rects) gate input. The UI agent owns the
    //   per-rect pointer-events; this fn flips the window-level transparency.
    // TODO(M3:) watchdog: if rects empty but transparency cleared, force-reset
    //   to full click-through (doc 11 §7 "click-through misconfiguration").
    todo!("M3: gate input to live bubble rects; full click-through when empty (doc 11 §2,§7)")
}
