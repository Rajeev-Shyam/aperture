//! WinEvent hooks on a dedicated thread (doc 05 §3).
//!
//! All hooks run on **one dedicated thread** with its own message loop; the
//! callbacks do **no work** beyond posting to the bus, so OS hook-latency rules
//! are never violated (doc 05 §3). Hook starvation under load is tolerated by
//! design — the 10 s heartbeat still samples (doc 05 §7).
//!
//! | Source | Mechanism | Yields |
//! |---|---|---|
//! | Foreground change | `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` + `GetForegroundWindow`/`GetWindowThreadProcessId` | `window_focus` |
//! | Window open/close | `EVENT_OBJECT_SHOW` / `EVENT_OBJECT_DESTROY` filtered to top-level | `window_open`/`window_close` |
//! | Title change | `EVENT_OBJECT_NAMECHANGE` on the foreground hwnd | title refresh |
//!
//! `EVENT_OBJECT_NAMECHANGE` on a browser process additionally triggers a UIA
//! address-bar read (see [`crate::uia`]) for `navigation` events.

// TODO(M1): the hook thread lands in the M1 capture milestone.

use crate::CaptureError;

/// A raw hook event, posted from the WinEvent callback to the consumer with **no
/// processing** (doc 05 §3). The dedicated thread drains these onto the bus via
/// the normalizer; the callback itself only enqueues.
#[derive(Debug, Clone)]
pub enum HookEvent {
    /// Foreground window changed; carries the new foreground hwnd (as `isize`).
    ForegroundChanged { hwnd: isize },
    /// A top-level window was shown.
    WindowOpened { hwnd: isize },
    /// A top-level window was destroyed.
    WindowClosed { hwnd: isize },
    /// The foreground window's title changed (feeds document/IDE connectors and,
    /// for browsers, a UIA navigation read — doc 05 §3).
    TitleChanged { hwnd: isize },
}

/// The set of WinEvents we install (doc 05 §3). Kept as data so the install/
/// uninstall paths and the OFF release (doc 05 §5 step 3) stay symmetric.
pub mod win_events {
    /// `EVENT_SYSTEM_FOREGROUND` — foreground window changed. [VERIFY constant].
    pub const SYSTEM_FOREGROUND: u32 = 0x0003;
    /// `EVENT_OBJECT_SHOW` — object (window) shown. [VERIFY constant].
    pub const OBJECT_SHOW: u32 = 0x8002;
    /// `EVENT_OBJECT_DESTROY` — object destroyed. [VERIFY constant].
    pub const OBJECT_DESTROY: u32 = 0x8001;
    /// `EVENT_OBJECT_NAMECHANGE` — object name (title) changed. [VERIFY constant].
    pub const OBJECT_NAMECHANGE: u32 = 0x800C;
}

/// Owns the dedicated hook thread, its message loop, and the installed hook
/// handles. Dropping (or [`HookThread::uninstall`]) unhooks everything — step 3
/// of the OFF path (doc 05 §5).
pub struct HookThread {
    // join: std::thread::JoinHandle<()>,
    // hooks: Vec<windows::Win32::UI::Accessibility::HWINEVENTHOOK>,
    // The callback posts HookEvents here; the normalizer drains them (doc 05 §6).
    // tx: tokio::sync::mpsc::UnboundedSender<HookEvent>,
}

impl HookThread {
    /// Spawn the dedicated thread, `SetWinEventHook` each event in
    /// [`win_events`], and run the message loop. Hook callbacks post [`HookEvent`]s
    /// to `tx` and return immediately (doc 05 §3).
    pub fn install(
        // tx: tokio::sync::mpsc::UnboundedSender<HookEvent>,
    ) -> Result<Self, CaptureError> {
        // TODO(M1):
        //   1. spawn a thread; CoInitialize / set up an STA message loop.
        //   2. SetWinEventHook for SYSTEM_FOREGROUND, OBJECT_SHOW/DESTROY,
        //      OBJECT_NAMECHANGE (WINEVENT_OUTOFCONTEXT) [VERIFY flags].
        //   3. callback: GetForegroundWindow / GetWindowThreadProcessId, filter to
        //      top-level windows, post a HookEvent, return — NO other work.
        //   4. GetMessage loop until WM_QUIT.
        todo!("M1: install WinEvent hooks on a dedicated thread")
    }

    /// `UnhookWinEvent` every handle and stop the message loop (doc 05 §5 step 3).
    /// Idempotent; part of the STOPPING transition and the SLA-breach force path.
    pub fn uninstall(&mut self) {
        // TODO(M1): post WM_QUIT; UnhookWinEvent each handle; join the thread.
        todo!("M1: UnhookWinEvent + stop the hook thread")
    }
}

/// Resolve `(app, process, window_title)` for an hwnd — used by the foreground
/// handler and the normalizer (doc 05 §3, §6). Returns logical metadata only; no
/// frame is touched here.
pub fn window_identity(_hwnd: isize) -> WindowIdentity {
    // TODO(M1): GetWindowThreadProcessId → QueryFullProcessImageName → friendly
    //   app name; GetWindowText for the title [VERIFY APIs].
    todo!("M1: resolve app/process/title for a window")
}

/// Logical identity of a window (doc 05 §3). Attached to events by the normalizer.
#[derive(Debug, Clone, Default)]
pub struct WindowIdentity {
    /// Friendly application name (e.g. `"Chrome"`).
    pub app: Option<String>,
    /// Process image name (e.g. `"chrome.exe"`).
    pub process: Option<String>,
    /// Window title (e.g. the browser tab title).
    pub window_title: Option<String>,
    /// The window class — used by the exclusion check (doc 05 §4, doc 13 §4).
    pub window_class: Option<String>,
}
