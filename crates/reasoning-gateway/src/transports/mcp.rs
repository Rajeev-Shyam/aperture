//! Claude Desktop via MCP transport — **Pull**, the handoff model (doc 09 §3, doc 13 §3).
//!
//! Unlike CLI/API (push), Aperture **cannot push a prompt into Claude Desktop**.
//! Instead Aperture runs a **local MCP server** (stdio, JSON-RPC 2.0) registered
//! in `%APPDATA%\Claude\claude_desktop_config.json` (doc 09 §3). Claude Desktop
//! initiates: it discovers and calls Aperture's tools.
//!
//! ## Exposed tools (doc 09 §3)
//! - [`TOOL_GET_CONTEXT`] — `aperture_get_context(payload_id)`. **THE GATE LIVES
//!   HERE** (doc 13 §3): the handler blocks, shows the preview, and returns the
//!   payload **only on the user's Send** (the same [`crate::preview`] gate the push
//!   transports run). MCP hosts also require their own tool-use consent.
//! - [`TOOL_LIST_RECENT`] — `aperture_list_recent`. Metadata only.
//! - [`TOOL_SUBMIT_SUGGESTIONS`] — `aperture_submit_suggestions(json)`. The return
//!   channel: schema-checked + per-connector re-validated (doc 09 §4) like every source.
//!
//! ## Status (M7, best-effort)
//! [`with_aperture_registered`] (the config-merge) is pure + tested, and
//! [`McpTransport::handle_submit_suggestions`] parses via the shared validator.
//! The stdio JSON-RPC 2.0 server, the `aperture_get_context` payload-store gate,
//! and the pull-model `send` handoff need the composition-root payload store +
//! preview UI + a live Claude Desktop — **UNVERIFIED**, deferred with honest
//! errors (never a panic). [VERIFY] config path, registration shape, JSON-RPC framing.

use async_trait::async_trait;

use aperture_contracts::{
    ContextPayload, Health, ReasoningTransport, StructuredSuggestions, TransportError, TransportId,
};

use crate::suggestion_validator::parse_response;

/// Tool name: returns one payload — **gated** inside the handler (doc 13 §3).
pub const TOOL_GET_CONTEXT: &str = "aperture_get_context";
/// Tool name: lists recent payload ids (metadata only).
pub const TOOL_LIST_RECENT: &str = "aperture_list_recent";
/// Tool name: the suggestions return channel.
pub const TOOL_SUBMIT_SUGGESTIONS: &str = "aperture_submit_suggestions";

/// The MCP server executable Claude Desktop launches for Aperture. // [VERIFY] name.
const MCP_SERVER_COMMAND: &str = "aperture-mcp";

/// Pull transport: the local MCP (stdio, JSON-RPC 2.0) server bridging Claude
/// Desktop (doc 09 §3).
pub struct McpTransport {
    /// `%APPDATA%\Claude\claude_desktop_config.json` (settings/derived).
    /// // [VERIFY] exact location & schema.
    config_path: String,
}

impl McpTransport {
    /// Construct from the resolved Claude Desktop config path.
    pub fn new(config_path: impl Into<String>) -> Self {
        Self { config_path: config_path.into() }
    }

    /// Register Aperture's MCP server in `claude_desktop_config.json` (doc 09 §3):
    /// read the existing config (or start empty), merge our entry, write it back.
    /// // [VERIFY] whether a Desktop restart is required to pick it up.
    pub fn register(&self) -> Result<(), TransportError> {
        let path = std::path::Path::new(&self.config_path);
        let existing = if path.exists() {
            let raw = std::fs::read_to_string(path).map_err(|e| TransportError::Other(e.to_string()))?;
            // NEVER overwrite a config we couldn't parse — a partial write / hand-edit
            // / BOM would otherwise wipe the user's other MCP servers + settings.
            serde_json::from_str(&raw).map_err(|e| {
                TransportError::Other(format!(
                    "refusing to rewrite an unparseable claude_desktop_config.json ({e}); fix or move it first"
                ))
            })?
        } else {
            serde_json::json!({})
        };
        let merged = with_aperture_registered(existing, MCP_SERVER_COMMAND)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| TransportError::Other(e.to_string()))?;
        }
        std::fs::write(
            path,
            serde_json::to_string_pretty(&merged).map_err(|e| TransportError::Other(e.to_string()))?,
        )
        .map_err(|e| TransportError::Other(e.to_string()))
    }

    /// `aperture_get_context(payload_id)` handler — **the gate** (doc 13 §3).
    ///
    /// The gate logic itself is [`crate::preview::PreviewSession`] (tested there):
    /// blocks, and returns the approved payload only on Send. Wiring the
    /// payload-by-id **store** + the preview UI into this handler is the
    /// composition-root step (doc 11) — deferred with an honest error, never a panic.
    pub async fn handle_get_context(
        &self,
        _payload_id: &str,
    ) -> Result<ContextPayload, TransportError> {
        Err(TransportError::Other(
            "aperture_get_context: the payload store + preview gate are wired at the \
             composition root (M7 UI, doc 11); the gate logic is preview::PreviewSession"
                .into(),
        ))
    }

    /// `aperture_submit_suggestions(json)` handler — the return channel (doc 09 §3/§4).
    /// Schema-checks the body; per-connector re-validation happens at the gateway.
    pub async fn handle_submit_suggestions(
        &self,
        json: serde_json::Value,
    ) -> Result<StructuredSuggestions, TransportError> {
        parse_response(&json.to_string()).map_err(|_| TransportError::MalformedResponse)
    }
}

/// Merge Aperture's server entry into a `claude_desktop_config.json` value under
/// `mcpServers.aperture` (doc 09 §3). Pure — the fs read/write lives in
/// [`McpTransport::register`]; this is the testable core.
pub fn with_aperture_registered(
    mut root: serde_json::Value,
    command: &str,
) -> Result<serde_json::Value, TransportError> {
    let obj = root
        .as_object_mut()
        .ok_or_else(|| TransportError::Other("config root is not a JSON object".into()))?;
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    servers
        .as_object_mut()
        .ok_or_else(|| TransportError::Other("`mcpServers` is not a JSON object".into()))?
        .insert(
            "aperture".to_string(),
            serde_json::json!({ "command": command, "args": [] }),
        );
    Ok(root)
}

#[async_trait]
impl ReasoningTransport for McpTransport {
    fn id(&self) -> TransportId {
        TransportId::ClaudeDesktopMcp
    }

    /// MCP is **pull** (Claude Desktop initiates via tool calls) — it does not
    /// serve the gateway's push `send_with_preview` path (doc 09 §3). Returning
    /// false makes the picker skip it on a push Send so a registered MCP does not
    /// dead-end the default MCP-primary order.
    fn supports_push(&self) -> bool {
        false
    }

    async fn health(&self) -> Health {
        let path = std::path::Path::new(&self.config_path);
        if !path.exists() {
            return Health::Unavailable("Claude Desktop config not found".into());
        }
        match std::fs::read_to_string(path) {
            Ok(raw) => {
                let registered = serde_json::from_str::<serde_json::Value>(&raw)
                    .ok()
                    .and_then(|v| v.pointer("/mcpServers/aperture").cloned())
                    .is_some();
                if registered {
                    Health::Ready
                } else {
                    Health::NeedsSetup("register Aperture's MCP server (call register())".into())
                }
            }
            Err(e) => Health::Unavailable(e.to_string()),
        }
    }

    async fn send(
        &self,
        _payload: &ContextPayload,
    ) -> Result<StructuredSuggestions, TransportError> {
        // PULL asymmetry (doc 09 §3): Aperture cannot push into Claude Desktop. The
        // real flow is Claude-initiated via the tool handlers; there is no synchronous
        // push-`send` result. The gateway's push path should prefer CLI/API; MCP is
        // driven by Claude calling our tools. Honest error, never a panic.
        Err(TransportError::Other(
            "MCP is pull-model (doc 09 §3): suggestions arrive via aperture_submit_suggestions, \
             not a synchronous push-send"
                .into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_into_an_empty_config() {
        let merged = with_aperture_registered(serde_json::json!({}), "aperture-mcp").unwrap();
        assert_eq!(
            merged.pointer("/mcpServers/aperture/command").and_then(|v| v.as_str()),
            Some("aperture-mcp")
        );
    }

    #[test]
    fn preserves_existing_servers_when_registering() {
        let existing = serde_json::json!({
            "mcpServers": { "other": { "command": "x" } },
            "unrelated": true
        });
        let merged = with_aperture_registered(existing, "aperture-mcp").unwrap();
        assert!(merged.pointer("/mcpServers/other").is_some(), "existing server kept");
        assert!(merged.pointer("/mcpServers/aperture").is_some(), "ours added");
        assert_eq!(merged.pointer("/unrelated"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn rejects_a_non_object_root() {
        assert!(with_aperture_registered(serde_json::json!([1, 2, 3]), "x").is_err());
    }

    #[tokio::test]
    async fn submit_suggestions_parses_the_return_channel() {
        let mcp = McpTransport::new("unused");
        let out = mcp
            .handle_submit_suggestions(serde_json::json!({ "suggestions": [], "answer_text": "hi" }))
            .await
            .unwrap();
        assert_eq!(out.answer_text.as_deref(), Some("hi"));
    }
}
