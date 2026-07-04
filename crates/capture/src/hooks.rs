//! WinEvent hooks on a dedicated thread (doc 05 §3).
//!
//! All hooks run on **one dedicated thread** with its own message loop; the
//! callbacks do **no work** beyond posting to a channel, so OS hook-latency
//! rules are never violated (doc 05 §3). Hook starvation under load is tolerated
//! by design — the heartbeat still samples (doc 05 §7).
//!
//! | Source | Mechanism | Yields |
//! |---|---|---|
//! | Foreground change | `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` | `window_focus` |
//! | Window open/close | `EVENT_OBJECT_SHOW` / `EVENT_OBJECT_DESTROY` filtered to top-level | `window_open`/`window_close` |
//! | Title change | `EVENT_OBJECT_NAMECHANGE` on the foreground hwnd | title refresh |
//!
//! `EVENT_OBJECT_NAMECHANGE` on a browser process additionally triggers a UIA
//! address-bar read (see [`crate::uia`]) for `navigation` events — off this
//! thread (doc 05 §3).

use crate::CaptureError;

/// A raw hook event, posted from the WinEvent callback to the consumer with **no
/// processing** (doc 05 §3). The pipeline drains these via the normalizer; the
/// callback itself only enqueues.
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

/// The WinEvents we install (doc 05 §3). Kept as data so the install/uninstall
/// paths and the OFF release (doc 05 §5 step 3) stay symmetric.
///
/// [VERIFY resolved — Step 0]: constants match the `windows` 0.58 values
/// (EVENT_SYSTEM_FOREGROUND 0x0003, EVENT_OBJECT_DESTROY 0x8001,
/// EVENT_OBJECT_SHOW 0x8002, EVENT_OBJECT_NAMECHANGE 0x800C).
pub mod win_events {
    pub const SYSTEM_FOREGROUND: u32 = 0x0003;
    pub const OBJECT_SHOW: u32 = 0x8002;
    pub const OBJECT_DESTROY: u32 = 0x8001;
    pub const OBJECT_NAMECHANGE: u32 = 0x800C;
}

/// Logical identity of a window (doc 05 §3). Attached to events by the normalizer.
#[derive(Debug, Clone, Default)]
pub struct WindowIdentity {
    /// Friendly application name (e.g. `"Chrome"`), derived from the process stem.
    pub app: Option<String>,
    /// Process image name (e.g. `"chrome.exe"`).
    pub process: Option<String>,
    /// Window title (e.g. the browser tab title).
    pub window_title: Option<String>,
    /// The window class — used by the exclusion check (doc 05 §4, doc 13 §4).
    pub window_class: Option<String>,
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::mpsc::Sender;
    use std::sync::{Mutex, OnceLock};

    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetAncestor, GetClassNameW, GetMessageW, GetWindowTextW,
        GetWindowThreadProcessId, PostThreadMessageW, TranslateMessage, GA_ROOT, MSG,
        OBJID_WINDOW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_QUIT,
    };

    /// The callback's route to the pipeline. One hook thread per process
    /// (enforced by [`HookThread::install`]); a global sender is the only way to
    /// reach a C-ABI callback without leaking closures.
    static SENDER: OnceLock<Mutex<Option<Sender<HookEvent>>>> = OnceLock::new();

    fn sender_slot() -> &'static Mutex<Option<Sender<HookEvent>>> {
        SENDER.get_or_init(|| Mutex::new(None))
    }

    /// The WinEvent callback: classify + post + return. NO other work (doc 05 §3).
    unsafe extern "system" fn win_event_proc(
        _hook: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        id_object: i32,
        id_child: i32,
        _id_thread: u32,
        _time: u32,
    ) {
        // Only whole-window object events; child/UI-element noise is dropped here.
        if id_object != OBJID_WINDOW.0 || id_child != 0 {
            return;
        }
        // Only top-level windows (doc 05 §3).
        if hwnd.0.is_null() {
            return;
        }
        let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
        if root != hwnd {
            return;
        }
        let ev = match event {
            win_events::SYSTEM_FOREGROUND => HookEvent::ForegroundChanged { hwnd: hwnd.0 as isize },
            win_events::OBJECT_SHOW => HookEvent::WindowOpened { hwnd: hwnd.0 as isize },
            win_events::OBJECT_DESTROY => HookEvent::WindowClosed { hwnd: hwnd.0 as isize },
            win_events::OBJECT_NAMECHANGE => HookEvent::TitleChanged { hwnd: hwnd.0 as isize },
            _ => return,
        };
        if let Ok(guard) = sender_slot().lock() {
            if let Some(tx) = guard.as_ref() {
                let _ = tx.send(ev); // closed channel: drop, never block (doc 05 §3)
            }
        }
    }

    /// Owns the dedicated hook thread, its message loop, and the installed hook
    /// handles. [`HookThread::uninstall`] (or drop) unhooks everything — step 3
    /// of the OFF path (doc 05 §5).
    pub struct HookThread {
        join: Option<std::thread::JoinHandle<()>>,
        thread_id: u32,
    }

    impl HookThread {
        /// Spawn the dedicated thread, `SetWinEventHook` each event in
        /// [`win_events`], and run the message loop. Hook callbacks post
        /// [`HookEvent`]s to `tx` and return immediately (doc 05 §3).
        pub fn install(tx: Sender<HookEvent>) -> Result<Self, CaptureError> {
            {
                let mut guard = sender_slot().lock().expect("hook sender lock");
                if guard.is_some() {
                    return Err(CaptureError::HookFailed(
                        "hook thread already installed (one per process)".into(),
                    ));
                }
                *guard = Some(tx);
            }

            let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<u32, String>>();
            let join = std::thread::Builder::new()
                .name("aperture-winevent-hooks".into())
                .spawn(move || unsafe {
                    let thread_id = windows::Win32::System::Threading::GetCurrentThreadId();

                    // WINEVENT_OUTOFCONTEXT: no DLL injection; SKIPOWNPROCESS: our
                    // own overlay never feeds the model (doc 05 §2 self-exclusion).
                    let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
                    let ranges = [
                        (win_events::SYSTEM_FOREGROUND, win_events::SYSTEM_FOREGROUND),
                        (win_events::OBJECT_DESTROY, win_events::OBJECT_SHOW), // 0x8001..=0x8002
                        (win_events::OBJECT_NAMECHANGE, win_events::OBJECT_NAMECHANGE),
                    ];
                    let mut hooks: Vec<HWINEVENTHOOK> = Vec::new();
                    for (lo, hi) in ranges {
                        let h = SetWinEventHook(
                            lo,
                            hi,
                            None,
                            Some(win_event_proc),
                            0, // all processes
                            0, // all threads
                            flags,
                        );
                        if h.is_invalid() {
                            for h in &hooks {
                                let _ = UnhookWinEvent(*h);
                            }
                            let _ = ready_tx
                                .send(Err(format!("SetWinEventHook({lo:#x}..{hi:#x}) failed")));
                            return;
                        }
                        hooks.push(h);
                    }
                    let _ = ready_tx.send(Ok(thread_id));

                    // Message loop until WM_QUIT (posted by uninstall()).
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }

                    for h in &hooks {
                        let _ = UnhookWinEvent(*h);
                    }
                })
                .map_err(|e| CaptureError::HookFailed(format!("spawn hook thread: {e}")))?;

            match ready_rx.recv() {
                Ok(Ok(thread_id)) => Ok(Self { join: Some(join), thread_id }),
                Ok(Err(e)) => {
                    *sender_slot().lock().expect("hook sender lock") = None;
                    Err(CaptureError::HookFailed(e))
                }
                Err(_) => {
                    *sender_slot().lock().expect("hook sender lock") = None;
                    Err(CaptureError::HookFailed("hook thread died during install".into()))
                }
            }
        }

        /// `UnhookWinEvent` every handle and stop the message loop (doc 05 §5
        /// step 3). Idempotent; part of the STOPPING transition and the
        /// SLA-breach force path.
        pub fn uninstall(&mut self) {
            if let Some(join) = self.join.take() {
                unsafe {
                    let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
                }
                let _ = join.join();
            }
            *sender_slot().lock().expect("hook sender lock") = None;
        }
    }

    impl Drop for HookThread {
        fn drop(&mut self) {
            self.uninstall();
        }
    }

    /// Resolve `(app, process, window_title, window_class)` for an hwnd — used
    /// by the foreground handler and the normalizer (doc 05 §3, §6). Returns
    /// logical metadata only; no frame is touched here.
    pub fn window_identity(hwnd: isize) -> WindowIdentity {
        let hwnd = HWND(hwnd as *mut _);
        unsafe {
            let mut title_buf = [0u16; 512];
            let title_len = GetWindowTextW(hwnd, &mut title_buf) as usize;
            let window_title =
                (title_len > 0).then(|| String::from_utf16_lossy(&title_buf[..title_len.min(512)]));

            let mut class_buf = [0u16; 256];
            let class_len = GetClassNameW(hwnd, &mut class_buf) as usize;
            let window_class =
                (class_len > 0).then(|| String::from_utf16_lossy(&class_buf[..class_len.min(256)]));

            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let process = (pid != 0).then(|| process_image_name(pid)).flatten();
            let app = process.as_deref().map(friendly_app_name);

            WindowIdentity { app, process, window_title, window_class }
        }
    }

    /// `pid → "chrome.exe"` via `QueryFullProcessImageNameW` (doc 05 §3).
    fn process_image_name(pid: u32) -> Option<String> {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut len,
            );
            let _ = CloseHandle(handle);
            ok.ok()?;
            let full = String::from_utf16_lossy(&buf[..len as usize]);
            Some(full.rsplit(['\\', '/']).next().unwrap_or(&full).to_string())
        }
    }

    /// `"chrome.exe" → "Chrome"` — a human-facing app label for bubbles.
    fn friendly_app_name(process: &str) -> String {
        let stem = process.strip_suffix(".exe").unwrap_or(process);
        let mut chars = stem.chars();
        match chars.next() {
            Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            None => stem.to_string(),
        }
    }
}

#[cfg(windows)]
pub use imp::{window_identity, HookThread};

// Non-Windows builds: the capture subsystem is a Windows-only surface (locked
// decision 1: Windows 11 only); inert stubs keep cross-platform type-checks alive.
#[cfg(not(windows))]
pub struct HookThread;
#[cfg(not(windows))]
impl HookThread {
    pub fn install(_tx: std::sync::mpsc::Sender<HookEvent>) -> Result<Self, CaptureError> {
        Err(CaptureError::HookFailed("windows-only".into()))
    }
    pub fn uninstall(&mut self) {}
}
#[cfg(not(windows))]
pub fn window_identity(_hwnd: isize) -> WindowIdentity {
    WindowIdentity::default()
}
