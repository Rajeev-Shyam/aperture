//! UI Automation address-bar read for navigation URLs (doc 05 §3).
//!
//! On focus/title-change of a **browser** process, walk the UIA tree to the
//! address-bar Edit element and read its value as the current URL, yielding a
//! `navigation { url }` event (doc 05 §3, [`aperture_contracts::EventType::Navigation`]).
//!
//! **RK4 — localization/version caveat.** The address bar is identified by its
//! accessible name ("Address and search bar" in en-US Chrome), which is **language-
//! and browser-version dependent** (doc 05 §3). When the read fails we do **not**
//! guess: fall back to the last-known URL or skip the `navigation` event entirely,
//! and the browser connector marks its state stale (doc 05 §7, RK4). A wrong URL
//! is worse than no URL — it would resume the user somewhere they never were.

// TODO(M1): the UIA address-bar read lands in the M1 capture milestone.

/// Outcome of an address-bar read (doc 05 §3, §7). Distinguishes "no URL" from
/// "read failed" so the normalizer can choose between skip vs. last-known (RK4).
#[derive(Debug, Clone)]
pub enum AddressBarRead {
    /// Successfully read a URL from the address-bar Edit element.
    Url(String),
    /// The element was found but empty (e.g. a new tab page).
    Empty,
    /// The read failed — localization/version mismatch or UIA timeout (RK4).
    /// Caller falls back to last-known or skips the event (doc 05 §7).
    Unavailable,
}

/// Per-browser hints for locating the address bar. Names are the en-US defaults
/// and are **best-effort** — see RK4 (doc 05 §3). User/locale overrides extend
/// this set from settings.
#[derive(Debug, Clone)]
pub struct AddressBarHints {
    /// Process image names treated as browsers (e.g. `chrome.exe`, `msedge.exe`,
    /// `firefox.exe`).
    pub browser_processes: Vec<String>,
    /// Candidate accessible names for the address-bar Edit element, in priority
    /// order. en-US Chrome: "Address and search bar" (doc 05 §3) — RK4.
    pub address_bar_names: Vec<String>,
}

impl Default for AddressBarHints {
    fn default() -> Self {
        // TODO(M1): seed from settings; allow locale-specific overrides (RK4).
        // [VERIFY] accessible names per browser/locale at build time.
        Self {
            browser_processes: vec![
                "chrome.exe".into(),
                "msedge.exe".into(),
                "firefox.exe".into(),
                "brave.exe".into(),
            ],
            address_bar_names: vec!["Address and search bar".into()],
        }
    }
}

/// Whether a process is one we attempt an address-bar read on (doc 05 §3).
pub fn is_browser_process(_process: &str, _hints: &AddressBarHints) -> bool {
    // TODO(M1): case-insensitive match against hints.browser_processes.
    todo!("M1: classify browser processes for navigation reads")
}

/// Read the address-bar URL for a browser window via UIA (doc 05 §3).
///
/// `hwnd` is the foreground browser window (as `isize`). Walks the UIA element
/// tree for an Edit control whose accessible name matches `hints.address_bar_names`
/// and returns its value. Never blocks the hook thread — invoked off it (doc 05 §3).
pub fn read_address_bar(_hwnd: isize, _hints: &AddressBarHints) -> AddressBarRead {
    // TODO(M1):
    //   1. CUIAutomation::ElementFromHandle(hwnd) [VERIFY COM init on this thread].
    //   2. FindFirst with a name/control-type condition (Edit + accessible name).
    //   3. read ValuePattern.CurrentValue; map empty → Empty, miss/err → Unavailable.
    // RK4: on any miss, return Unavailable — do NOT fabricate a URL.
    todo!("M1: UIA address-bar Edit read (RK4 localization caveat)")
}
