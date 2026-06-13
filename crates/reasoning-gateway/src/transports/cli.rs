//! Claude Code CLI transport — **Push**, the primary candidate (doc 09 §3).
//!
//! Aperture initiates: spawn the headless CLI and parse its JSON result.
//! Spawning a child process is a privileged capability — this file is one of the
//! few code paths in the crate the two-emitter rule (doc 13 §2) permits to do it.
//!
//! ```text
//! claude -p <prompt> --output-format json
//! ```
//! // TODO(M7: [VERIFY] exact flags, headless invocation, and JSON shape against
//! //          the installed CLI version before shipping — doc 09 §3 marks these [VERIFY].)
//!
//! Parses the headless result envelope `{ result, total_cost_usd, session_id }`
//! (doc 09 §3). `result` carries the model's text, from which we extract the
//! strict-JSON [`StructuredSuggestions`] (doc 09 §4); `total_cost_usd` feeds
//! cost telemetry.
//!
//! ## stdin caveats (doc 09 §3) — keep payloads compact
//! Documented headless behavior: empty output on large stdin (~7 000 chars on
//! some versions) and a hard ~10 MB stdin cap. Policy: keep the prompt compact
//! (OCR text, not screenshots — doc 09 §5) and, past the small-stdin threshold,
//! pass long context via a **temp-file path** if the CLI version supports it
//! rather than piping it through stdin. The preview's > 50 KB warning and the
//! per-transport hard-stop (doc 09 §6) guard this.

// TODO(M7:) spawn + parse + temp-file-for-large-context land in M7.

use async_trait::async_trait;

use aperture_contracts::{
    ContextPayload, Health, ReasoningTransport, StructuredSuggestions, TransportError, TransportId,
};

/// Below this prompt length we pipe via stdin; at/above it we spill to a temp
/// file to dodge the documented small-stdin empty-output caveat (~7k chars,
/// doc 09 §3). // [VERIFY] the real threshold for the installed CLI version.
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
    /// Path to the `claude` executable (settings, not hard-coded). // TODO(M7:)
    _exe_path: String,
}

impl CliTransport {
    /// Construct from the configured CLI executable path (settings).
    pub fn new(exe_path: impl Into<String>) -> Self {
        Self {
            _exe_path: exe_path.into(),
        }
    }
}

#[async_trait]
impl ReasoningTransport for CliTransport {
    fn id(&self) -> TransportId {
        TransportId::ClaudeCli
    }

    async fn health(&self) -> Health {
        // TODO(M7:) probe `claude --version` (or equivalent) for installed + authenticated;
        //           map to Ready / NeedsSetup / Unavailable. // [VERIFY] the auth check.
        todo!("M7: detect the CLI is installed, on PATH, and authenticated")
    }

    async fn send(
        &self,
        _payload: &ContextPayload,
    ) -> Result<StructuredSuggestions, TransportError> {
        // INVARIANT (doc 13 §2): only reached for an already-approved payload; the
        // gateway re-checks `user_approved` before calling any transport.
        // TODO(M7:)
        //   1. render the approved payload into a compact strict-JSON prompt (doc 09 §4 schema).
        //   2. if prompt.len() < CLI_STDIN_COMPACT_CHARS -> pass via -p/stdin;
        //      else spill to a temp file and pass its path (doc 09 §3) — and clean it up.
        //   3. tokio::process::Command: `claude -p <prompt> --output-format json` (doc 09 §3 [VERIFY]).
        //   4. parse CliResult; extract StructuredSuggestions from `result`
        //      (suggestion_validator::parse_response). On malformed JSON, render `result`
        //      as prose (answer_text) — no CLI repair round-trip (doc 09 §6).
        //   5. mid-call cancel => kill the child; store nothing partial (doc 09 §6).
        let _ = (CLI_STDIN_COMPACT_CHARS, CLI_STDIN_MAX_BYTES);
        todo!("M7: spawn `claude -p ... --output-format json`, parse CliResult, extract suggestions")
    }
}
