//! V2-M0 gate — the real `UiaExecutor` drives a live app (Doc 22 §10:
//! "click a known element in a test app reliably"; Q-V2-01 fuzzy-label
//! grounding answered on a real UIA tree).
//!
//! The script, against a freshly opened Notepad:
//!   switch_window "Notepad" → type "aperture v2m0" → read the text back via
//!   UIA → click "File" (menu grounding) → Esc → Ctrl+A → Backspace → Alt+F4
//!   (→ click "Don't save" if Notepad prompts) → the window is gone.
//!
//! **On-target only** (`#[ignore]`): opens and closes a real Notepad window
//! on the desktop and moves the mouse/keyboard. Needs an interactive session,
//! `notepad.exe` on PATH, and NO other Notepad window open (the cleanup guard
//! kills every `notepad.exe`, so exclusivity is a precondition, as with SC6).
//!
//! Spawning Notepad here is allowed: `gates` is lint-exempt and this never
//! runs on the proactive path. The executor under test spawns nothing — its
//! `launch` is a simulated Start-menu search (Doc 24 #52) — and that is
//! exactly why the app must be opened by the harness, not by the executor.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use aperture_action_executor::{
    list_open_windows, read_document_text, ActionExecutor, ExecutorTicket, UiaExecutor,
    WindowInfo,
};
use aperture_contracts::agent::{ActionError, ActionType, AgentAction};

const NOTEPAD_IMAGE: &str = "notepad.exe";
const TYPED: &str = "aperture v2m0";
/// Wait for the Notepad window to appear after spawn (Store Notepad is slow
/// on a cold start).
const WINDOW_APPEAR: Duration = Duration::from_secs(15);
/// Wait for UI state (text, window close) to settle after an action.
const SETTLE: Duration = Duration::from_secs(3);

/// Kills Notepad on every exit path. `child.kill()` covers the classic
/// notepad; the Store Notepad re-launches itself from `WindowsApps`, so the
/// image-name kill is what actually closes it.
struct NotepadGuard {
    child: Child,
}

impl Drop for NotepadGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = Command::new("taskkill").args(["/F", "/IM", NOTEPAD_IMAGE]).output();
    }
}

fn notepad_windows() -> Vec<WindowInfo> {
    list_open_windows()
        .into_iter()
        .filter(|w| w.process.as_deref() == Some(NOTEPAD_IMAGE))
        .collect()
}

/// Poll `cond` every 100 ms until it holds or `timeout` elapses.
fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    loop {
        if cond() {
            return true;
        }
        if started.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn action(action_type: ActionType, target: Option<&str>, value: Option<&str>) -> AgentAction {
    AgentAction {
        action_type,
        target: target.map(str::to_string),
        value: value.map(str::to_string),
        direction: None,
        amount: None,
        coords: None,
    }
}

#[test]
#[ignore = "V2-M0: on-target only — drives a real Notepad window"]
fn v2m0_uia_executor_drives_notepad_end_to_end() {
    assert!(
        notepad_windows().is_empty(),
        "V2-M0 setup: a Notepad window is already open — close it first (the gate kills every notepad.exe on exit)"
    );
    let child = Command::new(NOTEPAD_IMAGE).spawn().expect("spawn notepad.exe (on PATH)");
    let _guard = NotepadGuard { child };
    assert!(
        wait_until(WINDOW_APPEAR, || !notepad_windows().is_empty()),
        "V2-M0 setup: no Notepad window appeared within {WINDOW_APPEAR:?}"
    );
    let notepad = notepad_windows().remove(0);
    println!("notepad window: {notepad:?}");

    let exec = UiaExecutor::default();
    // The gate is the on-target loop stand-in; minting the ticket here is the
    // sanctioned exception (gates is lint-exempt — Doc 24 F2).
    let ticket = ExecutorTicket::for_task(uuid::Uuid::new_v4());
    let run = |a: AgentAction| {
        let r = exec.execute(&ticket, &a);
        println!("{:?} {:?} {:?} → {r:?}", a.action_type, a.target, a.value);
        r
    };

    // 1. switch_window grounds on the title/process stem and wins foreground.
    let out = run(action(ActionType::SwitchWindow, Some("Notepad"), None)).expect("switch_window");
    assert!(out.description.starts_with("switched to"), "{}", out.description);
    std::thread::sleep(Duration::from_millis(300));

    // 2. type → the text lands in the editor (read back through UIA).
    let out = run(action(ActionType::Type, None, Some(TYPED))).expect("type");
    assert_eq!(out.description, format!("typed {} chars", TYPED.len()));
    assert!(
        wait_until(SETTLE, || read_document_text(notepad.hwnd).is_some_and(|t| t.contains(TYPED))),
        "typed text did not appear in the editor: {:?}",
        read_document_text(notepad.hwnd)
    );

    // 3. click grounds a known element by UIA name (Doc 22 §10 gate), then Esc.
    let out = run(action(ActionType::Click, Some("File"), None)).expect("click File");
    assert!(out.description.starts_with("clicked 'File'"), "{}", out.description);
    std::thread::sleep(Duration::from_millis(400));
    run(action(ActionType::Key, None, Some("Esc"))).expect("Esc");
    std::thread::sleep(Duration::from_millis(300));

    // 4. key chords: select all + clear, so the session is left clean.
    run(action(ActionType::Key, None, Some("Ctrl+A"))).expect("Ctrl+A");
    run(action(ActionType::Key, None, Some("Backspace"))).expect("Backspace");
    assert!(
        wait_until(SETTLE, || read_document_text(notepad.hwnd).is_some_and(|t| !t.contains(TYPED))),
        "Ctrl+A / Backspace did not clear the editor: {:?}",
        read_document_text(notepad.hwnd)
    );

    // 5. Alt+F4 closes the window; a "save changes?" prompt (classic Notepad,
    //    or the Store one without session restore) is answered by grounding
    //    the "Don't save" button — the fuzzy-label path the spike exists for.
    run(action(ActionType::Key, None, Some("Alt+F4"))).expect("Alt+F4");
    let gone = || notepad_windows().iter().all(|w| w.hwnd != notepad.hwnd);
    if !wait_until(Duration::from_millis(1500), gone) {
        match run(action(ActionType::Click, Some("Don't save"), None)) {
            Ok(out) => assert!(out.description.starts_with("clicked 'Don"), "{}", out.description),
            Err(ActionError::ElementNotFound(_)) => {
                println!("no save prompt surfaced; window still closing");
            }
            Err(e) => panic!("Don't save: {e}"),
        }
    }
    assert!(
        wait_until(SETTLE, gone),
        "Notepad window survived Alt+F4 (+ Don't save): {:?}",
        notepad_windows()
    );
}
