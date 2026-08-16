//! The three swappable transports (doc 09 §3) — and the push/pull asymmetry
//! that shapes the UX.
//!
//! All three implement [`aperture_contracts::ReasoningTransport`] and live
//! **only** inside this crate (the two-emitter rule, doc 13 §2): they are the
//! sole code paths that open a socket or spawn the Claude CLI.
//!
//! | Transport | Direction | Gate placement (doc 09 §3) |
//! |---|---|---|
//! | [`cli`] (Claude Code CLI) | **Push** — Aperture initiates | Preview shown *before* spawning; approved bytes form the prompt. |
//! | [`mcp`] (Claude Desktop via MCP) | **Pull** — Claude initiates | Gate lives *inside* the `aperture_get_context` tool handler, which blocks on the user's Send. |
//! | [`api`] (Messages API) | **Push** | Preview *before* the HTTPS call. |
//!
//! Default fall-through order: Desktop-MCP -> CLI -> API (MCP-primary,
//! ADR-025), user-reorderable (doc 09 §3). Health failures fall through with a
//! visible notice; offline means the local answer stands and nothing queues
//! silently (doc 09 §6).

pub mod api;
pub mod cli;
pub mod mcp;

use aperture_contracts::{ContextPayload, PayloadItem, TransportError, TransportId};

/// Human name for errors that must say WHICH transport refused (decision #42).
pub fn transport_name(id: TransportId) -> &'static str {
    match id {
        TransportId::ClaudeCli => "Claude CLI",
        TransportId::ClaudeDesktopMcp => "Claude Desktop (MCP)",
        TransportId::MessagesApi => "Messages API",
    }
}

/// The per-transport HARD wire-byte cap (decision #42) — a distinct mechanism
/// from the 50 KB soft preview warning (doc 09 §5). Each constant documents its
/// source where it is defined, next to the transport it bounds.
pub fn hard_cap_bytes(id: TransportId) -> usize {
    match id {
        TransportId::ClaudeCli => cli::CLI_STDIN_MAX_BYTES,
        TransportId::ClaudeDesktopMcp => mcp::MCP_RESULT_MAX_BYTES,
        TransportId::MessagesApi => api::API_BODY_MAX_BYTES,
    }
}

/// Enforce a transport's hard cap on `len` wire bytes (decision #42): exactly at
/// the cap passes, the first byte over is a hard stop naming the transport, the
/// size, and the cap — never an auto-shrink (decision #40 keeps oversize a hard
/// error). Shared by the gateway's pre-egress check and each push transport's
/// own self-guard so the two can never disagree on the boundary.
pub fn check_hard_cap(id: TransportId, len: usize) -> Result<(), TransportError> {
    let cap = hard_cap_bytes(id);
    if len > cap {
        return Err(TransportError::PayloadTooLarge(format!(
            "{len} B exceeds the {} hard cap of {cap} B (decision #42; shrink the payload — \
             no auto-truncation, decision #40)",
            transport_name(id)
        )));
    }
    Ok(())
}

/// The system framing shared by the push transports (doc 09 §4): the model is a
/// suggestion *function*, and only connectors act on its output.
pub(crate) const SYSTEM_FRAMING: &str = "You are Aperture's reasoning function for a local-first \
Windows assistant. Given the user's on-screen context, return concise, actionable suggestions. \
You may only *suggest*; the app validates and performs any action. Do not invent facts you cannot \
support from the context.";

/// The strict-output instruction appended to every prompt (doc 09 §4): the model
/// must return a JSON object matching `StructuredSuggestions`.
pub(crate) const SCHEMA_INSTRUCTION: &str = "\n\nReturn ONLY a single JSON object of the form \
{\"suggestions\":[{\"title\":string,\"connector_type\":\"browser\"|\"youtube\"|\"document\"|\"ide\"|\"none\",\
\"reconstruct_payload\":object,\"rationale\":string}],\"answer_text\":string|null}. No prose outside the JSON.";

/// Render an approved [`ContextPayload`] into a compact text prompt (doc 09 §5:
/// OCR text is the currency; screenshots are summarized, not inlined, here). Pure
/// + testable; both push transports build their request body from this.
pub(crate) fn render_prompt(payload: &ContextPayload) -> String {
    use std::fmt::Write;
    let mut s = String::from("Context:\n");
    for item in &payload.items {
        match item {
            PayloadItem::OcrText { text, .. } => {
                let _ = writeln!(s, "- screen text: {text}");
            }
            PayloadItem::UserAddition { text } => {
                let _ = writeln!(s, "- user request: {text}");
            }
            PayloadItem::Connector { connector_type, payload } => {
                let _ = writeln!(s, "- resumable {connector_type}: {payload}");
            }
            PayloadItem::EventTrail { events } => {
                let _ = writeln!(s, "- recent activity: {} events", events.len());
            }
            PayloadItem::Screenshot { width, height, .. } => {
                let _ = writeln!(s, "- screenshot attached ({width}x{height})");
            }
        }
    }
    s.push_str(SCHEMA_INSTRUCTION);
    s
}

/// Extract the first balanced top-level `{...}` object from model text — the model
/// may fence or preface its JSON despite the instruction (doc 09 §4/§6). Returns
/// the JSON slice for [`crate::suggestion_validator::parse_response`], or `None`.
pub(crate) fn extract_json(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let (mut depth, mut in_str, mut escaped) = (0i32, false, false);
    for (i, ch) in text[start..].char_indices() {
        match ch {
            '"' if !escaped => in_str = !in_str,
            '\\' if in_str => {
                escaped = !escaped;
                continue;
            }
            '{' if !in_str => depth += 1,
            '}' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + i + 1]);
                }
            }
            _ => {}
        }
        escaped = false;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_contracts::{Intent, TransportTarget};
    use uuid::Uuid;

    #[test]
    fn render_prompt_includes_items_and_the_schema_instruction() {
        let payload = ContextPayload {
            payload_id: Uuid::nil(),
            created_ts: 0,
            intent: Intent::SummarizeCurrent,
            items: vec![
                PayloadItem::OcrText { source_event_id: 1, text: "build failed".into(), redacted: false },
                PayloadItem::UserAddition { text: "why?".into() },
            ],
            redactions: vec![],
            enrichment_offered: false,
            transport_target: TransportTarget::MessagesApi,
            user_approved: false,
        };
        let p = render_prompt(&payload);
        assert!(p.contains("build failed") && p.contains("why?"));
        assert!(p.contains("Return ONLY a single JSON object"), "schema instruction appended");
    }

    #[test]
    fn extract_json_handles_fences_prose_and_strings_with_braces() {
        assert_eq!(extract_json(r#"{"a":1}"#), Some(r#"{"a":1}"#));
        assert_eq!(extract_json("sure:\n```json\n{\"a\":1}\n```"), Some(r#"{"a":1}"#));
        assert_eq!(extract_json(r#"{"t":"a }{ b"}"#), Some(r#"{"t":"a }{ b"}"#));
        assert_eq!(extract_json("no json"), None);
    }

    #[test]
    fn c42_hard_caps_are_exact_boundaries_naming_the_transport() {
        for id in [
            TransportId::ClaudeCli,
            TransportId::ClaudeDesktopMcp,
            TransportId::MessagesApi,
        ] {
            let cap = hard_cap_bytes(id);
            assert!(check_hard_cap(id, cap).is_ok(), "exactly at the cap passes");
            let err = check_hard_cap(id, cap + 1).unwrap_err();
            assert!(matches!(err, TransportError::PayloadTooLarge(_)), "{err:?}");
            assert!(
                err.to_string().contains(transport_name(id)),
                "the error names the transport: {err}"
            );
        }
    }

    #[test]
    fn c42_cap_values_match_the_documented_limits() {
        // API: 32 MB body rejection line minus base64/envelope margin ⇒ 20 MB.
        assert_eq!(hard_cap_bytes(TransportId::MessagesApi), 20 * 1024 * 1024);
        // CLI: the documented headless ~10 MB stdin/arg cap (doc 09 §3).
        assert_eq!(hard_cap_bytes(TransportId::ClaudeCli), 10 * 1024 * 1024);
        // MCP: conservative 1 MB tool-result policy for Claude Desktop.
        assert_eq!(hard_cap_bytes(TransportId::ClaudeDesktopMcp), 1024 * 1024);
    }
}
