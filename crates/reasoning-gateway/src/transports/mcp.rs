//! Claude Desktop via MCP transport — **Pull**, the handoff model (doc 09 §3, doc 13 §3).
//!
//! Unlike CLI/API (push), Aperture **cannot push a prompt into Claude Desktop**.
//! Instead Aperture runs a **local MCP server** (stdio, JSON-RPC 2.0) registered
//! in `%APPDATA%\Claude\claude_desktop_config.json` (doc 09 §3). Claude Desktop
//! initiates: it discovers and calls Aperture's tools.
//!
//! ## Exposed tools (doc 09 §3)
//! - [`TOOL_GET_CONTEXT`] — `aperture_get_context(payload_id)`. **THE GATE LIVES
//!   HERE** (doc 13 §3): the handler *blocks*, shows the preview, and returns the
//!   payload **only on the user's Send** — otherwise it returns a refusal. This
//!   is the exact same [`crate::preview`] gate the push transports run, enforced
//!   inside the tool call so Claude Desktop's request blocks on the user's
//!   decision. MCP hosts additionally require their own tool-use consent.
//! - [`TOOL_LIST_RECENT`] — `aperture_list_recent`. Lists recent payload ids for
//!   Claude to choose from. Metadata only; never returns excluded content.
//! - [`TOOL_SUBMIT_SUGGESTIONS`] — `aperture_submit_suggestions(json)`. The return
//!   channel: Claude submits [`aperture_contracts::StructuredSuggestions`] back,
//!   schema-checked + per-connector re-validated (doc 09 §4) like every other source.
//!
//! UX: "copied a starter prompt — paste in Claude"; suggestions return via
//! `aperture_submit_suggestions` (doc 09 §3).
//!
//! // TODO(M7: [VERIFY] config path, registration shape, and JSON-RPC framing
//! //          against Claude Desktop — doc 09 §3 marks these [VERIFY].)

// TODO(M7:) stdio JSON-RPC 2.0 server + the three tool handlers land in M7.

use async_trait::async_trait;

use aperture_contracts::{
    ContextPayload, Health, ReasoningTransport, StructuredSuggestions, TransportError, TransportId,
};

/// Tool name: returns one payload — **gated** inside the handler (doc 13 §3).
pub const TOOL_GET_CONTEXT: &str = "aperture_get_context";
/// Tool name: lists recent payload ids (metadata only).
pub const TOOL_LIST_RECENT: &str = "aperture_list_recent";
/// Tool name: the suggestions return channel.
pub const TOOL_SUBMIT_SUGGESTIONS: &str = "aperture_submit_suggestions";

/// Pull transport: the local MCP (stdio, JSON-RPC 2.0) server bridging Claude
/// Desktop (doc 09 §3).
pub struct McpTransport {
    /// `%APPDATA%\Claude\claude_desktop_config.json` registration path (settings/derived).
    /// // TODO(M7:) resolve via `dirs`; [VERIFY] exact location & schema.
    _config_path: String,
}

impl McpTransport {
    /// Construct from the resolved Claude Desktop config path.
    pub fn new(config_path: impl Into<String>) -> Self {
        Self {
            _config_path: config_path.into(),
        }
    }

    /// Register Aperture's tools in `claude_desktop_config.json` (doc 09 §3).
    /// // TODO(M7: [VERIFY] registration format & whether a Desktop restart is required.)
    pub fn register(&self) -> Result<(), TransportError> {
        todo!("M7: write the MCP server entry into claude_desktop_config.json")
    }

    /// `aperture_get_context(payload_id)` handler — **the gate** (doc 13 §3).
    ///
    /// Blocks the JSON-RPC call, drives the [`crate::preview`] session for
    /// `payload_id`, and returns the approved payload **only** if the user
    /// presses Send (which is the sole way `user_approved` becomes true). On
    /// Cancel, returns a refusal and nothing leaves the machine (zero residue).
    pub async fn handle_get_context(
        &self,
        _payload_id: &str,
    ) -> Result<ContextPayload, TransportError> {
        // TODO(M7:) load payload by id -> preview::PreviewSession::new(..) -> await Send/Cancel:
        //           Send => return approved payload; Cancel => Err(TransportError::Cancelled).
        //           This blocks Claude Desktop's tool call on the user (doc 13 §3).
        todo!("M7: block on the preview gate inside the tool handler; return only on Send")
    }

    /// `aperture_submit_suggestions(json)` handler — the return channel (doc 09 §3/§4).
    pub async fn handle_submit_suggestions(
        &self,
        _json: serde_json::Value,
    ) -> Result<StructuredSuggestions, TransportError> {
        // TODO(M7:) suggestion_validator::parse_response + validate (one repair round-trip
        //           allowed on MCP, doc 09 §6).
        todo!("M7: schema-check + per-connector re-validate the submitted suggestions")
    }
}

#[async_trait]
impl ReasoningTransport for McpTransport {
    fn id(&self) -> TransportId {
        TransportId::ClaudeDesktopMcp
    }

    async fn health(&self) -> Health {
        // TODO(M7:) is Claude Desktop installed and our MCP server registered/reachable?
        //           Map to Ready / NeedsSetup(reason) / Unavailable(reason).
        todo!("M7: detect Claude Desktop + our MCP registration")
    }

    async fn send(
        &self,
        _payload: &ContextPayload,
    ) -> Result<StructuredSuggestions, TransportError> {
        // PULL asymmetry (doc 09 §3): Aperture cannot push a prompt into Claude Desktop.
        // The real flow is Claude-initiated via the tool handlers above; `send` here is
        // the handoff ("copied a starter prompt — paste in Claude"), and suggestions
        // arrive asynchronously via `aperture_submit_suggestions`.
        // TODO(M7:) stage the starter prompt + arm the submit channel; surface the handoff UX.
        todo!("M7: pull-model handoff — stage starter prompt, await aperture_submit_suggestions")
    }
}
