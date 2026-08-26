//! Consequential-action heuristics (Doc 24 #47, #51) and reversibility
//! classification (Doc 24 #54). Pure: the loop calls these *before* dispatching
//! to the executor to decide whether to force the confirmation path and what
//! to promise about undo. The executor itself never blocks on them — policy is
//! the loop's job (Doc 22 §2: Claude decides, local executes).
//!
//! The keyword check runs over the action's **label** (`target`) only. Typed
//! text and key chords are not labels — "please send the report" typed into a
//! chat box is ordinary text; the consequential act is clicking *Send*.

use aperture_contracts::agent::{ActionType, AgentAction};

use crate::grounding::normalize_label;

/// Multi-word phrases, matched first so `"sign out"` reports itself rather
/// than the bare `"sign"`.
const CONSEQUENTIAL_PHRASES: &[&str] = &[
    "shut down",
    "log out",
    "sign out",
    "empty trash",
    "empty bin",
    "empty recycle bin",
];

/// Single words (whole-word match on the normalized label). Decision #47/#51's
/// delete/send/pay/purchase/overwrite family, deliberately conservative:
/// a false positive costs one confirmation chip; a false negative is a sent
/// message. (`"sign"` therefore also flags "Sign in" — accepted.)
const CONSEQUENTIAL_WORDS: &[&str] = &[
    "delete",
    "remove",
    "send",
    "submit",
    "pay",
    "purchase",
    "buy",
    "checkout",
    "order",
    "confirm",
    "transfer",
    "sign",
    "agree",
    "accept",
    "install",
    "uninstall",
    "format",
    "erase",
    "discard",
    "publish",
    "post",
    "unsubscribe",
    "reply",
    "restart",
    "permanently",
];

/// The keyword that makes this action look consequential (decision #47/#51),
/// or `None` for routine actions. Returns the matched word/phrase so the
/// confirmation chip can say *why* ("looks like it may **send**").
pub fn consequential_reason(action: &AgentAction) -> Option<String> {
    let label = normalize_label(action.target.as_deref()?);
    if label.is_empty() {
        return None;
    }
    let words: Vec<&str> = label
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect();
    for phrase in CONSEQUENTIAL_PHRASES {
        let p: Vec<&str> = phrase.split(' ').collect();
        if words.windows(p.len()).any(|w| w == p.as_slice()) {
            return Some((*phrase).to_string());
        }
    }
    words
        .iter()
        .find(|w| CONSEQUENTIAL_WORDS.contains(w))
        .map(|w| (*w).to_string())
}

/// Whether a hard stop could roll this action back (Doc 24 #54). The UI must
/// be explicit about which is which — never promise undo for `Irreversible`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reversibility {
    /// Undo is mechanical: close the window we opened, switch back, scroll back.
    Reversible,
    /// A consequential click/key (decision #51 hit): assume it cannot be undone.
    Irreversible,
    /// Depends on what the target does; typed text may be editable, may not.
    Unknown,
}

/// Classify by verb (decision #54): Launch/SwitchWindow/Scroll/Wait/None are
/// reversible; Type is unknown; Click/Key are irreversible when
/// [`consequential_reason`] fires, otherwise unknown.
pub fn reversibility(action: &AgentAction) -> Reversibility {
    match action.action_type {
        ActionType::Launch
        | ActionType::SwitchWindow
        | ActionType::Scroll
        | ActionType::Wait
        | ActionType::None => Reversibility::Reversible,
        ActionType::Type => Reversibility::Unknown,
        ActionType::Click | ActionType::Key => {
            if consequential_reason(action).is_some() {
                Reversibility::Irreversible
            } else {
                Reversibility::Unknown
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn consequential_labels_are_flagged_with_the_matching_word() {
        let cases = [
            ("Send", "send"),
            ("&Delete permanently", "delete"),
            ("Place order", "order"),
            ("Pay now", "pay"),
            ("Sign out", "sign out"),
            ("Shut down", "shut down"),
            ("Empty Recycle Bin", "empty recycle bin"),
            ("I agree", "agree"),
            ("Confirm purchase", "confirm"),
        ];
        for (label, want) in cases {
            let a = action(ActionType::Click, Some(label), None);
            assert_eq!(consequential_reason(&a).as_deref(), Some(want), "{label}");
        }
    }

    #[test]
    fn routine_labels_and_substrings_are_not_flagged() {
        for label in ["Next", "Cancel", "Sender name", "Posture", "Orders (3)", "Search", "OK"] {
            let a = action(ActionType::Click, Some(label), None);
            assert_eq!(consequential_reason(&a), None, "{label}");
        }
    }

    #[test]
    fn typed_text_and_chords_are_not_labels() {
        // Typing "send" is text, not a Send button.
        let t = action(ActionType::Type, Some("Message"), Some("please send the report"));
        assert_eq!(consequential_reason(&t), None);
        // A chord's value is never scanned; only a label would be.
        let k = action(ActionType::Key, None, Some("Enter"));
        assert_eq!(consequential_reason(&k), None);
        assert_eq!(consequential_reason(&action(ActionType::Click, None, None)), None);
    }

    #[test]
    fn reversibility_follows_decision_54() {
        for ty in [
            ActionType::Launch,
            ActionType::SwitchWindow,
            ActionType::Scroll,
            ActionType::Wait,
            ActionType::None,
        ] {
            assert_eq!(reversibility(&action(ty, Some("Delete"), None)), Reversibility::Reversible, "{ty:?}");
        }
        assert_eq!(reversibility(&action(ActionType::Type, Some("Body"), Some("hi"))), Reversibility::Unknown);
        assert_eq!(reversibility(&action(ActionType::Click, Some("Send"), None)), Reversibility::Irreversible);
        assert_eq!(reversibility(&action(ActionType::Click, Some("Next"), None)), Reversibility::Unknown);
        assert_eq!(reversibility(&action(ActionType::Key, Some("Submit form"), Some("Enter"))), Reversibility::Irreversible);
        assert_eq!(reversibility(&action(ActionType::Key, None, Some("Enter"))), Reversibility::Unknown);
    }
}
