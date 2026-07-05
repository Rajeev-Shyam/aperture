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
//!    (doc 11 §7). v1 ships primary-monitor-only (Q42); multi-monitor lands at M8.

// Contract surface: BubbleRect/set_hit_test_rects are driven by the UI's bubble
// lifecycle (M3-UI wiring); create_overlays fans out at M8. Kept warning-free.
#![allow(dead_code)]

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

/// Create one overlay window per monitor at startup (doc 11 §2). v1: the config
/// window labeled `overlay` (primary monitor, Q42) already exists — `setup`
/// hardens it directly; M8 fans out per-monitor and wires DPI re-anchoring.
pub fn create_overlays(_app: &AppHandle) -> Result<Vec<WebviewWindow>, OverlayError> {
    // TODO(M8:) enumerate monitors (EnumDisplayMonitors), clone the overlay per
    //   monitor sized to that monitor's work area, and re-anchor on WM_DPICHANGED
    //   / display-change (doc 11 §7).
    todo!("M8: per-monitor overlays + DPI re-anchor (doc 11 §2,§7)")
}

/// Apply `WS_EX_LAYERED | WS_EX_TRANSPARENT` so the window is click-through
/// (doc 11 §2). With this set, all input falls through to the apps beneath;
/// hit-testing is selectively restored by [`set_hit_test_rects`].
pub fn make_click_through(window: &WebviewWindow) -> Result<(), OverlayError> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_LAYERED, WS_EX_TRANSPARENT,
        };
        let hwnd = window
            .hwnd()
            .map_err(|e| OverlayError::Win32(e.to_string()))?;
        let hwnd = HWND(hwnd.0);
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new_style = style | (WS_EX_LAYERED.0 as isize) | (WS_EX_TRANSPARENT.0 as isize);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Err(OverlayError::Win32("windows-only".into()))
    }
}

/// `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` — keep our overlay
/// out of all screen capture, including our own WGC sampler (doc 11 §2, doc 05 §2).
pub fn exclude_from_capture(window: &WebviewWindow) -> Result<(), OverlayError> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
        };
        let hwnd = window
            .hwnd()
            .map_err(|e| OverlayError::Win32(e.to_string()))?;
        // Some GPUs/drivers ignore the affinity (doc 11 §2) — surface the error;
        // the caller logs it and the truthful indicator remains the disclosure.
        SetWindowDisplayAffinity(HWND(hwnd.0), WDA_EXCLUDEFROMCAPTURE)
            .map_err(|e| OverlayError::Win32(e.to_string()))
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Err(OverlayError::Win32("windows-only".into()))
    }
}

/// Re-enable hit-testing ONLY over the given live bubble rects (doc 11 §2). With
/// an empty slice the overlay is fully click-through. The §7 watchdog asserts the
/// inverse: a hit-test region with no visible bubble resets to full click-through.
///
/// Window-level granularity: `WS_EX_TRANSPARENT` is toggled off while any bubble
/// is live (the WebView's own CSS `pointer-events` gates per-rect input — the UI
/// agent owns that half), and back on when none are.
pub fn set_hit_test_rects(
    window: &WebviewWindow,
    rects: &[BubbleRect],
) -> Result<(), OverlayError> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TRANSPARENT,
        };
        let hwnd = window
            .hwnd()
            .map_err(|e| OverlayError::Win32(e.to_string()))?;
        let hwnd = HWND(hwnd.0);
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new_style = if rects.is_empty() {
            style | (WS_EX_TRANSPARENT.0 as isize) // full click-through (watchdog reset)
        } else {
            style & !(WS_EX_TRANSPARENT.0 as isize) // bubbles live: WebView hit-tests
        };
        if new_style != style {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (window, rects);
        Err(OverlayError::Win32("windows-only".into()))
    }
}
