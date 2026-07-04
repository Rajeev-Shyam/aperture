//! UI Automation address-bar read for navigation URLs (doc 05 §3).
//!
//! On focus/title-change of a **browser** process, walk the UIA tree to the
//! address-bar Edit element and read its value as the current URL, yielding a
//! `navigation { url }` event (doc 05 §3, [`aperture_contracts::EventType::Navigation`]).
//!
//! **R2 framing (ADR-027):** the browser **extension** (tabs API) is the
//! *primary* URL source once it ships at M4; this UIA read is the committed
//! **no-extension fallback**. At M1 (pre-extension) it is the only source.
//!
//! **RK4 — localization/version caveat.** The address bar is identified by its
//! accessible name ("Address and search bar" in en-US Chrome), which is **language-
//! and browser-version dependent** (doc 05 §3). When the read fails we do **not**
//! guess: fall back to the last-known URL or skip the `navigation` event entirely,
//! and the browser connector marks its state stale (doc 05 §7, RK4). A wrong URL
//! is worse than no URL — it would resume the user somewhere they never were.

/// Outcome of an address-bar read (doc 05 §3, §7). Distinguishes "no URL" from
/// "read failed" so the normalizer can choose between skip vs. last-known (RK4).
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// `firefox.exe`, `opera.exe` — Opera GX ships as `opera.exe`, ADR-027).
    pub browser_processes: Vec<String>,
    /// Candidate accessible names for the address-bar Edit element, in priority
    /// order. en-US Chrome: "Address and search bar" (doc 05 §3) — RK4.
    pub address_bar_names: Vec<String>,
}

impl Default for AddressBarHints {
    fn default() -> Self {
        // [VERIFY] accessible names per browser/locale at the M1 hardware gate;
        // extended from settings (RK4).
        Self {
            browser_processes: vec![
                "chrome.exe".into(),
                "msedge.exe".into(),
                "firefox.exe".into(),
                "brave.exe".into(),
                "opera.exe".into(),
                "opera_gx.exe".into(),
            ],
            address_bar_names: vec![
                "Address and search bar".into(), // Chrome/Brave/Opera en-US
                "Search or enter web address".into(), // Firefox en-US
                "Search or enter address".into(),
            ],
        }
    }
}

/// Whether a process is one we attempt an address-bar read on (doc 05 §3).
pub fn is_browser_process(process: &str, hints: &AddressBarHints) -> bool {
    let p = process.to_ascii_lowercase();
    hints
        .browser_processes
        .iter()
        .any(|b| b.eq_ignore_ascii_case(&p))
}

#[cfg(windows)]
mod imp {
    use super::*;

    use windows::core::{BSTR, VARIANT};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationElement, TreeScope_Descendants,
        UIA_ControlTypePropertyId, UIA_EditControlTypeId, UIA_NamePropertyId,
        UIA_ValueValuePropertyId,
    };

    thread_local! {
        /// One UIA automation object per calling thread (COM apartment-bound).
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
                        Err(e) => {
                            tracing::warn!(%e, "CUIAutomation unavailable (RK4)");
                        }
                    }
                }
            }
            slot.clone()
        })
    }

    /// Read the address-bar URL for a browser window via UIA (doc 05 §3).
    ///
    /// `hwnd` is the foreground browser window (as `isize`). Finds a descendant
    /// Edit control whose accessible name matches `hints.address_bar_names` and
    /// returns its ValuePattern value. Never runs on the hook thread — invoked
    /// off it (doc 05 §3).
    pub fn read_address_bar(hwnd: isize, hints: &AddressBarHints) -> AddressBarRead {
        let Some(auto) = automation() else {
            return AddressBarRead::Unavailable;
        };
        unsafe {
            let root: IUIAutomationElement = match auto
                .ElementFromHandle(windows::Win32::Foundation::HWND(hwnd as *mut _))
            {
                Ok(el) => el,
                Err(_) => return AddressBarRead::Unavailable,
            };

            // Condition: ControlType == Edit (name matching done per-candidate —
            // UIA name conditions are exact-match only, and we accept several).
            let edit_cond = match auto.CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &VARIANT::from(UIA_EditControlTypeId.0),
            ) {
                Ok(c) => c,
                Err(_) => return AddressBarRead::Unavailable,
            };
            let edits = match root.FindAll(TreeScope_Descendants, &edit_cond) {
                Ok(list) => list,
                Err(_) => return AddressBarRead::Unavailable,
            };
            let count = edits.Length().unwrap_or(0);
            for i in 0..count {
                let Ok(el) = edits.GetElement(i) else { continue };
                let name: String = el
                    .GetCurrentPropertyValue(UIA_NamePropertyId)
                    .ok()
                    .and_then(|v| BSTR::try_from(&v).ok())
                    .map(|b| b.to_string())
                    .unwrap_or_default();
                let matched = hints
                    .address_bar_names
                    .iter()
                    .any(|want| name.eq_ignore_ascii_case(want));
                if !matched {
                    continue;
                }
                let value: String = el
                    .GetCurrentPropertyValue(UIA_ValueValuePropertyId)
                    .ok()
                    .and_then(|v| BSTR::try_from(&v).ok())
                    .map(|b| b.to_string())
                    .unwrap_or_default();
                return if value.trim().is_empty() {
                    AddressBarRead::Empty
                } else {
                    AddressBarRead::Url(normalize_typed_url(value.trim()))
                };
            }
            // RK4: no matching element — do NOT fabricate a URL.
            AddressBarRead::Unavailable
        }
    }
}

#[cfg(windows)]
pub use imp::read_address_bar;

#[cfg(not(windows))]
pub fn read_address_bar(_hwnd: isize, _hints: &AddressBarHints) -> AddressBarRead {
    AddressBarRead::Unavailable
}

/// Browsers display URLs without a scheme ("docs.rs/tokio"); re-add `https://`
/// when missing so downstream URL handling (exclusion `url_pattern`, connectors)
/// sees one canonical shape. A value with spaces is a search phrase, not a URL —
/// returned as-is (the connector's validate() rejects it, ADR-035).
pub fn normalize_typed_url(shown: &str) -> String {
    if shown.contains(' ') || shown.contains("://") {
        return shown.to_string();
    }
    format!("https://{shown}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_process_classification() {
        let hints = AddressBarHints::default();
        assert!(is_browser_process("CHROME.EXE", &hints));
        assert!(is_browser_process("opera_gx.exe", &hints), "Opera GX (ADR-027)");
        assert!(!is_browser_process("code.exe", &hints));
    }

    #[test]
    fn typed_url_normalization() {
        assert_eq!(normalize_typed_url("docs.rs/tokio"), "https://docs.rs/tokio");
        assert_eq!(normalize_typed_url("https://x.test/a"), "https://x.test/a");
        assert_eq!(
            normalize_typed_url("how to exit vim"),
            "how to exit vim",
            "search phrases pass through untouched"
        );
    }
}
