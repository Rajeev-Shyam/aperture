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
//!    rects (60 Hz while any rect or modal exists, 30 Hz otherwise) and clears
//!    `WS_EX_TRANSPARENT` only while the cursor is inside one — hover and
//!    click reach the WebView, everything else falls through to the apps
//!    beneath.
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
/// Kept at 8 (decision #11): nothing in the history argues for a change, and
/// the real-mouse feel pass below is what would.
const RECT_PAD: i32 = 8;

/// Poll cadence — 60 Hz (decision #11, was 30). The tick is two Win32 calls +
/// a few rect compares when rects exist and a map walk when idle, so doubling
/// the rate halves worst-case hover latency (33 → 16 ms) for negligible cost.
/// Rect and modal changes never wait for a tick: `set_rects`/`set_modal`
/// reconcile immediately on arrival.
///
/// PROFILING (decision #11 — owner feel pass on real hardware, post-install):
/// with a physical mouse, evaluate (a) fast flicks across a bubble edge — does
/// the window turn interactive before the press lands, or does the click fall
/// through; (b) whether the 8 px pad leaves perceptible dead-click halos on
/// the apps beneath a bubble; (c) whether whole-window `WS_EX_TRANSPARENT`
/// flipping is too coarse when rects from several surfaces coexist. The
/// escalation path if window-level proves too coarse is per-pixel hit-testing
/// (a `WM_NCHITTEST` subclass returning HTTRANSPARENT outside the rects)
/// rather than more poll rate.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);

/// Idle cadence — 30 Hz, used whenever NO window has a published rect or a live
/// modal (doc 04 §8; SDLC review 2026-08-19, finding 6). Doc 04 budgets < 2 %
/// average CPU at idle for an always-on app, and a 60 Hz timer that wakes
/// forever against an empty rect set spends that budget on nothing: with zero
/// rects and no modal `reconcile` can only answer "click-through", so the
/// answer cannot change between ticks. Halving the idle rate loses nothing —
/// rect and modal arrivals reconcile immediately on their own (`set_rects`,
/// `set_modal`), and the NEXT tick after one lands runs at [`POLL_INTERVAL`]
/// again. Active hover feel (decision #11) is untouched.
pub const IDLE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(33);

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

    /// Is there anything for the poller to decide — ANY window with a published
    /// rect or a live modal? When not, the next tick's answer is fixed
    /// ("click-through"), so the poller backs off to [`IDLE_POLL_INTERVAL`]
    /// (review finding 6).
    pub fn has_work(&self) -> bool {
        lock(&self.windows)
            .values()
            .any(|hit| hit.modal > 0 || !hit.rects.is_empty())
    }

    /// One poller tick: for every tracked window, decide `interactive` from
    /// (modal || cursor-in-rect) and **re-assert** the window style every tick.
    ///
    /// Re-asserting (rather than applying only on a cached transition, as this
    /// did until 2026-09-06) is what makes the click-through invariant
    /// self-healing: the style bits are set with raw `SetWindowLongPtrW` and
    /// tao — which rewrites `GWL_EXSTYLE` wholesale from its own flag model on
    /// any window-flag change — does not know about them, so a cache-trusting
    /// poller could leave a whole monitor swallowing clicks until the next
    /// hover transition. The cost is one `GetWindowLongPtrW` per window per
    /// tick; the write only happens when the live style actually differs. The
    /// cache still gates the one side effect that must NOT repeat: a modal's
    /// focus grab (`set_interactive`) fires on its transition only.
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
                            cursor_in_rects(&hit.rects, cx - pos.x, cy - pos.y)
                        })
                    }
                    _ => false,
                };
            let target = Applied { interactive, modal: want_modal && interactive };
            let transition = hit.applied != Some(target);
            // Modal surfaces also need focus (the window is created focus:false);
            // hover interactivity must NOT steal focus from the user's work.
            let result = if target.modal && transition {
                overlay::set_interactive(&window, true)
            } else {
                overlay::set_transparent(&window, !target.interactive)
            };
            match result {
                Ok(()) => hit.applied = Some(target),
                // Log on the transition only — a hwnd that cannot be styled
                // would otherwise spam at 60 Hz.
                Err(e) if transition => {
                    tracing::error!(%e, label, "hit-test style flip failed")
                }
                Err(_) => {}
            }
        }
    }
}

/// Spawn the cursor poller. Idles cheaply: with no rects and no modal anywhere it
/// does one map walk and sleeps [`IDLE_POLL_INTERVAL`]; once anything is
/// published it ticks at [`POLL_INTERVAL`] (review finding 6, doc 04 §8). A
/// plain sleep-after-work loop rather than `interval`: the cadence is chosen per
/// tick, and a tick that overran simply starts the next sleep late — the
/// skip-missed-ticks semantics the interval had are moot.
pub fn spawn_poller(app: tauri::AppHandle) {
    tokio::spawn(async move {
        use tauri::Manager;
        loop {
            let cadence = {
                let state = app.state::<HitTestState>();
                state.reconcile(&app);
                if state.has_work() { POLL_INTERVAL } else { IDLE_POLL_INTERVAL }
            };
            tokio::time::sleep(cadence).await;
        }
    });
}

/// Is the window-relative cursor inside any published rect, padded by
/// [`RECT_PAD`]? Pure — the poller's containment decision, kept extractable so
/// the pad boundary is testable off-hardware.
fn cursor_in_rects(rects: &[BubbleRect], rx: i32, ry: i32) -> bool {
    rects.iter().any(|r| {
        rx >= r.x - RECT_PAD
            && rx <= r.x + r.width + RECT_PAD
            && ry >= r.y - RECT_PAD
            && ry <= r.y + r.height + RECT_PAD
    })
}

/// Global cursor position in screen physical px, or `None` off-Windows/on error.
/// Crate-visible: control-surface routing (`overlay::cursor_overlay_label`,
/// decision #13) reuses the same source of truth as the poller.
pub(crate) fn cursor_pos() -> Option<(i32, i32)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> BubbleRect {
        BubbleRect { x, y, width: w, height: h }
    }

    #[test]
    fn no_rects_never_hits() {
        assert!(!cursor_in_rects(&[], 0, 0));
    }

    #[test]
    fn inside_and_outside_are_distinguished() {
        let rects = [rect(100, 100, 50, 20)];
        assert!(cursor_in_rects(&rects, 125, 110), "center hits");
        assert!(!cursor_in_rects(&rects, 200, 110), "well right of the pad misses");
        assert!(!cursor_in_rects(&rects, 125, 200), "well below the pad misses");
    }

    #[test]
    fn the_8px_pad_is_inclusive_at_its_boundary() {
        let rects = [rect(100, 100, 50, 20)];
        // Exactly RECT_PAD outside every edge still hits (jitter absorption)…
        assert!(cursor_in_rects(&rects, 100 - RECT_PAD, 110));
        assert!(cursor_in_rects(&rects, 150 + RECT_PAD, 110));
        assert!(cursor_in_rects(&rects, 125, 100 - RECT_PAD));
        assert!(cursor_in_rects(&rects, 125, 120 + RECT_PAD));
        // …and one more pixel out does not.
        assert!(!cursor_in_rects(&rects, 100 - RECT_PAD - 1, 110));
        assert!(!cursor_in_rects(&rects, 150 + RECT_PAD + 1, 110));
        assert!(!cursor_in_rects(&rects, 125, 100 - RECT_PAD - 1));
        assert!(!cursor_in_rects(&rects, 125, 120 + RECT_PAD + 1));
    }

    #[test]
    fn any_rect_in_the_set_can_hit() {
        let rects = [rect(0, 0, 10, 10), rect(500, 500, 10, 10)];
        assert!(cursor_in_rects(&rects, 505, 505));
        assert!(cursor_in_rects(&rects, 5, 5));
        assert!(!cursor_in_rects(&rects, 250, 250));
    }

    /// Review finding 6: the poller backs off only when EVERY window is empty
    /// (no rects, no modal) — one live surface anywhere keeps the active rate.
    #[test]
    fn has_work_is_false_only_when_every_window_is_empty() {
        let state = HitTestState::default();
        assert!(!state.has_work(), "nothing tracked ⇒ idle");
        lock(&state.windows).insert("overlay".into(), WindowHit::default());
        lock(&state.windows).insert("overlay-2".into(), WindowHit::default());
        assert!(!state.has_work(), "tracked but empty ⇒ idle");

        lock(&state.windows).get_mut("overlay-2").unwrap().rects = vec![rect(0, 0, 10, 10)];
        assert!(state.has_work(), "a rect on any monitor ⇒ active");

        lock(&state.windows).get_mut("overlay-2").unwrap().rects.clear();
        lock(&state.windows).get_mut("overlay").unwrap().modal = 1;
        assert!(state.has_work(), "a modal on any monitor ⇒ active");

        lock(&state.windows).get_mut("overlay").unwrap().modal = 0;
        assert!(!state.has_work(), "all clear again ⇒ idle");
    }
}
