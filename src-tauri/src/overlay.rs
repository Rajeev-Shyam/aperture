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
#[derive(Debug, Clone, Copy, serde::Deserialize)]
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

/// A monitor's physical bounds — the minimal descriptor `plan_overlays` needs,
/// mapped from `tauri::Monitor` in [`create_overlays`] so the placement math is
/// pure + testable without a running app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorInfo {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// The label + physical bounds for one monitor's overlay window (doc 11 §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayPlacement {
    pub label: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// The overlay window label for monitor-enumeration index `i` — the single
/// naming rule shared by [`plan_overlays`] (window creation) and the
/// cursor→window routing (decision #13), so they can never disagree.
pub fn overlay_label(i: usize) -> String {
    if i == 0 {
        OVERLAY_LABEL.to_string()
    } else {
        format!("{OVERLAY_LABEL}-{i}")
    }
}

/// Index of the monitor containing `pos` (physical px), or `None` when no
/// monitor does (a cursor mid-transition, a stale enumeration). Pure. Bounds
/// are half-open (`[x, x+w)`) so a shared edge belongs to exactly one monitor.
pub fn monitor_index_at(pos: (i32, i32), monitors: &[MonitorInfo]) -> Option<usize> {
    let (cx, cy) = pos;
    monitors.iter().position(|m| {
        cx >= m.x
            && cx < m.x + m.width as i32
            && cy >= m.y
            && cy < m.y + m.height as i32
    })
}

/// The overlay window under the cursor RIGHT NOW — where a summoned control
/// surface should open (decision #13: controls follow the user). Falls back to
/// the primary [`OVERLAY_LABEL`] whenever the cursor/monitor query fails or the
/// mapped window does not exist (e.g. the per-monitor fan-out failed at setup).
pub fn cursor_overlay_label(app: &AppHandle) -> String {
    use tauri::Manager;
    let mapped = crate::hit_test::cursor_pos().and_then(|cursor| {
        let monitors = app.available_monitors().ok()?;
        let infos: Vec<MonitorInfo> = monitors.iter().map(monitor_info).collect();
        monitor_index_at(cursor, &infos).map(overlay_label)
    });
    match mapped {
        Some(label) if app.get_webview_window(&label).is_some() => label,
        _ => OVERLAY_LABEL.to_string(),
    }
}

/// Which overlay window currently shows the Context-Preview panel, if any —
/// the routing target for core-staged preview requests (decision #13): a new
/// MCP request must QUEUE behind the panel the user may be mid-edit in, never
/// open a second panel on another monitor. Set/cleared by the UI via the
/// `set_preview_host` command as its panel opens/closes; claiming broadcasts
/// `preview_claimed` so every other window folds its copy (exactly one
/// instance across monitors — the 08-15 dismissal-convergence pattern).
#[derive(Default)]
pub struct PreviewHost(std::sync::Mutex<Option<String>>);

impl PreviewHost {
    /// `label`'s panel is now open — it becomes the routing target.
    pub fn claim(&self, label: &str) {
        *self.lock() = Some(label.to_string());
    }

    /// `label`'s panel closed. Only the current host may clear the slot: a
    /// displaced window's teardown must not un-claim the window that took over.
    pub fn release(&self, label: &str) {
        let mut host = self.lock();
        if host.as_deref() == Some(label) {
            *host = None;
        }
    }

    pub fn get(&self) -> Option<String> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Map a `tauri::Monitor` to the pure [`MonitorInfo`] descriptor.
fn monitor_info(m: &tauri::Monitor) -> MonitorInfo {
    let p = m.position();
    let s = m.size();
    MonitorInfo { x: p.x, y: p.y, width: s.width, height: s.height }
}

/// Plan one overlay per monitor (doc 11 §2), **pure**: the primary (index 0)
/// reuses [`OVERLAY_LABEL`] (the config window, Q42); each subsequent monitor gets
/// `overlay-1`, `overlay-2`, … Every overlay exactly covers its monitor in
/// physical pixels. Empty input ⇒ no overlays.
pub fn plan_overlays(monitors: &[MonitorInfo]) -> Vec<OverlayPlacement> {
    monitors
        .iter()
        .enumerate()
        .map(|(i, m)| OverlayPlacement {
            label: overlay_label(i),
            x: m.x,
            y: m.y,
            width: m.width,
            height: m.height,
        })
        .collect()
}

/// Create one overlay window per monitor at startup (doc 11 §2, §7). The config
/// window labeled `overlay` (primary, Q42) is repositioned to the primary monitor;
/// each additional monitor gets a cloned transparent, click-through,
/// capture-excluded overlay. Every window is hardened via [`harden`].
///
/// **UNVERIFIED (on-hardware):** the Tauri window creation + physical placement is
/// compile-checked but not exercised on a multi-monitor rig. DPI re-anchoring on
/// `WM_DPICHANGED` / display-change (doc 11 §7) is the remaining on-hardware wiring.
pub fn create_overlays(app: &AppHandle) -> Result<Vec<WebviewWindow>, OverlayError> {
    use tauri::Manager;

    let monitors = app
        .available_monitors()
        .map_err(|e| OverlayError::MonitorEnum(e.to_string()))?;
    let infos: Vec<MonitorInfo> = monitors.iter().map(monitor_info).collect();

    let mut windows = Vec::with_capacity(infos.len());
    for placement in plan_overlays(&infos) {
        let window = match app.get_webview_window(&placement.label) {
            // The primary config window already exists — re-anchor it.
            Some(existing) => existing,
            // Secondary monitors: clone the overlay from the same WebView entry.
            None => tauri::WebviewWindowBuilder::new(
                app,
                &placement.label,
                tauri::WebviewUrl::App("index.html".into()),
            )
            .transparent(true)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .build()
            .map_err(|e| OverlayError::WindowCreate(e.to_string()))?,
        };
        // Cover the monitor exactly, in physical pixels.
        let _ = window.set_position(tauri::PhysicalPosition::new(placement.x, placement.y));
        let _ = window.set_size(tauri::PhysicalSize::new(placement.width, placement.height));
        harden(&window);
        windows.push(window);
    }
    Ok(windows)
}

/// Apply both overlay-invariant hardening steps to one window (doc 11 §2, doc 05
/// §2), logging (not failing) if a driver ignores capture-exclusion — the truthful
/// indicator remains the disclosure.
pub fn harden(window: &WebviewWindow) {
    if let Err(e) = make_click_through(window) {
        tracing::error!(%e, "overlay click-through failed (doc 11 §2)");
    }
    if let Err(e) = exclude_from_capture(window) {
        tracing::error!(%e, "overlay capture-exclusion failed (doc 05 §2)");
    }
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

/// Flip only the `WS_EX_TRANSPARENT` bit — the raw click-through switch, with no
/// focus side effect. The cursor poller (`hit_test`) drives this at bubble-hover
/// granularity; [`set_interactive`] layers focus on top for modal surfaces.
pub fn set_transparent(window: &WebviewWindow, transparent: bool) -> Result<(), OverlayError> {
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
        let new_style = if transparent {
            style | (WS_EX_TRANSPARENT.0 as isize)
        } else {
            style & !(WS_EX_TRANSPARENT.0 as isize)
        };
        if new_style != style {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (window, transparent);
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
    set_transparent(window, rects.is_empty())
}

/// Make the overlay window accept input (or go back to click-through).
///
/// The overlay is created click-through (`WS_EX_TRANSPARENT`, [`harden`]) and
/// `"focus": false` — correct for passive bubbles, and fatal for a *modal*.
/// M9 puts two real dialogs on this surface (first-run consent and the Activity
/// & Privacy view); without clearing the bit, every click falls through to the
/// app underneath and the user can never press "Turn on capture" — the flow just
/// silently does nothing, forever.
///
/// `interactive == true` also focuses the window, since a window created with
/// `focus: false` and `skipTaskbar: true` cannot otherwise be reached by
/// keyboard either. Bubbles keep using [`set_hit_test_rects`]; this is the
/// coarser "a modal owns the screen" switch.
pub fn set_interactive(window: &WebviewWindow, interactive: bool) -> Result<(), OverlayError> {
    #[cfg(windows)]
    {
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
            let new_style = if interactive {
                style & !(WS_EX_TRANSPARENT.0 as isize)
            } else {
                style | (WS_EX_TRANSPARENT.0 as isize)
            };
            if new_style != style {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style);
            }
        }
        if interactive {
            let _ = window.set_focus();
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (window, interactive);
        Err(OverlayError::Win32("windows-only".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(x: i32, y: i32, w: u32, h: u32) -> MonitorInfo {
        MonitorInfo { x, y, width: w, height: h }
    }

    #[test]
    fn primary_reuses_the_config_label_others_get_suffixes() {
        let plan = plan_overlays(&[
            mon(0, 0, 2560, 1440),
            mon(2560, 0, 1920, 1080),
            mon(-1920, 0, 1920, 1080),
        ]);
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[0].label, OVERLAY_LABEL, "primary keeps the config window label");
        assert_eq!(plan[1].label, "overlay-1");
        assert_eq!(plan[2].label, "overlay-2");
    }

    #[test]
    fn each_overlay_covers_its_monitor_exactly_incl_negative_origins() {
        let plan = plan_overlays(&[mon(0, 0, 2560, 1440), mon(-1920, 200, 1920, 1080)]);
        assert_eq!((plan[0].x, plan[0].y, plan[0].width, plan[0].height), (0, 0, 2560, 1440));
        // A monitor left-of / above the primary has negative physical origins.
        assert_eq!((plan[1].x, plan[1].y, plan[1].width, plan[1].height), (-1920, 200, 1920, 1080));
    }

    #[test]
    fn no_monitors_means_no_overlays() {
        assert!(plan_overlays(&[]).is_empty());
    }

    // --- decision #13: cursor -> monitor -> overlay-window routing ---------

    #[test]
    fn cursor_maps_to_its_containing_monitor_incl_negative_origins() {
        let monitors = [
            mon(0, 0, 2560, 1440),
            mon(2560, 0, 1920, 1080),
            mon(-1920, 200, 1920, 1080),
        ];
        assert_eq!(monitor_index_at((100, 100), &monitors), Some(0));
        assert_eq!(monitor_index_at((3000, 500), &monitors), Some(1));
        assert_eq!(monitor_index_at((-500, 300), &monitors), Some(2));
    }

    #[test]
    fn monitor_bounds_are_half_open_so_shared_edges_pick_one_monitor() {
        let monitors = [mon(0, 0, 2560, 1440), mon(2560, 0, 1920, 1080)];
        // x = 2560 is the first pixel OF the second monitor, not the last of
        // the first — a cursor on the seam maps to exactly one window.
        assert_eq!(monitor_index_at((2559, 0), &monitors), Some(0));
        assert_eq!(monitor_index_at((2560, 0), &monitors), Some(1));
        // Bottom/right edges are exclusive.
        assert_eq!(monitor_index_at((2560, 1080), &monitors), None);
    }

    #[test]
    fn a_cursor_on_no_monitor_maps_to_none() {
        assert_eq!(monitor_index_at((5000, 5000), &[mon(0, 0, 2560, 1440)]), None);
        assert_eq!(monitor_index_at((0, 0), &[]), None);
    }

    #[test]
    fn routing_labels_agree_with_the_planned_window_labels() {
        let monitors = [mon(0, 0, 2560, 1440), mon(2560, 0, 1920, 1080)];
        let plan = plan_overlays(&monitors);
        for (i, placement) in plan.iter().enumerate() {
            assert_eq!(overlay_label(i), placement.label);
        }
        let idx = monitor_index_at((2700, 40), &monitors).expect("second monitor");
        assert_eq!(overlay_label(idx), "overlay-1");
    }

    // --- decision #13: single preview host across windows ------------------

    #[test]
    fn preview_host_claim_and_release() {
        let host = PreviewHost::default();
        assert_eq!(host.get(), None);
        host.claim("overlay");
        assert_eq!(host.get(), Some("overlay".to_string()));
        host.release("overlay");
        assert_eq!(host.get(), None);
    }

    #[test]
    fn a_displaced_windows_release_cannot_unclaim_the_new_host() {
        let host = PreviewHost::default();
        host.claim("overlay");
        // The user summons the panel on monitor 1; the primary's teardown
        // (its close effect) races in afterwards and must be a no-op.
        host.claim("overlay-1");
        host.release("overlay");
        assert_eq!(host.get(), Some("overlay-1".to_string()));
        host.release("overlay-1");
        assert_eq!(host.get(), None);
    }
}
