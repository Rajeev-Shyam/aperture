//! [`UiaExecutor`] — the real Win32/UIA backend (Doc 22 §3.1, V2-M0).
//!
//! Every `execute` runs the same guard ladder before touching the screen:
//!
//! 1. hard stop observed ⇒ [`ActionError::Stopped`] (locked decision 5);
//! 2. foreground window on the exclusion list ⇒ [`ActionError::Excluded`]
//!    (Doc 24 #49 — the loop pauses and notifies; the executor never acts on
//!    an excluded window);
//! 3. foreground window's process is UAC-elevated while ours is not ⇒
//!    [`ActionError::Elevated`] (Doc 24 #50 — the run-as-admin prompt is the
//!    loop's job; the executor only refuses);
//! 4. dispatch by [`ActionType`].
//!
//! `Launch` is a simulated Start-menu search — Win key, type the name, Enter —
//! never a process spawn (Doc 24 #52: screen and keyboard only, forever).
//! The windows that appeared during the launch are reported in
//! [`ActionOutcome::new_windows`] so the loop can undo it (Doc 24 #54).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aperture_contracts::agent::{ActionError, ActionType, AgentAction, ScrollDirection};

use crate::grounding::best_index;
use crate::keys::{modifier_virtual_key, parse_chord, virtual_key, KeyName, Modifier};
use crate::platform;
use crate::{ActionExecutor, ActionOutcome, ExclusionProbe, ExecutorTicket, NoExclusions, WindowInfo};

/// Longest `wait` the executor will honour in one step (Doc 22 §5 `amount`
/// is in seconds for `wait`); longer waits are the loop's business.
pub const MAX_WAIT: Duration = Duration::from_secs(5);
/// `switch_window`: attempts × gap to win the foreground from a starting app.
const SWITCH_ATTEMPTS: usize = 5;
const SWITCH_RETRY_GAP: Duration = Duration::from_millis(300);
/// Start-menu settle after the Win tap / before Enter (decision #52 launch).
const LAUNCH_SEARCH_SETTLE: Duration = Duration::from_millis(400);
const LAUNCH_TYPE_SETTLE: Duration = Duration::from_millis(300);
/// How long to wait for the launched app's window before diffing.
const LAUNCH_WINDOW_SETTLE: Duration = Duration::from_millis(1500);

/// The real Win32/UIA backend. One instance per task loop; `Send + Sync`
/// because every OS call is made on the *calling* thread (UIA is initialised
/// per thread, see [`crate::platform`]).
pub struct UiaExecutor {
    probe: Arc<dyn ExclusionProbe>,
    stop: Arc<AtomicBool>,
}

impl UiaExecutor {
    /// `probe` answers "is this window excluded?" (src-tauri implements it over
    /// capture's `ExclusionList`); `stop` is the hard-stop flag the loop raises.
    pub fn new(probe: Arc<dyn ExclusionProbe>, stop: Arc<AtomicBool>) -> Self {
        Self { probe, stop }
    }

    /// The shared hard-stop flag (raise it to make every later call `Stopped`).
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    fn check_stop(&self) -> Result<(), ActionError> {
        if self.stop.load(Ordering::SeqCst) {
            Err(ActionError::Stopped)
        } else {
            Ok(())
        }
    }

    /// Steps 1–3 of the guard ladder; returns the foreground window the action
    /// may act on (`None` when nothing is foreground, e.g. the bare desktop).
    fn guard(&self) -> Result<Option<WindowInfo>, ActionError> {
        self.check_stop()?;
        let Some(fg) = platform::foreground_window() else {
            return Ok(None);
        };
        if let Some(label) = self.probe.excluded_label(&fg) {
            return Err(ActionError::Excluded(label));
        }
        if platform::window_is_elevated_above_us(fg.hwnd) {
            return Err(ActionError::Elevated(fg.title.clone()));
        }
        Ok(Some(fg))
    }

    fn outcome(description: String) -> ActionOutcome {
        ActionOutcome {
            description,
            focused_element: platform::focused_element_name(),
            new_windows: Vec::new(),
        }
    }

    fn click(&self, fg: Option<&WindowInfo>, action: &AgentAction) -> Result<ActionOutcome, ActionError> {
        let target = action.target.as_deref().map(str::trim).filter(|t| !t.is_empty());
        // Primary grounding (Doc 22 §6): UIA label match inside the foreground
        // window's subtree. Only a *miss* falls through to the coords fallback;
        // any other failure (disabled, blocked input) is reported as-is.
        let miss = match (fg, target) {
            (Some(fg), Some(target)) => match platform::click_element(fg.hwnd, target) {
                Ok(report) => {
                    let desc = match report.point {
                        Some((x, y)) => format!(
                            "clicked '{}' ({}) at ({x},{y})",
                            report.name,
                            report.quality.label()
                        ),
                        None => format!("invoked '{}' ({})", report.name, report.quality.label()),
                    };
                    return Ok(Self::outcome(desc));
                }
                Err(ActionError::ElementNotFound(_)) => ActionError::ElementNotFound(target.to_string()),
                Err(other) => return Err(other),
            },
            (None, Some(target)) => ActionError::ElementNotFound(target.to_string()),
            (_, None) => ActionError::Unsupported("click needs a target label or coords".into()),
        };
        // Fallback (Doc 22 §6): Claude's pixel coordinates, used verbatim as
        // **primary-monitor physical pixels** — nothing scales them (doc 22 §6
        // agrees; a stale comment claiming a 768-px rescale was fixed 2026-09-05).
        if let Some(c) = action.coords {
            platform::click_at(c.x, c.y)?;
            let why = target.map(|t| format!(" (no UIA match for '{t}')")).unwrap_or_default();
            return Ok(Self::outcome(format!("clicked coords ({},{}){why}", c.x, c.y)));
        }
        Err(miss)
    }

    fn type_text(&self, action: &AgentAction) -> Result<ActionOutcome, ActionError> {
        let value = action
            .value
            .as_deref()
            .ok_or_else(|| ActionError::Unsupported("type needs a value".into()))?;
        platform::type_text(value, &self.stop)?;
        Ok(Self::outcome(format!("typed {} chars", value.chars().count())))
    }

    fn key(&self, action: &AgentAction) -> Result<ActionOutcome, ActionError> {
        let value = action
            .value
            .as_deref()
            .ok_or_else(|| ActionError::Unsupported("key needs a value".into()))?;
        let chord = parse_chord(value)?;
        platform::send_chord(&chord)?;
        Ok(Self::outcome(format!("pressed {}", value.trim())))
    }

    /// Decision #52: Win key → type the app name → Enter, all via SendInput.
    fn launch(&self, action: &AgentAction) -> Result<ActionOutcome, ActionError> {
        let name = action
            .target
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| ActionError::Unsupported("launch needs a target name".into()))?;
        let before = platform::list_open_windows();
        platform::tap_key(modifier_virtual_key(Modifier::Win))?;
        std::thread::sleep(LAUNCH_SEARCH_SETTLE);
        self.check_stop()?;
        platform::type_text(name, &self.stop)?;
        std::thread::sleep(LAUNCH_TYPE_SETTLE);
        self.check_stop()?;
        platform::tap_key(virtual_key(KeyName::Enter))?;
        std::thread::sleep(LAUNCH_WINDOW_SETTLE);
        let new_windows: Vec<WindowInfo> = platform::list_open_windows()
            .into_iter()
            .filter(|w| !before.iter().any(|b| b.hwnd == w.hwnd))
            .collect();
        Ok(ActionOutcome {
            description: format!(
                "launched '{name}' via Start search; {} new window(s)",
                new_windows.len()
            ),
            focused_element: platform::focused_element_name(),
            new_windows,
        })
    }

    fn switch_window(&self, action: &AgentAction) -> Result<ActionOutcome, ActionError> {
        let target = action
            .target
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| ActionError::Unsupported("switch_window needs a target".into()))?;
        let windows = platform::list_open_windows();
        // Candidates: the title and the process stem ("notepad" for notepad.exe),
        // so "Notepad" finds "Untitled - Notepad" and "chrome" finds Chrome.
        let labels: Vec<(usize, String)> = windows
            .iter()
            .enumerate()
            .flat_map(|(i, w)| {
                let stem = w
                    .process
                    .as_deref()
                    .map(|p| p.strip_suffix(".exe").unwrap_or(p).to_string());
                std::iter::once((i, w.title.clone())).chain(stem.map(|s| (i, s)))
            })
            .collect();
        let (li, quality) = best_index(labels.iter().map(|(_, l)| l.as_str()), target)
            .ok_or_else(|| ActionError::ElementNotFound(target.to_string()))?;
        let win = &windows[labels[li].0];
        // A window that exists but whose app is still starting refuses
        // SetForegroundWindow for a moment (V2-M0 gate, 2026-08-22: a Notepad
        // found 0.8 s after spawn). Retry briefly before calling it a failure.
        let mut brought = false;
        for attempt in 0..SWITCH_ATTEMPTS {
            self.check_stop()?;
            if platform::bring_to_foreground(win.hwnd) {
                brought = true;
                break;
            }
            if attempt + 1 < SWITCH_ATTEMPTS {
                std::thread::sleep(SWITCH_RETRY_GAP);
            }
        }
        if !brought {
            return Err(ActionError::NotInteractable(format!(
                "could not bring '{}' to the foreground",
                win.title
            )));
        }
        // The window is in front; its app may not be ready for input yet.
        platform::settle_after_switch(win.hwnd);
        Ok(Self::outcome(format!("switched to '{}' ({})", win.title, quality.label())))
    }

    fn scroll(&self, fg: Option<&WindowInfo>, action: &AgentAction) -> Result<ActionOutcome, ActionError> {
        let direction = action.direction.unwrap_or(ScrollDirection::Down);
        let notches = action.amount.unwrap_or(3).max(1);
        let anchor = match (fg, action.target.as_deref().map(str::trim).filter(|t| !t.is_empty())) {
            (Some(fg), Some(target)) => platform::element_centre(fg.hwnd, target),
            _ => None,
        };
        platform::scroll(direction, notches, anchor.map(|(x, y, _)| (x, y)))?;
        let at = match anchor {
            Some((x, y, q)) => format!(" at ({x},{y}) [{}]", q.label()),
            None => " at cursor".to_string(),
        };
        Ok(Self::outcome(format!("scrolled {direction:?} {notches} notch(es){at}")))
    }

    /// Interruptible sleep: the hard stop ends a wait early with `Stopped`.
    fn wait(&self, action: &AgentAction) -> Result<ActionOutcome, ActionError> {
        let want = Duration::from_secs(u64::from(action.amount.unwrap_or(1))).min(MAX_WAIT);
        let started = std::time::Instant::now();
        while started.elapsed() < want {
            self.check_stop()?;
            std::thread::sleep(Duration::from_millis(50).min(want - started.elapsed()));
        }
        Ok(Self::outcome(format!("waited {:.1}s", want.as_secs_f32())))
    }
}

/// Decision #54 undo: close the given windows (the ones a task opened and
/// that are still open) by the SAME simulated-UI means the task used —
/// bring each to the foreground, press Alt+F4. Skips windows that are gone,
/// excluded, or elevated; returns how many close chords were sent. No
/// ticket: this is a user gesture on a finished task, bounded to windows the
/// loop recorded, and it spawns/kills nothing (Doc 24 #52).
pub fn close_windows(targets: &[WindowInfo], probe: &dyn ExclusionProbe) -> usize {
    let open = platform::list_open_windows();
    let mut closed = 0;
    for w in targets {
        let Some(live) = open.iter().find(|o| o.hwnd == w.hwnd) else { continue };
        if probe.excluded_label(live).is_some() || platform::window_is_elevated_above_us(live.hwnd) {
            continue;
        }
        if !platform::bring_to_foreground(live.hwnd) {
            continue;
        }
        std::thread::sleep(Duration::from_millis(150));
        let Ok(chord) = parse_chord("Alt+F4") else { continue };
        if platform::send_chord(&chord).is_ok() {
            closed += 1;
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    closed
}

impl Default for UiaExecutor {
    /// No exclusions, a fresh (unraised) stop flag — for spikes and gates.
    fn default() -> Self {
        Self::new(Arc::new(NoExclusions), Arc::new(AtomicBool::new(false)))
    }
}

impl ActionExecutor for UiaExecutor {
    fn execute(&self, ticket: &ExecutorTicket, action: &AgentAction) -> Result<ActionOutcome, ActionError> {
        let fg = self.guard()?;
        // Process + hwnd only — NEVER the window title (a password manager's
        // or mail client's title is screen content; doc 13 §4 strips exactly
        // these from excluded rows, so they must not land in the log either).
        // `target` stays: it is Claude's requested label, not the screen's.
        tracing::debug!(
            task_id = %ticket.task_id(),
            action = ?action.action_type,
            target = action.target.as_deref().unwrap_or(""),
            foreground_process = fg.as_ref().and_then(|w| w.process.as_deref()).unwrap_or(""),
            foreground_hwnd = fg.as_ref().map(|w| w.hwnd).unwrap_or(0),
            "executing"
        );
        match action.action_type {
            ActionType::Click => self.click(fg.as_ref(), action),
            ActionType::Type => self.type_text(action),
            ActionType::Key => self.key(action),
            ActionType::Launch => self.launch(action),
            ActionType::SwitchWindow => self.switch_window(action),
            ActionType::Scroll => self.scroll(fg.as_ref(), action),
            ActionType::Wait => self.wait(action),
            ActionType::None => Ok(ActionOutcome {
                description: "no-op".into(),
                focused_element: None,
                new_windows: Vec::new(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(action_type: ActionType, target: Option<&str>, value: Option<&str>) -> AgentAction {
        AgentAction {
            action_type,
            target: target.map(str::to_string),
            value: value.map(str::to_string),
            direction: None,
            amount: None,
            coords: None,
        }
    }

    fn ticket() -> ExecutorTicket {
        ExecutorTicket::for_task(uuid::Uuid::new_v4())
    }

    /// Locked decision 5: the stop flag is checked before anything else, so a
    /// raised flag yields `Stopped` without touching the desktop.
    #[test]
    fn raised_stop_flag_refuses_every_action_before_acting() {
        let exec = UiaExecutor::default();
        exec.stop_flag().store(true, Ordering::SeqCst);
        for ty in [
            ActionType::Click,
            ActionType::Type,
            ActionType::Key,
            ActionType::Launch,
            ActionType::SwitchWindow,
            ActionType::Scroll,
            ActionType::Wait,
            ActionType::None,
        ] {
            assert_eq!(
                exec.execute(&ticket(), &act(ty, Some("x"), Some("y"))),
                Err(ActionError::Stopped),
                "{ty:?}"
            );
        }
    }

    /// Decision #49: an exclusion hit is reported before any dispatch. The
    /// probe here excludes *everything*, so whatever is foreground on the test
    /// machine trips it (or, with no foreground window at all, the no-op passes).
    #[test]
    fn excluded_foreground_window_pauses_rather_than_acting() {
        struct All;
        impl ExclusionProbe for All {
            fn excluded_label(&self, _: &WindowInfo) -> Option<String> {
                Some("everything".into())
            }
        }
        let exec = UiaExecutor::new(Arc::new(All), Arc::new(AtomicBool::new(false)));
        let r = exec.execute(&ticket(), &act(ActionType::None, None, None));
        if platform::foreground_window().is_some() {
            assert_eq!(r, Err(ActionError::Excluded("everything".into())));
        } else {
            assert!(r.is_ok());
        }
    }

    /// `None` is a pure no-op; `Wait` is bounded by `MAX_WAIT` and interruptible.
    /// Neither touches the screen, so they are safe in a unit test. (Skipped
    /// when the desktop's foreground window is elevated — the guard is right to
    /// refuse then, and that is covered by the on-target gate.)
    #[test]
    fn none_and_wait_are_safe_no_ops() {
        let exec = UiaExecutor::default();
        let r = exec.execute(&ticket(), &act(ActionType::None, None, None));
        if matches!(r, Err(ActionError::Elevated(_))) {
            return;
        }
        assert_eq!(r.as_ref().map(|o| o.description.as_str()), Ok("no-op"));

        let mut wait = act(ActionType::Wait, None, None);
        wait.amount = Some(0);
        let started = std::time::Instant::now();
        let r = exec.execute(&ticket(), &wait).expect("wait 0");
        assert!(r.description.starts_with("waited"));
        assert!(started.elapsed() < Duration::from_secs(1));

        // A raised flag ends a long wait early.
        wait.amount = Some(60);
        let flag = exec.stop_flag();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            flag.store(true, Ordering::SeqCst);
        });
        let started = std::time::Instant::now();
        assert_eq!(exec.execute(&ticket(), &wait), Err(ActionError::Stopped));
        assert!(started.elapsed() < MAX_WAIT, "stop must interrupt the wait");
        t.join().unwrap();
    }

    /// Malformed actions are `Unsupported`, never silently ignored — checked
    /// on the parse path that runs before any input is sent.
    #[test]
    fn malformed_actions_are_unsupported() {
        let exec = UiaExecutor::default();
        for a in [
            act(ActionType::Type, None, None),
            act(ActionType::Key, None, None),
            act(ActionType::Key, None, Some("Hyper+Q")),
            act(ActionType::Launch, None, None),
            act(ActionType::SwitchWindow, Some("   "), None),
            act(ActionType::Click, None, None),
        ] {
            match exec.execute(&ticket(), &a) {
                Err(ActionError::Unsupported(_)) => {}
                Err(ActionError::Elevated(_)) | Err(ActionError::Excluded(_)) => {} // guard fired first
                other => panic!("{:?} → {other:?}", a.action_type),
            }
        }
    }
}
