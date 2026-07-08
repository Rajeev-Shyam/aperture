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
//! ## Verified vs on-hardware
//! [`HotkeyChord::parse`] (chord string → modifiers + key) is **pure + tested**.
//! [`PttHotkey`] drives `global-hotkey` 0.6 and is **UNVERIFIED**: it compiles but
//! is not exercised in CI. [VERIFY on the box]: global-hotkey needs a running
//! Win32 message pump on its thread to receive `WM_HOTKEY`, and press-and-hold
//! relies on it emitting BOTH `HotKeyState::Pressed` and `Released` (0.6 does).

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
        Self { spec: DEFAULT_HOTKEY.to_string() }
    }
}

/// The chord decomposed into modifier flags + a single non-modifier key name —
/// the pure, backend-agnostic parse result (verified independently of the OS).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedChord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// The Windows / Super key.
    pub meta: bool,
    /// The non-modifier key, lowercased (e.g. `"space"`, `"a"`, `"f1"`).
    pub key: String,
}

impl HotkeyChord {
    /// Parse `spec` (`"Ctrl+Win+Space"`) into modifier flags + one key. Pure — no
    /// OS calls — so the settings round-trip is exhaustively testable. Rejects an
    /// empty segment, a missing key, or more than one non-modifier key.
    pub fn parse(&self) -> Result<ParsedChord, HotkeyError> {
        let mut c = ParsedChord::default();
        for part in self.spec.split('+') {
            let p = part.trim().to_ascii_lowercase();
            match p.as_str() {
                "ctrl" | "control" => c.ctrl = true,
                "alt" | "option" => c.alt = true,
                "shift" => c.shift = true,
                "win" | "super" | "meta" | "cmd" | "command" => c.meta = true,
                "" => return Err(HotkeyError::Parse(format!("'{}': empty chord segment", self.spec))),
                key => {
                    if !c.key.is_empty() {
                        return Err(HotkeyError::Parse(format!(
                            "'{}': more than one non-modifier key",
                            self.spec
                        )));
                    }
                    c.key = key.to_string();
                }
            }
        }
        if c.key.is_empty() {
            return Err(HotkeyError::Parse(format!("'{}': no non-modifier key", self.spec)));
        }
        Ok(c)
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
///
/// **UNVERIFIED (on-hardware):** wraps `global_hotkey::GlobalHotKeyManager`.
pub struct PttHotkey {
    _chord: HotkeyChord,
    manager: global_hotkey::GlobalHotKeyManager,
    hotkey_id: u32,
}

impl PttHotkey {
    /// Register the global PTT hotkey. On `Conflict` the shell prompts to rebind
    /// (doc 07 §6). A parse error is surfaced as [`HotkeyError::Parse`].
    pub fn register(chord: HotkeyChord) -> Result<Self, HotkeyError> {
        use global_hotkey::hotkey::{HotKey, Modifiers};
        use global_hotkey::GlobalHotKeyManager;

        let parsed = chord.parse()?;
        let mut mods = Modifiers::empty();
        if parsed.ctrl {
            mods |= Modifiers::CONTROL;
        }
        if parsed.alt {
            mods |= Modifiers::ALT;
        }
        if parsed.shift {
            mods |= Modifiers::SHIFT;
        }
        if parsed.meta {
            mods |= Modifiers::META;
        }
        let code = key_to_code(&parsed.key)
            .ok_or_else(|| HotkeyError::Parse(format!("unsupported key '{}'", parsed.key)))?;
        let hotkey = HotKey::new((!mods.is_empty()).then_some(mods), code);
        let hotkey_id = hotkey.id();

        // Both `new()` failure and `register()` conflict map to a rebind prompt.
        let manager =
            GlobalHotKeyManager::new().map_err(|e| HotkeyError::Conflict(e.to_string()))?;
        manager
            .register(hotkey)
            .map_err(|e| HotkeyError::Conflict(e.to_string()))?;
        Ok(Self { _chord: chord, manager, hotkey_id })
    }

    /// Block until the next press/release transition for *our* chord, mapped to a
    /// [`PttEvent`]. The subsystem loop maps `Down` → `ptt_down()` and `Up` →
    /// `ptt_up()`. Events for other hotkeys are ignored.
    pub fn next_event(&mut self) -> PttEvent {
        use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
        let rx = GlobalHotKeyEvent::receiver();
        loop {
            match rx.recv() {
                Ok(ev) if ev.id == self.hotkey_id => {
                    return match ev.state {
                        HotKeyState::Pressed => PttEvent::Down,
                        HotKeyState::Released => PttEvent::Up,
                    };
                }
                Ok(_) => continue, // a different hotkey — ignore
                // Sender gone (manager dropped): report Up so any in-flight
                // capture is finalized rather than hung.
                Err(_) => return PttEvent::Up,
            }
        }
    }

    /// Explicitly unregister (also happens on drop via the manager). Idempotent.
    pub fn manager(&self) -> &global_hotkey::GlobalHotKeyManager {
        &self.manager
    }
}

/// Map a lowercased key name to a `global_hotkey` key code (best-effort; the
/// common PTT keys). Returns `None` for an unsupported key.
fn key_to_code(key: &str) -> Option<global_hotkey::hotkey::Code> {
    use global_hotkey::hotkey::Code;
    // Single letters a–z and digits 0–9.
    if key.len() == 1 {
        let ch = key.chars().next().unwrap();
        return match ch {
            'a'..='z' => Some(match ch {
                'a' => Code::KeyA, 'b' => Code::KeyB, 'c' => Code::KeyC, 'd' => Code::KeyD,
                'e' => Code::KeyE, 'f' => Code::KeyF, 'g' => Code::KeyG, 'h' => Code::KeyH,
                'i' => Code::KeyI, 'j' => Code::KeyJ, 'k' => Code::KeyK, 'l' => Code::KeyL,
                'm' => Code::KeyM, 'n' => Code::KeyN, 'o' => Code::KeyO, 'p' => Code::KeyP,
                'q' => Code::KeyQ, 'r' => Code::KeyR, 's' => Code::KeyS, 't' => Code::KeyT,
                'u' => Code::KeyU, 'v' => Code::KeyV, 'w' => Code::KeyW, 'x' => Code::KeyX,
                'y' => Code::KeyY, _ => Code::KeyZ,
            }),
            '0' => Some(Code::Digit0), '1' => Some(Code::Digit1), '2' => Some(Code::Digit2),
            '3' => Some(Code::Digit3), '4' => Some(Code::Digit4), '5' => Some(Code::Digit5),
            '6' => Some(Code::Digit6), '7' => Some(Code::Digit7), '8' => Some(Code::Digit8),
            '9' => Some(Code::Digit9),
            _ => None,
        };
    }
    match key {
        "space" => Some(Code::Space),
        "enter" | "return" => Some(Code::Enter),
        "tab" => Some(Code::Tab),
        "f1" => Some(Code::F1), "f2" => Some(Code::F2), "f3" => Some(Code::F3),
        "f4" => Some(Code::F4), "f5" => Some(Code::F5), "f6" => Some(Code::F6),
        "f7" => Some(Code::F7), "f8" => Some(Code::F8), "f9" => Some(Code::F9),
        "f10" => Some(Code::F10), "f11" => Some(Code::F11), "f12" => Some(Code::F12),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_default_chord() {
        let c = HotkeyChord::default().parse().unwrap();
        assert!(c.ctrl && c.meta && !c.alt && !c.shift);
        assert_eq!(c.key, "space");
    }

    #[test]
    fn parse_is_case_and_whitespace_insensitive() {
        let c = HotkeyChord { spec: " control + SHIFT + A ".into() }.parse().unwrap();
        assert!(c.ctrl && c.shift);
        assert_eq!(c.key, "a");
    }

    #[test]
    fn rejects_no_key_empty_segment_and_double_key() {
        assert!(HotkeyChord { spec: "Ctrl+Shift".into() }.parse().is_err());
        assert!(HotkeyChord { spec: "Ctrl+".into() }.parse().is_err());
        assert!(HotkeyChord { spec: "A+B".into() }.parse().is_err());
    }

    #[test]
    fn key_codes_map_for_common_ptt_keys() {
        assert!(key_to_code("space").is_some());
        assert!(key_to_code("f8").is_some());
        assert!(key_to_code("k").is_some());
        assert!(key_to_code("nope").is_none());
    }
}
