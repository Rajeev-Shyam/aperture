//! The Win32/UIA primitives behind [`crate::UiaExecutor`] (Doc 22 §3.1,
//! V2-M0). Everything that touches the OS lives here, behind `cfg(windows)`,
//! with inert stubs for other targets so the workspace type-checks anywhere.
//!
//! **UI-only by construction** (locked decision 3, Doc 24 #52): the only
//! output channels are `SendInput` (mouse/keyboard) and `SetForegroundWindow`.
//! No process, file, registry, or socket API is reachable from this module —
//! `xtask lint-emitters` denies the spawn surface crate-wide.
//!
//! COM/UIA: one `CUIAutomation` per calling thread (thread-local, the same
//! pattern as `capture/src/uia.rs`); every UIA call happens on the caller's
//! thread.
//!
//! Coordinates: UIA bounding rectangles are physical screen pixels; absolute
//! `SendInput` moves are normalised against the **primary monitor's physical**
//! size read via `GetDeviceCaps(DESKTOPHORZRES/VERTRES)`, which is not DPI-
//! virtualised even in a DPI-unaware process (so the executor never has to
//! change the host process's DPI awareness). v2 is primary-monitor only
//! (Doc 22 §14).

/// Where a click landed and how the element was grounded — feeds the outcome
/// description ("clicked 'Submit' (exact) at (x,y)").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickReport {
    /// The UIA name of the element that was clicked.
    pub name: String,
    /// How `name` matched Claude's target.
    pub quality: crate::grounding::MatchQuality,
    /// Screen position clicked (physical px); `None` when the element was
    /// invoked via `InvokePattern` because it had no on-screen rectangle.
    pub point: Option<(i32, i32)>,
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use aperture_contracts::agent::{ActionError, ScrollDirection};
    use windows::core::{PWSTR, VARIANT};
    use windows::Win32::Foundation::{CloseHandle, BOOL, HANDLE, HWND, LPARAM, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, DESKTOPHORZRES, DESKTOPVERTRES};
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Threading::{
        AttachThreadInput, GetCurrentProcess, GetCurrentThreadId, OpenProcess, OpenProcessToken,
        QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationCondition, IUIAutomationElement,
        IUIAutomationInvokePattern, IUIAutomationScrollItemPattern, IUIAutomationTextPattern,
        IUIAutomationValuePattern, TreeScope_Descendants, UIA_ButtonControlTypeId,
        UIA_CheckBoxControlTypeId, UIA_ComboBoxControlTypeId, UIA_ControlTypePropertyId,
        UIA_DocumentControlTypeId, UIA_EditControlTypeId, UIA_HyperlinkControlTypeId,
        UIA_ImageControlTypeId, UIA_InvokePatternId, UIA_ListItemControlTypeId,
        UIA_MenuItemControlTypeId, UIA_RadioButtonControlTypeId, UIA_ScrollItemPatternId,
        UIA_SplitButtonControlTypeId, UIA_TabItemControlTypeId, UIA_TextControlTypeId,
        UIA_TextPatternId, UIA_TreeItemControlTypeId, UIA_ValuePatternId, UIA_CONTROLTYPE_ID,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MapVirtualKeyW, SendInput, VkKeyScanW, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
        KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC,
        MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_MOVE, MOUSEEVENTF_WHEEL, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetForegroundWindow, GetWindowTextW, SendMessageTimeoutW,
        SMTO_ABORTIFHUNG, WM_NULL,
        GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, SetForegroundWindow,
        ShowWindow, SW_RESTORE, WHEEL_DELTA,
    };

    use super::ClickReport;
    use crate::grounding::{best_index, MatchQuality};
    use crate::keys::{is_extended, modifier_virtual_key, virtual_key, Chord, KeyName};
    use crate::WindowInfo;

    // ------------------------------------------------------------------
    // Windows / processes
    // ------------------------------------------------------------------

    /// `EnumWindows` callback: append every visible, titled top-level window.
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = unsafe { &mut *(lparam.0 as *mut Vec<WindowInfo>) };
        if unsafe { IsWindowVisible(hwnd) }.as_bool() {
            if let Some(info) = window_info(hwnd.0 as isize) {
                if !info.title.is_empty() {
                    out.push(info);
                }
            }
        }
        BOOL(1)
    }

    /// Every visible, titled top-level window, in Z order (foreground first).
    pub fn list_open_windows() -> Vec<WindowInfo> {
        let mut out: Vec<WindowInfo> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(enum_proc), LPARAM(&mut out as *mut _ as isize));
        }
        out
    }

    /// The current foreground window, if any.
    pub fn foreground_window() -> Option<WindowInfo> {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return None;
        }
        window_info(hwnd.0 as isize)
    }

    /// Title / process / class for an hwnd (`None` if it is not a window).
    pub fn window_info(hwnd: isize) -> Option<WindowInfo> {
        let h = HWND(hwnd as *mut _);
        unsafe {
            if !IsWindow(h).as_bool() {
                return None;
            }
            let mut title_buf = [0u16; 512];
            let n = GetWindowTextW(h, &mut title_buf) as usize;
            let title = String::from_utf16_lossy(&title_buf[..n.min(512)]);
            let mut class_buf = [0u16; 256];
            let n = GetClassNameW(h, &mut class_buf) as usize;
            let window_class = (n > 0).then(|| String::from_utf16_lossy(&class_buf[..n.min(256)]));
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            let process = (pid != 0).then(|| process_image_name(pid)).flatten();
            Some(WindowInfo { hwnd, title, process, window_class })
        }
    }

    /// `pid → "chrome.exe"` (lowercased basename) via `QueryFullProcessImageNameW`,
    /// the same shape capture's `window_identity` produces.
    fn process_image_name(pid: u32) -> Option<String> {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            );
            let _ = CloseHandle(handle);
            ok.ok()?;
            let full = String::from_utf16_lossy(&buf[..len as usize]);
            Some(full.rsplit(['\\', '/']).next().unwrap_or(&full).to_ascii_lowercase())
        }
    }

    /// Is the process token behind `handle` elevated? `None` when the token
    /// cannot be queried (the caller treats that as elevated — fail closed).
    fn token_is_elevated(process: HANDLE) -> Option<bool> {
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;
            let mut info = TOKEN_ELEVATION::default();
            let mut returned = 0u32;
            let res = GetTokenInformation(
                token,
                TokenElevation,
                Some(&mut info as *mut TOKEN_ELEVATION as *mut core::ffi::c_void),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned,
            );
            let _ = CloseHandle(token);
            res.ok()?;
            Some(info.TokenIsElevated != 0)
        }
    }

    /// Decision #50: is the window's process elevated while ours is not?
    /// Access-denied on the target process counts as elevated.
    pub fn window_is_elevated_above_us(hwnd: isize) -> bool {
        unsafe {
            let ours = token_is_elevated(GetCurrentProcess()).unwrap_or(false);
            if ours {
                return false; // we can reach anything we are allowed to see
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(HWND(hwnd as *mut _), Some(&mut pid));
            if pid == 0 {
                return false;
            }
            let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return true; // cannot even open it: treat as elevated
            };
            let elevated = token_is_elevated(handle).unwrap_or(true);
            let _ = CloseHandle(handle);
            elevated
        }
    }

    /// `SetForegroundWindow` with the two standard nudges when Windows refuses
    /// (foreground-lock rules): attach to the foreground thread's input, then
    /// an Alt tap (our process becomes the last input source) and retry.
    pub fn bring_to_foreground(hwnd: isize) -> bool {
        let h = HWND(hwnd as *mut _);
        let is_fg = || unsafe { GetForegroundWindow() } == h;
        unsafe {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
            std::thread::sleep(Duration::from_millis(80));
            if is_fg() {
                return true;
            }
            // Nudge 1: borrow the current foreground thread's input state.
            let fg = GetForegroundWindow();
            let fg_tid = GetWindowThreadProcessId(fg, None);
            let ours = GetCurrentThreadId();
            let attached = fg_tid != 0 && fg_tid != ours && AttachThreadInput(ours, fg_tid, true).as_bool();
            let _ = SetForegroundWindow(h);
            if attached {
                let _ = AttachThreadInput(ours, fg_tid, false);
            }
            std::thread::sleep(Duration::from_millis(80));
            if is_fg() {
                return true;
            }
            // Nudge 2: an Alt tap makes us the last input source.
            let alt = modifier_virtual_key(crate::keys::Modifier::Alt);
            let _ = send(&[key_input(alt, KEYBD_EVENT_FLAGS(0)), key_input(alt, KEYEVENTF_KEYUP)]);
            let _ = SetForegroundWindow(h);
            std::thread::sleep(Duration::from_millis(120));
            is_fg()
        }
    }

    // ------------------------------------------------------------------
    // SendInput
    // ------------------------------------------------------------------

    /// Gap between typed characters (see [`type_text`]).
    const TYPE_CHAR_GAP: Duration = Duration::from_millis(20);
    /// Down→up hold per typed key (see `type_char`).
    const TYPE_KEY_HOLD: Duration = Duration::from_millis(12);
    /// Settle after the first typed character (see `type_text`).
    const TYPE_FIRST_CHAR_SETTLE: Duration = Duration::from_millis(200);
    /// Settle after a foreground switch (see `settle_after_switch`).
    const SWITCH_SETTLE: Duration = Duration::from_millis(500);

    /// A virtual-key event. The hardware scan code is filled in too: XAML /
    /// WinUI apps (Win11 Notepad among them) route accelerators such as
    /// Alt+F4 by scan code and ignore a bare `wVk` event.
    fn key_input(vk: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } as u16;
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
            },
        }
    }

    fn unicode_input(unit: u16, up: bool) -> INPUT {
        let flags = if up { KEYEVENTF_UNICODE | KEYEVENTF_KEYUP } else { KEYEVENTF_UNICODE };
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: unit, dwFlags: flags, time: 0, dwExtraInfo: 0 },
            },
        }
    }

    fn mouse_input(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: 0 },
            },
        }
    }

    /// One `SendInput` call; a short count means the input was blocked
    /// (UIPI / another thread holding input) — reported, never ignored.
    fn send(inputs: &[INPUT]) -> Result<(), ActionError> {
        if inputs.is_empty() {
            return Ok(());
        }
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() {
            Ok(())
        } else {
            Err(ActionError::NotInteractable(format!(
                "SendInput delivered {sent}/{} events (input blocked)",
                inputs.len()
            )))
        }
    }

    /// Primary monitor size in **physical** pixels (DESKTOPHORZRES is not DPI-
    /// virtualised, unlike GetSystemMetrics in a DPI-unaware process).
    fn primary_physical_size() -> (i32, i32) {
        unsafe {
            let dc = GetDC(None);
            let w = GetDeviceCaps(dc, DESKTOPHORZRES);
            let h = GetDeviceCaps(dc, DESKTOPVERTRES);
            let _ = ReleaseDC(None, dc);
            (w.max(1), h.max(1))
        }
    }

    /// Physical px → the 0..=65535 absolute space `SendInput` expects.
    fn to_absolute(x: i32, y: i32) -> (i32, i32) {
        let (w, h) = primary_physical_size();
        let nx = (i64::from(x) * 65535 / i64::from((w - 1).max(1))).clamp(0, 65535) as i32;
        let ny = (i64::from(y) * 65535 / i64::from((h - 1).max(1))).clamp(0, 65535) as i32;
        (nx, ny)
    }

    fn move_cursor_to(x: i32, y: i32) -> Result<(), ActionError> {
        let (ax, ay) = to_absolute(x, y);
        send(&[mouse_input(ax, ay, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE)])
    }

    /// Left-click at physical screen coordinates (move, settle, down, up).
    pub fn click_at(x: i32, y: i32) -> Result<(), ActionError> {
        move_cursor_to(x, y)?;
        std::thread::sleep(Duration::from_millis(40));
        let (ax, ay) = to_absolute(x, y);
        send(&[
            mouse_input(ax, ay, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_LEFTDOWN),
            mouse_input(ax, ay, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_LEFTUP),
        ])
    }

    /// Press + release one virtual key.
    pub fn tap_key(vk: u16) -> Result<(), ActionError> {
        let target = unsafe { GetForegroundWindow() }.0 as isize;
        send(&[key_input(vk, KEYBD_EVENT_FLAGS(0))])?;
        wait_until_target_pumps(target);
        std::thread::sleep(TYPE_KEY_HOLD);
        send(&[key_input(vk, KEYEVENTF_KEYUP)])
    }

    /// Modifiers down → key down/up → modifiers up (reverse order), one batch.
    /// Press a chord the way a hand does: modifiers down → (target pumps) →
    /// key down → (target pumps + hold) → key up → modifiers up. One batch
    /// with everything in it fails on WinUI: it reads the modifier state when
    /// it *processes* the key, by which time the batch's Alt-up has landed —
    /// Alt+F4 arrived as a bare F4 (V2-M0 gate, 2026-08-22).
    pub fn send_chord(chord: &Chord) -> Result<(), ActionError> {
        let target = unsafe { GetForegroundWindow() }.0 as isize;
        let ext = if is_extended(chord.key) { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) };
        let vk = virtual_key(chord.key);
        let downs: Vec<INPUT> = chord
            .modifiers
            .iter()
            .map(|m| key_input(modifier_virtual_key(*m), KEYBD_EVENT_FLAGS(0)))
            .collect();
        if !downs.is_empty() {
            send(&downs)?;
            wait_until_target_pumps(target);
            std::thread::sleep(TYPE_KEY_HOLD);
        }
        send(&[key_input(vk, ext)])?;
        wait_until_target_pumps(target);
        std::thread::sleep(TYPE_KEY_HOLD);
        let mut ups = vec![key_input(vk, ext | KEYEVENTF_KEYUP)];
        for m in chord.modifiers.iter().rev() {
            ups.push(key_input(modifier_virtual_key(*m), KEYEVENTF_KEYUP));
        }
        send(&ups)
    }

    /// Type text. Newlines become Enter, tabs become Tab.
    ///
    /// Characters the active keyboard layout can produce go out as REAL key
    /// events (`VkKeyScanW` → virtual key + Shift/Ctrl/Alt state, with the
    /// scan code): that is what every app, including WinUI, handles
    /// reliably. Only characters the layout cannot produce fall back to a
    /// `KEYEVENTF_UNICODE` packet — and those are sent one `SendInput` per
    /// character with a gap, because a `VK_PACKET` carries its character in
    /// per-thread state the target reads when it *processes* the message, so
    /// queued packets resolve to the last one ("text" → "tttt" in Win11
    /// Notepad; seen again as "aperture v2m0" → "auuuuummm0" by the V2-M0
    /// gate, 2026-08-22, even one packet per call).
    /// Stops early (with `Stopped`) if the hard-stop flag is raised mid-text.
    pub fn type_text(text: &str, stop: &Arc<AtomicBool>) -> Result<(), ActionError> {
        // The window that will receive the keys — the foreground one — is the
        // thread whose message loop we wait on between down and up.
        let target = unsafe { GetForegroundWindow() }.0 as isize;
        wait_until_target_pumps(target);
        let mut chars = text.chars().peekable();
        let mut first = true;
        while let Some(c) = chars.next() {
            if stop.load(Ordering::SeqCst) {
                return Err(ActionError::Stopped);
            }
            match c {
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    tap_key(virtual_key(KeyName::Enter))?;
                }
                '\n' => tap_key(virtual_key(KeyName::Enter))?,
                '\t' => tap_key(virtual_key(KeyName::Tab))?,
                _ => type_char(c, target)?,
            }
            // The FIRST edit makes many editors re-layout (dirty marker, title
            // change, undo stack) and drop the keystroke that lands during it —
            // the V2-M0 gate lost the 2nd/3rd character every run until this.
            std::thread::sleep(if first { TYPE_FIRST_CHAR_SETTLE } else { TYPE_CHAR_GAP });
            first = false;
        }
        Ok(())
    }

    /// One character: layout key events when the layout has it, else a
    /// Unicode packet per UTF-16 unit.
    /// Synchronous round-trip to `hwnd`'s message loop: returns once the
    /// owning thread is pumping again (or after 300 ms / if it is hung). Not
    /// a strict "key processed" ack — sent messages outrank posted input —
    /// but it holds the key through any stall the app is in, which is the
    /// failure mode observed.
    fn wait_until_target_pumps(hwnd: isize) {
        if hwnd == 0 {
            return;
        }
        unsafe {
            let _ = SendMessageTimeoutW(
                HWND(hwnd as *mut _),
                WM_NULL,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                300,
                None,
            );
        }
    }

    /// After a window was brought to the foreground: wait for its thread to
    /// pump, then a fixed settle, then pump again. A freshly launched WinUI
    /// app (Store Notepad) silently drops keystrokes for ~500 ms after it
    /// becomes foreground — measured by the V2-M0 gate on 2026-08-22: 300 ms
    /// lost 1–2 of the first 6 characters every run, 700 ms lost none.
    pub fn settle_after_switch(hwnd: isize) {
        wait_until_target_pumps(hwnd);
        std::thread::sleep(SWITCH_SETTLE);
        wait_until_target_pumps(hwnd);
    }

    fn type_char(c: char, target: isize) -> Result<(), ActionError> {
        let mut units = [0u16; 2];
        let encoded = c.encode_utf16(&mut units);
        if encoded.len() == 1 {
            let scan = unsafe { VkKeyScanW(encoded[0]) };
            if scan != -1 {
                let vk = (scan & 0xff) as u16;
                let shift_state = ((scan >> 8) & 0xff) as u8;
                let mut inputs = Vec::with_capacity(8);
                // Bit 1 = Shift, 2 = Ctrl, 4 = Alt (VkKeyScan high byte).
                const MODS: [(u8, u16); 3] = [(1, 0x10), (2, 0x11), (4, 0x12)];
                for (bit, mvk) in MODS {
                    if shift_state & bit != 0 {
                        inputs.push(key_input(mvk, KEYBD_EVENT_FLAGS(0)));
                    }
                }
                inputs.push(key_input(vk, KEYBD_EVENT_FLAGS(0)));
                send(&inputs)?;
                // Hold the key until the target has pumped its queue, plus a
                // short real-keystroke hold. XAML/WinUI drops a key-down it
                // processes after the matching key-up already arrived — the
                // V2-M0 gate lost one of the first four characters on every run
                // with any fixed hold, because Notepad stalls for tens of ms
                // after the first edit (dirty marker, session autosave).
                wait_until_target_pumps(target);
                std::thread::sleep(TYPE_KEY_HOLD);
                let mut ups = vec![key_input(vk, KEYEVENTF_KEYUP)];
                for (bit, mvk) in MODS.iter().rev() {
                    if shift_state & bit != 0 {
                        ups.push(key_input(*mvk, KEYEVENTF_KEYUP));
                    }
                }
                return send(&ups);
            }
        }
        for unit in encoded.iter() {
            send(&[unicode_input(*unit, false), unicode_input(*unit, true)])?;
            std::thread::sleep(TYPE_CHAR_GAP);
        }
        Ok(())
    }

    /// Mouse-wheel `notches` in `direction` at the current cursor position
    /// (or at `at`, physical px, when given).
    pub fn scroll(direction: ScrollDirection, notches: u32, at: Option<(i32, i32)>) -> Result<(), ActionError> {
        if let Some((x, y)) = at {
            move_cursor_to(x, y)?;
            std::thread::sleep(Duration::from_millis(30));
        }
        let delta = WHEEL_DELTA as i32 * notches.min(100) as i32;
        let (flags, signed) = match direction {
            ScrollDirection::Up => (MOUSEEVENTF_WHEEL, delta),
            ScrollDirection::Down => (MOUSEEVENTF_WHEEL, -delta),
            ScrollDirection::Right => (MOUSEEVENTF_HWHEEL, delta),
            ScrollDirection::Left => (MOUSEEVENTF_HWHEEL, -delta),
        };
        send(&[mouse_input(0, 0, signed as u32, flags)])
    }

    // ------------------------------------------------------------------
    // UIA
    // ------------------------------------------------------------------

    thread_local! {
        /// One UIA automation object per calling thread (COM apartment-bound) —
        /// the `capture/src/uia.rs` pattern.
        static AUTOMATION: std::cell::RefCell<Option<IUIAutomation>> =
            const { std::cell::RefCell::new(None) };
    }

    fn automation() -> Option<IUIAutomation> {
        AUTOMATION.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                unsafe {
                    // Idempotent per thread; S_FALSE (already initialized) is fine.
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                    match CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) {
                        Ok(a) => *slot = Some(a),
                        Err(e) => tracing::warn!(%e, "CUIAutomation unavailable"),
                    }
                }
            }
            slot.clone()
        })
    }

    /// Control types a `click` may ground to (Doc 22 §6).
    const CLICKABLE_TYPES: &[UIA_CONTROLTYPE_ID] = &[
        UIA_ButtonControlTypeId,
        UIA_HyperlinkControlTypeId,
        UIA_MenuItemControlTypeId,
        UIA_ListItemControlTypeId,
        UIA_TabItemControlTypeId,
        UIA_CheckBoxControlTypeId,
        UIA_RadioButtonControlTypeId,
        UIA_ComboBoxControlTypeId,
        UIA_EditControlTypeId,
        UIA_TextControlTypeId,
        UIA_TreeItemControlTypeId,
        UIA_SplitButtonControlTypeId,
        UIA_ImageControlTypeId,
    ];

    /// `ControlType ∈ types` as one OR condition.
    fn control_type_condition(auto: &IUIAutomation, types: &[UIA_CONTROLTYPE_ID]) -> Option<IUIAutomationCondition> {
        unsafe {
            let conds: Vec<Option<IUIAutomationCondition>> = types
                .iter()
                .map(|t| auto.CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(t.0)).ok())
                .collect();
            if conds.iter().any(Option::is_none) {
                return None;
            }
            auto.CreateOrConditionFromNativeArray(&conds).ok()
        }
    }

    fn element_name(el: &IUIAutomationElement) -> String {
        unsafe { el.CurrentName().map(|b| b.to_string()).unwrap_or_default() }
    }

    fn descendants(hwnd: isize, types: &[UIA_CONTROLTYPE_ID]) -> Vec<IUIAutomationElement> {
        let Some(auto) = automation() else { return Vec::new() };
        unsafe {
            let Ok(root) = auto.ElementFromHandle(HWND(hwnd as *mut _)) else { return Vec::new() };
            let Some(cond) = control_type_condition(&auto, types) else { return Vec::new() };
            let Ok(list) = root.FindAll(TreeScope_Descendants, &cond) else { return Vec::new() };
            let n = list.Length().unwrap_or(0);
            (0..n).filter_map(|i| list.GetElement(i).ok()).collect()
        }
    }

    /// Best label match among the clickable descendants of `hwnd`.
    fn find_element(hwnd: isize, target: &str) -> Option<(IUIAutomationElement, String, MatchQuality)> {
        let els = descendants(hwnd, CLICKABLE_TYPES);
        let names: Vec<String> = els.iter().map(element_name).collect();
        let (i, q) = best_index(names.iter().map(String::as_str), target)?;
        Some((els[i].clone(), names[i].clone(), q))
    }

    fn rect_centre(r: &RECT) -> Option<(i32, i32)> {
        (r.right > r.left && r.bottom > r.top).then(|| ((r.left + r.right) / 2, (r.top + r.bottom) / 2))
    }

    /// Physical centre of the element matching `target` (scroll anchoring).
    pub fn element_centre(hwnd: isize, target: &str) -> Option<(i32, i32, MatchQuality)> {
        let (el, _, q) = find_element(hwnd, target)?;
        let rect = unsafe { el.CurrentBoundingRectangle().ok()? };
        rect_centre(&rect).map(|(x, y)| (x, y, q))
    }

    /// Doc 22 §6 grounding: UIA label match within `hwnd`'s subtree → click the
    /// bounding-rect centre. Disabled ⇒ `NotInteractable`; offscreen ⇒ try
    /// `ScrollItemPattern` first; no rectangle ⇒ `InvokePattern` fallback.
    /// `ElementNotFound` when nothing matches (the caller then tries coords).
    pub fn click_element(hwnd: isize, target: &str) -> Result<ClickReport, ActionError> {
        let (el, name, quality) =
            find_element(hwnd, target).ok_or_else(|| ActionError::ElementNotFound(target.to_string()))?;
        unsafe {
            if !el.CurrentIsEnabled().map(|b| b.as_bool()).unwrap_or(true) {
                return Err(ActionError::NotInteractable(format!("'{name}' is disabled")));
            }
            let mut offscreen = el.CurrentIsOffscreen().map(|b| b.as_bool()).unwrap_or(false);
            if offscreen {
                if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationScrollItemPattern>(UIA_ScrollItemPatternId) {
                    let _ = p.ScrollIntoView();
                    std::thread::sleep(Duration::from_millis(150));
                    offscreen = el.CurrentIsOffscreen().map(|b| b.as_bool()).unwrap_or(false);
                }
            }
            let centre = if offscreen {
                None
            } else {
                el.CurrentBoundingRectangle().ok().as_ref().and_then(rect_centre)
            };
            match centre {
                Some((x, y)) => {
                    click_at(x, y)?;
                    Ok(ClickReport { name, quality, point: Some((x, y)) })
                }
                None => {
                    // No usable rectangle: invoke programmatically if the element
                    // supports it, otherwise be honest.
                    let invoke = el
                        .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                        .map_err(|_| ActionError::NotInteractable(format!("'{name}' is offscreen with no invoke pattern")))?;
                    invoke
                        .Invoke()
                        .map_err(|e| ActionError::NotInteractable(format!("'{name}' invoke failed: {e}")))?;
                    Ok(ClickReport { name, quality, point: None })
                }
            }
        }
    }

    /// The UIA name of whatever currently has keyboard focus, if readable.
    pub fn focused_element_name() -> Option<String> {
        let auto = automation()?;
        let name = unsafe { auto.GetFocusedElement().ok().map(|el| element_name(&el))? };
        (!name.trim().is_empty()).then_some(name)
    }

    /// The text of the VISIBLE Document (else Edit) control under `hwnd`, via
    /// `ValuePattern` then `TextPattern`. `None` when there is no such control.
    ///
    /// "Visible" matters: the Store Notepad restores every unsaved tab at
    /// launch and keeps each one's editor in the UIA tree, offscreen — the
    /// first Document found is then a background tab, not the one the user
    /// (or the executor) is typing into (found by the V2-M0 gate, 2026-08-22).
    pub fn read_document_text(hwnd: isize) -> Option<String> {
        let pick = |els: Vec<IUIAutomationElement>| {
            let onscreen = els
                .iter()
                .find(|e| unsafe { e.CurrentIsOffscreen().map(|b| !b.as_bool()).unwrap_or(true) })
                .cloned();
            onscreen.or_else(|| els.into_iter().next())
        };
        let el = pick(descendants(hwnd, &[UIA_DocumentControlTypeId]))
            .or_else(|| pick(descendants(hwnd, &[UIA_EditControlTypeId])))?;
        unsafe {
            if let Ok(v) = el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
                if let Ok(s) = v.CurrentValue() {
                    let s = s.to_string();
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
            if let Ok(t) = el.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) {
                if let Ok(range) = t.DocumentRange() {
                    if let Ok(s) = range.GetText(-1) {
                        return Some(s.to_string());
                    }
                }
            }
            Some(String::new())
        }
    }
}

#[cfg(windows)]
pub use imp::{
    bring_to_foreground, click_at, click_element, element_centre, focused_element_name,
    foreground_window, list_open_windows, read_document_text, scroll, send_chord,
    settle_after_switch, tap_key, type_text, window_info, window_is_elevated_above_us,
};

// Non-Windows builds: the executor is a Windows-only surface (locked decision 1);
// inert stubs keep cross-platform type-checks alive.
#[cfg(not(windows))]
mod stub {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use aperture_contracts::agent::{ActionError, ScrollDirection};

    use super::ClickReport;
    use crate::grounding::MatchQuality;
    use crate::keys::Chord;
    use crate::WindowInfo;

    fn unsupported() -> ActionError {
        ActionError::Unsupported("windows-only".into())
    }
    pub fn list_open_windows() -> Vec<WindowInfo> {
        Vec::new()
    }
    pub fn foreground_window() -> Option<WindowInfo> {
        None
    }
    pub fn window_info(_hwnd: isize) -> Option<WindowInfo> {
        None
    }
    pub fn window_is_elevated_above_us(_hwnd: isize) -> bool {
        false
    }
    pub fn bring_to_foreground(_hwnd: isize) -> bool {
        false
    }
    pub fn settle_after_switch(_hwnd: isize) {}
    pub fn click_at(_x: i32, _y: i32) -> Result<(), ActionError> {
        Err(unsupported())
    }
    pub fn tap_key(_vk: u16) -> Result<(), ActionError> {
        Err(unsupported())
    }
    pub fn send_chord(_chord: &Chord) -> Result<(), ActionError> {
        Err(unsupported())
    }
    pub fn type_text(_text: &str, _stop: &Arc<AtomicBool>) -> Result<(), ActionError> {
        Err(unsupported())
    }
    pub fn scroll(_d: ScrollDirection, _n: u32, _at: Option<(i32, i32)>) -> Result<(), ActionError> {
        Err(unsupported())
    }
    pub fn element_centre(_hwnd: isize, _target: &str) -> Option<(i32, i32, MatchQuality)> {
        None
    }
    pub fn click_element(_hwnd: isize, target: &str) -> Result<ClickReport, ActionError> {
        Err(ActionError::ElementNotFound(target.to_string()))
    }
    pub fn focused_element_name() -> Option<String> {
        None
    }
    pub fn read_document_text(_hwnd: isize) -> Option<String> {
        None
    }
}

#[cfg(not(windows))]
pub use stub::{
    bring_to_foreground, click_at, click_element, element_centre, focused_element_name,
    foreground_window, list_open_windows, read_document_text, scroll, send_chord, tap_key,
    type_text, window_info, window_is_elevated_above_us,
};
