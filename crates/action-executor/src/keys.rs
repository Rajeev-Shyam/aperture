//! Key-chord parsing for [`ActionType::Key`](aperture_contracts::agent::ActionType::Key)
//! (Doc 22 §5 `value`: "text to type or key combo"). Pure: parses Claude's
//! `"Ctrl+Shift+T"` into modifiers + a key and maps keys to Win32 virtual-key
//! codes as plain `u16`s, so the whole table is unit-testable without the
//! `windows` crate. The SendInput side lives in [`crate::platform`].
//!
//! Anything not in the table is [`ActionError::Unsupported`] — the loop
//! surfaces that to Claude rather than the executor guessing a key.

use aperture_contracts::agent::ActionError;

/// Modifier keys, in the order they are pressed (and released in reverse).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Ctrl,
    Shift,
    Alt,
    Win,
}

/// The non-modifier key of a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyName {
    Enter,
    Esc,
    Tab,
    Space,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    /// `F1`..=`F12`.
    F(u8),
    /// An ASCII letter (stored uppercase) or digit.
    Char(char),
}

/// A parsed chord: zero or more modifiers plus exactly one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    pub modifiers: Vec<Modifier>,
    pub key: KeyName,
}

/// Parse `"Ctrl+Shift+T"`, `"Alt+F4"`, `"Enter"`, `"Win+R"`, `"Esc"`, …
/// Tokens are `+`-separated, case-insensitive, whitespace-tolerant; every token
/// but the last must be a modifier. Unknown tokens ⇒ `Unsupported`.
pub fn parse_chord(raw: &str) -> Result<Chord, ActionError> {
    let unsupported = || ActionError::Unsupported(format!("key chord {raw:?}"));
    let tokens: Vec<&str> = raw.split('+').map(str::trim).collect();
    let (key_tok, mod_toks) = tokens.split_last().ok_or_else(unsupported)?;
    let mut modifiers = Vec::with_capacity(mod_toks.len());
    for tok in mod_toks {
        let m = parse_modifier(tok).ok_or_else(unsupported)?;
        if !modifiers.contains(&m) {
            modifiers.push(m);
        }
    }
    let key = parse_key(key_tok).ok_or_else(unsupported)?;
    Ok(Chord { modifiers, key })
}

fn parse_modifier(tok: &str) -> Option<Modifier> {
    match tok.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Some(Modifier::Ctrl),
        "shift" => Some(Modifier::Shift),
        "alt" => Some(Modifier::Alt),
        "win" | "windows" | "meta" | "super" | "cmd" => Some(Modifier::Win),
        _ => None,
    }
}

fn parse_key(tok: &str) -> Option<KeyName> {
    let lower = tok.to_ascii_lowercase();
    let lower = lower.split_whitespace().collect::<String>(); // "page up" → "pageup"
    let key = match lower.as_str() {
        "enter" | "return" => KeyName::Enter,
        "esc" | "escape" => KeyName::Esc,
        "tab" => KeyName::Tab,
        "space" | "spacebar" => KeyName::Space,
        "backspace" | "back" | "bksp" => KeyName::Backspace,
        "delete" | "del" => KeyName::Delete,
        "insert" | "ins" => KeyName::Insert,
        "home" => KeyName::Home,
        "end" => KeyName::End,
        "pageup" | "pgup" => KeyName::PageUp,
        "pagedown" | "pgdn" | "pgdown" => KeyName::PageDown,
        "up" | "uparrow" | "arrowup" => KeyName::Up,
        "down" | "downarrow" | "arrowdown" => KeyName::Down,
        "left" | "leftarrow" | "arrowleft" => KeyName::Left,
        "right" | "rightarrow" | "arrowright" => KeyName::Right,
        _ => {
            if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                if (1..=12).contains(&n) {
                    return Some(KeyName::F(n));
                }
                return None;
            }
            let mut chars = lower.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphanumeric() => {
                    KeyName::Char(c.to_ascii_uppercase())
                }
                _ => return None,
            }
        }
    };
    Some(key)
}

/// Win32 virtual-key code (`VK_*`) for a key. Values are the documented
/// constants (winuser.h); asserted against the `windows` crate's in tests.
pub fn virtual_key(key: KeyName) -> u16 {
    match key {
        KeyName::Enter => 0x0D,
        KeyName::Esc => 0x1B,
        KeyName::Tab => 0x09,
        KeyName::Space => 0x20,
        KeyName::Backspace => 0x08,
        KeyName::Delete => 0x2E,
        KeyName::Insert => 0x2D,
        KeyName::Home => 0x24,
        KeyName::End => 0x23,
        KeyName::PageUp => 0x21,
        KeyName::PageDown => 0x22,
        KeyName::Up => 0x26,
        KeyName::Down => 0x28,
        KeyName::Left => 0x25,
        KeyName::Right => 0x27,
        KeyName::F(n) => 0x70 + u16::from(n.clamp(1, 12)) - 1,
        // Letters and digits: VK == ASCII code of the uppercase form.
        KeyName::Char(c) => c.to_ascii_uppercase() as u16,
    }
}

/// Win32 virtual-key code for a modifier (the generic, not left/right, code).
pub fn modifier_virtual_key(m: Modifier) -> u16 {
    match m {
        Modifier::Ctrl => 0x11,  // VK_CONTROL
        Modifier::Shift => 0x10, // VK_SHIFT
        Modifier::Alt => 0x12,   // VK_MENU
        Modifier::Win => 0x5B,   // VK_LWIN
    }
}

/// Keys on the extended keypad (navigation cluster) need
/// `KEYEVENTF_EXTENDEDKEY` or some apps read them as numpad keys.
pub fn is_extended(key: KeyName) -> bool {
    matches!(
        key,
        KeyName::Insert
            | KeyName::Delete
            | KeyName::Home
            | KeyName::End
            | KeyName::PageUp
            | KeyName::PageDown
            | KeyName::Up
            | KeyName::Down
            | KeyName::Left
            | KeyName::Right
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_documented_chords() {
        assert_eq!(
            parse_chord("Ctrl+Shift+T").unwrap(),
            Chord { modifiers: vec![Modifier::Ctrl, Modifier::Shift], key: KeyName::Char('T') }
        );
        assert_eq!(
            parse_chord("Alt+F4").unwrap(),
            Chord { modifiers: vec![Modifier::Alt], key: KeyName::F(4) }
        );
        assert_eq!(parse_chord("Enter").unwrap(), Chord { modifiers: vec![], key: KeyName::Enter });
        assert_eq!(
            parse_chord("Win+R").unwrap(),
            Chord { modifiers: vec![Modifier::Win], key: KeyName::Char('R') }
        );
        assert_eq!(parse_chord("Esc").unwrap().key, KeyName::Esc);
        assert_eq!(parse_chord(" ctrl + a ").unwrap().key, KeyName::Char('A'));
        assert_eq!(parse_chord("Page Down").unwrap().key, KeyName::PageDown);
        assert_eq!(parse_chord("Ctrl+0").unwrap().key, KeyName::Char('0'));
        assert_eq!(parse_chord("F12").unwrap().key, KeyName::F(12));
        assert_eq!(parse_chord("Ctrl+Ctrl+C").unwrap().modifiers, vec![Modifier::Ctrl]);
    }

    #[test]
    fn unknown_tokens_are_unsupported() {
        for bad in ["", "Ctrl+", "F13", "Hyper+X", "Ctrl+Shift", "PrintScreen", "ab", "é"] {
            assert!(
                matches!(parse_chord(bad), Err(ActionError::Unsupported(_))),
                "{bad:?} should be Unsupported"
            );
        }
    }

    #[test]
    fn virtual_key_table_matches_winuser_constants() {
        assert_eq!(virtual_key(KeyName::Enter), 13);
        assert_eq!(virtual_key(KeyName::Esc), 27);
        assert_eq!(virtual_key(KeyName::Tab), 9);
        assert_eq!(virtual_key(KeyName::Backspace), 8);
        assert_eq!(virtual_key(KeyName::Delete), 46);
        assert_eq!(virtual_key(KeyName::PageUp), 33);
        assert_eq!(virtual_key(KeyName::PageDown), 34);
        assert_eq!(virtual_key(KeyName::Home), 36);
        assert_eq!(virtual_key(KeyName::End), 35);
        assert_eq!(virtual_key(KeyName::Left), 37);
        assert_eq!(virtual_key(KeyName::Right), 39);
        assert_eq!(virtual_key(KeyName::F(1)), 112);
        assert_eq!(virtual_key(KeyName::F(12)), 123);
        assert_eq!(virtual_key(KeyName::Char('a')), 65);
        assert_eq!(virtual_key(KeyName::Char('9')), 57);
        assert_eq!(modifier_virtual_key(Modifier::Ctrl), 17);
        assert_eq!(modifier_virtual_key(Modifier::Shift), 16);
        assert_eq!(modifier_virtual_key(Modifier::Alt), 18);
        assert_eq!(modifier_virtual_key(Modifier::Win), 91);
        assert!(is_extended(KeyName::Down));
        assert!(!is_extended(KeyName::Enter));
    }

    /// On Windows, cross-check the hand-written table against the real crate
    /// constants so a typo can never reach SendInput.
    #[cfg(windows)]
    #[test]
    fn virtual_key_table_matches_the_windows_crate() {
        use windows::Win32::UI::Input::KeyboardAndMouse as k;
        let pairs = [
            (KeyName::Enter, k::VK_RETURN),
            (KeyName::Esc, k::VK_ESCAPE),
            (KeyName::Tab, k::VK_TAB),
            (KeyName::Space, k::VK_SPACE),
            (KeyName::Backspace, k::VK_BACK),
            (KeyName::Delete, k::VK_DELETE),
            (KeyName::Insert, k::VK_INSERT),
            (KeyName::Home, k::VK_HOME),
            (KeyName::End, k::VK_END),
            (KeyName::PageUp, k::VK_PRIOR),
            (KeyName::PageDown, k::VK_NEXT),
            (KeyName::Up, k::VK_UP),
            (KeyName::Down, k::VK_DOWN),
            (KeyName::Left, k::VK_LEFT),
            (KeyName::Right, k::VK_RIGHT),
            (KeyName::F(1), k::VK_F1),
            (KeyName::F(12), k::VK_F12),
            (KeyName::Char('A'), k::VK_A),
            (KeyName::Char('Z'), k::VK_Z),
            (KeyName::Char('0'), k::VK_0),
            (KeyName::Char('9'), k::VK_9),
        ];
        for (key, vk) in pairs {
            assert_eq!(virtual_key(key), vk.0, "{key:?}");
        }
        assert_eq!(modifier_virtual_key(Modifier::Ctrl), k::VK_CONTROL.0);
        assert_eq!(modifier_virtual_key(Modifier::Shift), k::VK_SHIFT.0);
        assert_eq!(modifier_virtual_key(Modifier::Alt), k::VK_MENU.0);
        assert_eq!(modifier_virtual_key(Modifier::Win), k::VK_LWIN.0);
    }
}
