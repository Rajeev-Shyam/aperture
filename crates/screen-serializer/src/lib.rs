//! Screen-state → per-step payload (Doc 22 §3.2) — **v2 SKELETON**.
//!
//! Converts the current screen into the structured [`StepPayload`] the agent
//! loop stages for Claude. The privacy ordering is non-negotiable and encoded
//! in [`build_step_payload`]'s signature: the redactor runs over every text
//! field BEFORE the payload exists — a caller cannot assemble an unredacted
//! payload through this crate.
//!
//! What is REAL here: the schema, the redaction pass, the payload hash (the
//! audit datum locked decision 6 requires), and the serialization contract.
//! What is V2-M1: wiring the live capture/OCR feed and the 768 px screenshot
//! downscale (v1's `vision-ocr` `prepare_image` path composes in); Q-V2-02
//! (does a base64 768 px screenshot fit MCP payload limits?) is answered by
//! measurement there, not assumed.

use aperture_privacy::redaction::Redactor;
use serde::{Deserialize, Serialize};

/// The focused window's identity (Doc 22 §3.2). `url` only for browsers with
/// the extension feed live; None otherwise.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FocusedWindow {
    pub app: String,
    pub title: String,
    #[serde(default)]
    pub url: Option<String>,
}

/// The previous step's action + result, echoed so Claude sees what happened
/// (Doc 22 §3.2 `last_action`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastAction {
    #[serde(rename = "type")]
    pub action_type: String,
    pub target: String,
    pub result: String,
}

/// The per-step payload Claude receives (Doc 22 §3.2), post-redaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepPayload {
    /// The user's stated task, verbatim.
    pub task: String,
    pub step_number: u32,
    /// 768 px-downscaled JPEG, base64 — None until V2-M1 wires capture
    /// (Q-V2-02 measures whether it fits the transport).
    #[serde(default)]
    pub screenshot_b64: Option<String>,
    /// Redacted OCR text of the screen.
    pub ocr_text: String,
    pub focused_window: FocusedWindow,
    #[serde(default)]
    pub last_action: Option<LastAction>,
    /// Open top-level windows by app name (metadata only).
    #[serde(default)]
    pub open_windows: Vec<String>,
    /// Claude's own rolling summary from the previous step (Doc 22 §3.2 —
    /// context stays bounded on long tasks; Q-V2-06 revisits the strategy).
    #[serde(default)]
    pub prior_steps_summary: Option<String>,
}

/// Raw observation handed in by the capture side — UNREDACTED. Only this
/// crate turns it into a [`StepPayload`], and only through the redactor.
#[derive(Debug, Clone)]
pub struct RawObservation {
    pub ocr_text: String,
    pub focused_window: FocusedWindow,
    pub open_windows: Vec<String>,
}

/// Build one step's payload: redact every text surface, then assemble.
/// Returns the payload plus the SHA-256 of its canonical serialization — the
/// hash `task_steps.screen_payload_hash` records (locked decision 6).
pub fn build_step_payload(
    task: &str,
    step_number: u32,
    observation: RawObservation,
    last_action: Option<LastAction>,
    prior_steps_summary: Option<String>,
    redactor: &Redactor,
) -> (StepPayload, String) {
    // Redaction BEFORE assembly (Doc 22 §8: runs on every payload). Titles and
    // window names are text surfaces too — a password manager's window title
    // can carry the secret it guards.
    let (ocr_text, _) = redactor.redact_text(&observation.ocr_text);
    let (title, _) = redactor.redact_text(&observation.focused_window.title);
    let payload = StepPayload {
        task: task.to_string(),
        step_number,
        screenshot_b64: None, // V2-M1: capture + 768 px downscale + Q-V2-02
        ocr_text,
        focused_window: FocusedWindow {
            app: observation.focused_window.app,
            title,
            url: observation.focused_window.url,
        },
        last_action,
        open_windows: observation.open_windows,
        prior_steps_summary,
    };
    let wire = serde_json::to_vec(&payload).unwrap_or_default();
    (payload, sha256_hex(&wire))
}

/// Lowercase-hex SHA-256 (same convention as the v1 audit log, doc 13 §3).
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor() -> Redactor {
        Redactor::new(&[]).expect("built-in rules compile")
    }

    #[test]
    fn secrets_never_reach_the_assembled_payload() {
        let (payload, _) = build_step_payload(
            "file my expenses",
            3,
            RawObservation {
                ocr_text: "api key sk-abcdefghijklmnop1234 visible on screen".into(),
                focused_window: FocusedWindow {
                    app: "Chrome".into(),
                    title: "Console — sk-abcdefghijklmnop1234".into(),
                    url: None,
                },
                open_windows: vec!["Chrome".into(), "VSCode".into()],
            },
            None,
            None,
            &redactor(),
        );
        assert!(!payload.ocr_text.contains("sk-abcdefghijklmnop1234"), "ocr redacted");
        assert!(
            !payload.focused_window.title.contains("sk-abcdefghijklmnop1234"),
            "window title redacted too"
        );
    }

    #[test]
    fn payload_hash_is_stable_over_the_exact_serialization() {
        let obs = || RawObservation {
            ocr_text: "hello".into(),
            focused_window: FocusedWindow { app: "a".into(), title: "t".into(), url: None },
            open_windows: vec![],
        };
        let (_, h1) = build_step_payload("task", 1, obs(), None, None, &redactor());
        let (_, h2) = build_step_payload("task", 1, obs(), None, None, &redactor());
        assert_eq!(h1, h2, "same observation ⇒ same audit hash");
        let (_, h3) = build_step_payload("task", 2, obs(), None, None, &redactor());
        assert_ne!(h1, h3, "any field change ⇒ different hash");
    }

    /// The Doc 22 §3.2 field names are the wire contract for the system prompt.
    #[test]
    fn wire_field_names_match_doc22() {
        let (payload, _) = build_step_payload(
            "t",
            1,
            RawObservation {
                ocr_text: "x".into(),
                focused_window: FocusedWindow::default(),
                open_windows: vec![],
            },
            Some(LastAction {
                action_type: "click".into(),
                target: "Submit button".into(),
                result: "success".into(),
            }),
            Some("did things".into()),
            &redactor(),
        );
        let v = serde_json::to_value(&payload).unwrap();
        for key in [
            "task",
            "step_number",
            "ocr_text",
            "focused_window",
            "last_action",
            "open_windows",
            "prior_steps_summary",
        ] {
            assert!(v.get(key).is_some(), "missing wire key {key}");
        }
        assert_eq!(v["last_action"]["type"], "click");
    }
}
