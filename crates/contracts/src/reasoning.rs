//! Contract 5 — the ReasoningGateway transport + StructuredSuggestions (doc 09, doc 15 §5).
//!
//! Law: **local candidates and cloud results flatten to the same
//! [`StructuredSuggestions`] shape** before the Bubble UI sees them — the UI is
//! source-agnostic except for a small "via Claude" source tag.
//!
//! The gateway holds an ordered transport list from settings, picks the first
//! healthy one, and is the **only** crate permitted to open network sockets or
//! spawn the Claude CLI (doc 13 §2, the two-emitter rule, CI-lint enforced).

use serde::{Deserialize, Serialize};

use crate::context_payload::ContextPayload;
use crate::suggestions::StructuredSuggestions;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportId {
    /// Push — Aperture initiates; `claude -p <prompt> --output-format json` (doc 09 §3).
    ClaudeCli,
    /// Pull — Claude initiates via a local MCP server; the gate lives inside the
    /// `aperture_get_context` tool handler (doc 09 §3, doc 13 §3).
    ClaudeDesktopMcp,
    /// Push — plain HTTPS to the Messages endpoint; model/headers are settings,
    /// never code (locked NG8, doc 09 §3).
    MessagesApi,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Health {
    /// Installed, running, authenticated — usable now.
    Ready,
    /// Reachable but not authenticated / not configured.
    NeedsSetup(String),
    /// Not installed / not running / offline.
    Unavailable(String),
}

/// A swappable transport (doc 09 §2). Implementations live **only** inside the
/// `reasoning-gateway` crate.
#[async_trait::async_trait]
pub trait ReasoningTransport: Send + Sync {
    fn id(&self) -> TransportId;
    /// Installed / running / authenticated?
    async fn health(&self) -> Health;
    /// Transmit an **already-approved** payload and return structured suggestions.
    async fn send(
        &self,
        payload: &ContextPayload,
    ) -> Result<StructuredSuggestions, TransportError>;
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("no healthy transport (all fell through)")]
    NoHealthyTransport,
    #[error("transport unhealthy: {0}")]
    Unhealthy(String),
    #[error("model returned malformed JSON")]
    MalformedResponse,
    #[error("payload exceeds this transport's limit ({0})")]
    PayloadTooLarge(String),
    #[error("cancelled mid-call")]
    Cancelled,
    #[error("transport error: {0}")]
    Other(String),
}
