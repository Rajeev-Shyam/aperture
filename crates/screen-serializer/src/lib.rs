//! Screen-state → per-step payload (Doc 22 §3.2) — **v2 SKELETON**.
//!
//! Converts the current screen into the structured [`StepPayload`] the agent
//! loop stages for Claude. The privacy ordering is non-negotiable and encoded
//! in [`build_step_payload`]'s signature: the redactor runs over every text
//! field BEFORE the payload exists — a caller cannot assemble an unredacted
//! payload through this crate.
//!
//! What is REAL here: the schema, the redaction pass, the payload hash (the
//! audit datum locked decision 6 requires), the serialization contract, and —
//! since V2-M1 (2026-08-22) — the screenshot leg in [`screenshot`]: OCR with
//! word boxes → image redaction at OCR scale → 768 px JPEG → base64. Q-V2-02
//! (does a base64 768 px screenshot fit the MCP result cap?) is measured by
//! `screenshot::tests::q_v2_02_…` against the 1 MiB cap, not assumed.
//!
//! The live capture feed is `aperture_capture::CaptureSubsystem::observe_now`
//! (same exclusion gate as a scheduled sample); the shell composes the two.

pub mod screenshot;

use aperture_privacy::redaction::Redactor;
use serde::{Deserialize, Serialize};

pub use screenshot::{observe_frame, ObservedScreen, RedactedScreenshot, ScreenshotError};

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
    /// 768 px-downscaled, REDACTED JPEG, base64 (Doc 22 §3.2; the redaction
    /// gate is `screenshot::observe_frame`). `None` when the observation
    /// carried no frame (capture off / event-only context).
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

/// Raw observation handed in by the capture side — text UNREDACTED. Only this
/// crate turns it into a [`StepPayload`], and only through the redactor.
///
/// `screenshot` is the exception that proves the rule: it can only be built
/// by [`screenshot::observe_frame`], which has already painted over every
/// word the text rules matched — there is no constructor for an unredacted
/// payload screenshot in this crate.
#[derive(Debug, Clone)]
pub struct RawObservation {
    pub ocr_text: String,
    pub focused_window: FocusedWindow,
    pub open_windows: Vec<String>,
    pub screenshot: Option<RedactedScreenshot>,
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
    // Redaction BEFORE assembly (Doc 22 §8: runs on every payload). Every
    // text field is a surface (08-22 review): titles and window names can
    // carry the secret a password manager guards; a browser URL can carry a
    // reset token, JWT or email in its query string; `last_action` is
    // executor text built from screen content; `prior_steps_summary` carries
    // the user's clarification answer ("User answered: …"). `open_windows`
    // is app names only since 08-22, but redacts anyway — defense in depth.
    // Redaction is idempotent (placeholders match no rule), so a field that
    // was already scrubbed upstream passes through unchanged.
    let (ocr_text, _) = redactor.redact_text(&observation.ocr_text);
    let (title, _) = redactor.redact_text(&observation.focused_window.title);
    let url = observation
        .focused_window
        .url
        .map(|u| redactor.redact_text(&u).0);
    let last_action = last_action.map(|a| LastAction {
        action_type: a.action_type,
        target: redactor.redact_text(&a.target).0,
        result: redactor.redact_text(&a.result).0,
    });
    let open_windows = observation
        .open_windows
        .iter()
        .map(|w| redactor.redact_text(w).0)
        .collect();
    let prior_steps_summary = prior_steps_summary.map(|s| redactor.redact_text(&s).0);
    let payload = StepPayload {
        task: task.to_string(),
        step_number,
        screenshot_b64: observation
            .screenshot
            .as_ref()
            .map(|s| screenshot::to_base64(&s.jpeg)),
        ocr_text,
        focused_window: FocusedWindow {
            app: observation.focused_window.app,
            title,
            url,
        },
        last_action,
        open_windows,
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
                screenshot: None,
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
            screenshot: None,
        };
        let (_, h1) = build_step_payload("task", 1, obs(), None, None, &redactor());
        let (_, h2) = build_step_payload("task", 1, obs(), None, None, &redactor());
        assert_eq!(h1, h2, "same observation ⇒ same audit hash");
        let (_, h3) = build_step_payload("task", 2, obs(), None, None, &redactor());
        assert_ne!(h1, h3, "any field change ⇒ different hash");
    }

    /// Every non-OCR text surface is redacted too (08-22 review): the URL's
    /// query string, the executor-built `last_action`, `open_windows`, and
    /// the prior-steps summary carrying the user's clarification answer.
    #[test]
    fn url_last_action_and_summary_are_redacted() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6y";
        let (payload, _) = build_step_payload(
            "reset my password",
            2,
            RawObservation {
                ocr_text: "plain".into(),
                focused_window: FocusedWindow {
                    app: "Chrome".into(),
                    title: "Reset".into(),
                    url: Some(format!(
                        "https://app.example.com/reset?token={jwt}&email=john.doe@corp.com"
                    )),
                },
                open_windows: vec!["Chrome".into(), "Mail — jane.roe@corp.com".into()],
                screenshot: None,
            },
            Some(LastAction {
                action_type: "type".into(),
                target: "Email field — john.doe@corp.com".into(),
                result: "typed into field showing john.doe@corp.com".into(),
            }),
            Some("Asked which key to use. User answered: my key is sk-abcdefghijklmnop1234".into()),
            &redactor(),
        );
        let url = payload.focused_window.url.as_deref().unwrap();
        assert!(!url.contains(jwt), "JWT in query string redacted");
        assert!(!url.contains("john.doe@corp.com"), "email in query string redacted");
        let action = payload.last_action.as_ref().unwrap();
        assert!(!action.target.contains("john.doe@corp.com"), "last_action.target redacted");
        assert!(!action.result.contains("john.doe@corp.com"), "last_action.result redacted");
        assert!(
            !payload.open_windows.iter().any(|w| w.contains("jane.roe@corp.com")),
            "open_windows entries redacted"
        );
        let summary = payload.prior_steps_summary.as_deref().unwrap();
        assert!(
            !summary.contains("sk-abcdefghijklmnop1234"),
            "clarification answer in prior_steps_summary redacted"
        );
        assert!(summary.contains("User answered:"), "harmless summary text kept");
    }

    /// The audit hash is over the REDACTED serialization, deterministically:
    /// two builds from the same raw input agree, and feeding the redacted
    /// fields back through produces the identical payload (idempotence — a
    /// placeholder matches no rule, so nothing double-scrubs).
    #[test]
    fn hash_covers_the_redacted_form_and_redaction_is_idempotent() {
        let obs = || RawObservation {
            ocr_text: "contact john.doe@corp.com".into(),
            focused_window: FocusedWindow {
                app: "Chrome".into(),
                title: "Inbox".into(),
                url: Some("https://mail.example.com/?email=john.doe@corp.com".into()),
            },
            open_windows: vec!["Chrome".into()],
            screenshot: None,
        };
        let action = || Some(LastAction {
            action_type: "click".into(),
            target: "row john.doe@corp.com".into(),
            result: "ok".into(),
        });
        let summary = || Some("User answered: use sk-abcdefghijklmnop1234".into());
        let (p1, h1) = build_step_payload("t", 1, obs(), action(), summary(), &redactor());
        let (_, h2) = build_step_payload("t", 1, obs(), action(), summary(), &redactor());
        assert_eq!(h1, h2, "same raw input ⇒ same audit hash over the redacted form");

        // Rebuild from the already-redacted payload: everything is a no-op.
        let (p3, _) = build_step_payload(
            "t",
            1,
            RawObservation {
                ocr_text: p1.ocr_text.clone(),
                focused_window: p1.focused_window.clone(),
                open_windows: p1.open_windows.clone(),
                screenshot: None,
            },
            p1.last_action.clone(),
            p1.prior_steps_summary.clone(),
            &redactor(),
        );
        assert_eq!(p3.ocr_text, p1.ocr_text);
        assert_eq!(p3.focused_window.url, p1.focused_window.url);
        assert_eq!(
            p3.last_action.as_ref().unwrap().target,
            p1.last_action.as_ref().unwrap().target
        );
        assert_eq!(p3.prior_steps_summary, p1.prior_steps_summary);
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
                screenshot: None,
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
