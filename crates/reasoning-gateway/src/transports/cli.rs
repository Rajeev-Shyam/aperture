//! Claude Code CLI transport — **Push**, the first fallback after Desktop-MCP
//! (MCP-primary order, ADR-025 / doc 09 §3).
//!
//! Aperture initiates: spawn the headless CLI and parse its JSON result.
//! Spawning a child process is a privileged capability — this file is one of the
//! few code paths in the crate the two-emitter rule (doc 13 §2) permits it.
//!
//! ```text
//! claude -p <prompt> --output-format json
//! ```
//!
//! Parses the headless result envelope `{ result, total_cost_usd, session_id }`
//! (doc 09 §3). `result` carries the model's text, from which we extract the
//! strict-JSON [`StructuredSuggestions`] (doc 09 §4); `total_cost_usd` feeds cost
//! telemetry.
//!
//! ## stdin caveats (doc 09 §3) — keep payloads compact
//! Documented headless behavior: empty output on large stdin (~7 000 chars on some
//! versions) and a hard ~10 MB cap. Policy: keep the prompt compact (OCR text, not
//! screenshots — doc 09 §5); the preview's > 50 KB warning + the per-transport
//! hard-stop guard this.
//!
//! ## Status (M7, best-effort)
//! Real `tokio::process` spawn + parse; **UNVERIFIED** — not exercised against an
//! installed CLI in CI. [VERIFY] exact flags, the headless auth check, the JSON
//! shape, and the large-input mechanism against the installed CLI version.

use async_trait::async_trait;
use tokio::process::Command;

use aperture_contracts::{
    ContextPayload, Health, ReasoningTransport, StructuredSuggestions, TransportError, TransportId,
};

use crate::suggestion_validator::parse_response;
use crate::transports::{extract_json, render_prompt, SYSTEM_FRAMING};

/// Below this prompt length we pass via `-p`; at/above it, large-context spill to a
/// temp file is the intended mechanism (~7k-char small-stdin caveat, doc 09 §3).
/// // [VERIFY] the real threshold + spill flag for the installed CLI version.
pub const CLI_STDIN_COMPACT_CHARS: usize = 7_000;

/// The CLI's hard stdin cap (~10 MB, doc 09 §3). The payload builder hard-stops
/// before this on the CLI transport (doc 09 §6). // [VERIFY] at build time.
pub const CLI_STDIN_MAX_BYTES: usize = 10 * 1024 * 1024;

/// The headless `--output-format json` result envelope (doc 09 §3).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CliResult {
    /// The model's textual output; contains the strict-JSON suggestions block.
    pub result: String,
    /// Reported spend for the call; feeds cost telemetry.
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    /// Session id echoed by the CLI.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Push transport over the Claude Code CLI (doc 09 §3).
pub struct CliTransport {
    /// Path to the `claude` executable (settings, not hard-coded).
    exe_path: String,
}

impl CliTransport {
    /// Construct from the configured CLI executable path (settings).
    pub fn new(exe_path: impl Into<String>) -> Self {
        Self { exe_path: exe_path.into() }
    }

    /// The exact prompt string transmitted via `-p` — the system framing (shared
    /// with the API path, doc 09 §4) + the rendered payload + the schema
    /// instruction. Single source of truth for both `send` and `wire_bytes`.
    fn build_prompt(&self, payload: &ContextPayload) -> String {
        format!("{SYSTEM_FRAMING}\n\n{}", render_prompt(payload))
    }
}

#[async_trait]
impl ReasoningTransport for CliTransport {
    fn id(&self) -> TransportId {
        TransportId::ClaudeCli
    }

    async fn health(&self) -> Health {
        // `--version` succeeding ⇒ installed + on PATH. It does NOT prove login;
        // a real send surfaces an auth failure. [VERIFY] a cheap auth check.
        match Command::new(&self.exe_path).arg("--version").output().await {
            Ok(o) if o.status.success() => Health::Ready,
            Ok(o) => Health::NeedsSetup(format!("`claude --version` exited {}", o.status)),
            Err(e) => Health::Unavailable(format!("claude CLI not found: {e}")),
        }
    }

    fn supports_push(&self) -> bool {
        true
    }

    /// The exact prompt bytes that egress as the `-p` argument — audited by the
    /// gateway (doc 13 §3).
    fn wire_bytes(&self, payload: &ContextPayload) -> Vec<u8> {
        self.build_prompt(payload).into_bytes()
    }

    async fn send(
        &self,
        payload: &ContextPayload,
    ) -> Result<StructuredSuggestions, TransportError> {
        // INVARIANT (doc 13 §2): the egress primitive self-guards — never spawn the
        // CLI for an unapproved payload, even if a caller bypasses the gateway.
        if !payload.user_approved {
            return Err(TransportError::Other(
                "refusing to spawn the CLI for an unapproved payload (two-emitter rule, doc 13 §2)".into(),
            ));
        }
        let prompt = self.build_prompt(payload);
        if prompt.len() >= CLI_STDIN_MAX_BYTES {
            return Err(TransportError::PayloadTooLarge(format!(
                "{} B exceeds the CLI cap of {} B",
                prompt.len(),
                CLI_STDIN_MAX_BYTES
            )));
        }
        if prompt.len() >= CLI_STDIN_COMPACT_CHARS {
            tracing::warn!(
                len = prompt.len(),
                "prompt exceeds the compact threshold; large-context temp-file spill is [VERIFY] on this CLI"
            );
        }

        let output = Command::new(&self.exe_path)
            .arg("-p")
            .arg(&prompt)
            .arg("--output-format")
            .arg("json")
            .output()
            .await
            .map_err(|e| TransportError::Other(e.to_string()))?;
        if !output.status.success() {
            return Err(TransportError::Unhealthy(format!("claude CLI exited {}", output.status)));
        }

        let envelope: CliResult =
            serde_json::from_slice(&output.stdout).map_err(|_| TransportError::MalformedResponse)?;
        if let Some(cost) = envelope.total_cost_usd {
            tracing::info!(cost_usd = cost, session = ?envelope.session_id, "claude CLI call");
        }
        // Extract strict JSON from `result`; on failure render it as prose — NO CLI
        // repair round-trip (doc 09 §6).
        match extract_json(&envelope.result).and_then(|j| parse_response(j).ok()) {
            Some(suggestions) => Ok(suggestions),
            None => Ok(StructuredSuggestions {
                suggestions: Vec::new(),
                answer_text: Some(envelope.result),
            }),
        }
    }
}
