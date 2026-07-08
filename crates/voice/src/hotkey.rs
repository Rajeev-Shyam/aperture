//! Global push-to-talk hotkey (doc 07 §2).
//!
//! `RegisterHotKey`-backed global hotkey with **press-and-hold** semantics:
//! key-down starts capture (shell shows the "listening" pill, doc 11), key-up
//! stops it. Default chord `Ctrl+Win+Space` [ASSUMPTION, doc 07 §2], configurable
//! from settings. A registration failure means a conflict with another app; the
//! shell surfaces a rebind prompt (doc 07 §6).
//!
//! The 30 s max-utterance ceiling ([`crate::MAX_UTTERANCE`]) is enforced by the
//! capture layer, not here — this module only translates raw key transitions into
//! [`PttEvent`]s.
//!
//! TODO(M6:) implement on `global-hotkey` (Win32 `RegisterHotKey`). [VERIFY] that
//! crate's press/release event surface — some versions only emit key-down, in
//! which case we pair it with a raw key-up watch (WH_KEYBOARD_LL) for hold release.

use std::time::Duration;

/// Default PTT chord (doc 07 §2) — `Ctrl + Win + Space` [ASSUMPTION].
pub const DEFAULT_HOTKEY: &str = "Ctrl+Win+Space";

/// Max hold before capture auto-stops regardless of key state (doc 07 §2).
pub const MAX_HOLD: Duration = crate::MAX_UTTERANCE;

/// A parsed hotkey chord (modifiers + key). Held in settings as a string and
/// parsed into the `global-hotkey` representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyChord {
    /// Human-readable chord, e.g. `"Ctrl+Win+Space"`.
    pub spec: String,
}

impl Default for HotkeyChord {
    fn default() -> Self {
        Self {
            spec: DEFAULT_HOTKEY.to_string(),
        }
    }
}

/// Press-and-hold transitions the subsystem reacts to (doc 07 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PttEvent {
    /// Chord pressed — begin capture.
    Down,
    /// Chord released — end capture and run the pipeline.
    Up,
}

/// Errors registering the global hotkey.
#[derive(Debug, thiserror::Error)]
pub enum HotkeyError {
    /// Another app already owns the chord (doc 07 §6) — surface a rebind prompt.
    #[error("hotkey '{0}' is already in use by another application")]
    Conflict(String),
    /// The chord string could not be parsed.
    #[error("invalid hotkey chord: {0}")]
    Parse(String),
}

/// Owns the live `RegisterHotKey` registration. Dropping it (capture toggle OFF,
/// doc 12 §6) unregisters the chord so PTT goes inert.
pub struct PttHotkey {
    _chord: HotkeyChord,
    // manager: global_hotkey::GlobalHotKeyManager,  // holds the registration alive
    // hotkey_id: u32,
}

impl PttHotkey {
    /// Register the global PTT hotkey. On `Conflict` the shell prompts to rebind
    /// (doc 07 §6).
    pub fn register(_chord: HotkeyChord) -> Result<Self, HotkeyError> {
        // TODO(M6:) parse chord -> global_hotkey::hotkey::HotKey;
        //           GlobalHotKeyManager::new()?.register(hk) — map AlreadyRegistered
        //           / OS error to HotkeyError::Conflict.
        todo!("M6: RegisterHotKey via global-hotkey; map conflicts to a rebind prompt")
    }

    /// Block until the next press/release transition. The subsystem loop maps
    /// [`PttEvent::Down`] -> `ptt_down()` and [`PttEvent::Up`] -> `ptt_up()`.
    pub fn next_event(&mut self) -> PttEvent {
        // TODO(M6:) recv from GlobalHotKeyEvent::receiver(); coalesce key-repeat so a
        //           held key yields exactly one Down then one Up.
        todo!("M6: receive next PTT key transition")
    }
}
