//! Bubble-granularity hit-testing for the click-through overlay (doc 11 §2).
//!
//! The overlay window is created `WS_EX_TRANSPARENT` (click-through). Clearing
//! that bit window-wide would make the whole monitor-sized WebView swallow every
//! click, so interactivity is driven at *rect* granularity instead:
//!
//! 1. The UI measures every interactive surface (`.surface-interactive`) and
//!    publishes the rects via the `set_hit_test_rects` command (physical px,
//!    window-relative).
//! 2. A lightweight poller compares the global cursor position against those
//!    rects (~30 Hz) and clears `WS_EX_TRANSPARENT` only while the cursor is
//!    inside one — hover and click reach the WebView, everything else falls
//!    through to the apps beneath.
//! 3. A modal surface (`set_overlay_interactive`) overrides the poller: while a
//!    modal is up the window stays interactive regardless of the cursor.
//!
//! The doc 11 §7 watchdog invariant holds structurally: with no rects and no
//! modal, the poller's next tick restores full click-through.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::overlay::{self, BubbleRect};

/// Hover padding around each published rect, physical px — absorbs measurement
/// jitter at bubble edges so the interactive flag doesn't flap mid-click.
const RECT_PAD: i32 = 8;

/// Poll cadence. 30 Hz is far below any perceptible hover latency and the tick
/// is two Win32 calls + a few rect compares when rects exist, nothing when idle.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(33);

/// What was last applied to the window, so the poller only touches styles on
/// transitions. Tracks the MECHANISM, not just the boolean: hover-interactive
/// (`set_transparent`, no focus) and modal-interactive (`set_interactive`,
/// takes focus) must not satisfy each other — a modal mounting while the
/// cursor already hovers a bubble still needs its focus call.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Applied {
    interactive: bool,
    modal: bool,
}

#[derive(Default)]
struct WindowHit {
    rects: Vec<BubbleRect>,
    /// COUNT of live modal surfaces (first-run consent, privacy panel, context
    /// preview can stack) — interactive regardless of the cursor while > 0.
    /// A count, not a bool: closing one stacked modal must not strip the other.
    modal: u32,
    applied: Option<Applied>,
}

/// Shared hit-test state, managed by Tauri; keyed by overlay window label so
/// per-monitor overlays (M8 fan-out) each get their own rect set.
#[derive(Default)]
pub struct HitTestState {
    windows: Mutex<HashMap<String, WindowHit>>,
}

impl HitTestState {
    /// Store the UI-published rects for `label` and reconcile immediately (an
    /// emptied set must restore click-through without waiting a tick).
    pub fn set_rects(&self, app: &tauri::AppHandle, label: &str, rects: Vec<BubbleRect>) {
        {
            let mut windows = lock(&self.windows);
            windows.entry(label.to_string()).or_default().rects = rects;
        }
        self.reconcile(app);
    }

    /// Adjust the modal override count for `label` and reconcile immediately —
    /// a modal must be clickable the moment it mounts, not a poll tick later.
    pub fn set_modal(&self, app: &tauri::AppHandle, label: &str, modal: bool) {
        {
            let mut windows = lock(&self.windows);
            let hit = windows.entry(label.to_string()).or_default();
            hit.modal = if modal {
                hit.modal.saturating_add(1)
            } else {
                hit.modal.saturating_sub(1)
            };
        }
        self.reconcile(app);
    }

    /// Reset `label`'s state to click-through. Called by the UI root on mount:
    /// a WebView reload/crash while a modal was open would otherwise orphan the
    /// modal count at > 0 and leave the whole monitor swallowing clicks forever
    /// (multi-agent review, 2026-08-13).
    pub fn reset(&self, app: &tauri::AppHandle, label: &str) {
        {
            let mut windows = lock(&self.windows);
            let hit = windows.entry(label.to_string()).or_default();
            hit.modal = 0;
            hit.rects.clear();
        }
        self.reconcile(app);
    }

    /// One poller tick: for every tracked window, decide `interactive` from
    /// (modal || cursor-in-rect) and apply it only on change.
    pub fn reconcile(&self, app: &tauri::AppHandle) {
        use tauri::Manager;
        let cursor = cursor_pos();
        let mut windows = lock(&self.windows);
        for (label, hit) in windows.iter_mut() {
            let Some(window) = app.get_webview_window(label) else { continue };
            let want_modal = hit.modal > 0;
            let interactive = want_modal
                || match (cursor, hit.rects.is_empty()) {
                    (Some((cx, cy)), false) => {
                        // Window-relative physical px, same space the UI published.
                        window.outer_position().is_ok_and(|pos| {
                            let (rx, ry) = (cx - pos.x, cy - pos.y);
                            hit.rects.iter().any(|r| {
                                rx >= r.x - RECT_PAD
                                    && rx <= r.x + r.width + RECT_PAD
                                    && ry >= r.y - RECT_PAD
                                    && ry <= r.y + r.height + RECT_PAD
                            })
                        })
                    }
                    _ => false,
                };
            let target = Applied { interactive, modal: want_modal && interactive };
            if hit.applied == Some(target) {
                continue;
            }
            // Modal surfaces also need focus (the window is created focus:false);
            // hover interactivity must NOT steal focus from the user's work.
            let result = if target.modal {
                overlay::set_interactive(&window, true)
            } else {
                overlay::set_transparent(&window, !target.interactive)
            };
            match result {
                Ok(()) => hit.applied = Some(target),
                Err(e) => tracing::error!(%e, label, "hit-test style flip failed"),
            }
        }
    }
}

/// Spawn the cursor poller. Idles cheaply: with no rects and no modal anywhere it
/// does two map lookups and goes back to sleep.
pub fn spawn_poller(app: tauri::AppHandle) {
    tokio::spawn(async move {
        use tauri::Manager;
        let mut tick = tokio::time::interval(POLL_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let state = app.state::<HitTestState>();
            state.reconcile(&app);
        }
    });
}

/// Global cursor position in screen physical px, or `None` off-Windows/on error.
fn cursor_pos() -> Option<(i32, i32)> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut p = POINT::default();
        GetCursorPos(&mut p).ok().map(|()| (p.x, p.y))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Mutex lock that shrugs off poisoning — the state is plain data; a panicked
/// writer leaves nothing inconsistent worth refusing over.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}
