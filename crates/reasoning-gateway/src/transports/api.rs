//! Messages API transport — **Push**, plain HTTPS (doc 09 §3, §5).
//!
//! Opening an HTTPS socket is the privileged capability the two-emitter rule
//! (doc 13 §2) confines to this crate; `reqwest` (rustls) lives here and nowhere
//! else. Needs the user's API key (settings, never logged).
//!
//! ## NG8 — model name and beta headers are SETTINGS, never code (doc 09 §3)
//! No model string / beta header is hard-coded: the model id, `anthropic-version`,
//! and any `anthropic-beta` headers come from [`ApiSettings`] at call time (the
//! *default* model seed is `claude-opus-4-8`, but that seed lives in settings).
//!
//! ## Prompt-cache layout (doc 09 §5)
//! `tools -> system -> messages`, prefix-cached, so the request is assembled
//! stable-prefix-first: the framing (+ schema) goes in a cached `system` block
//! with an `ephemeral` breakpoint; the volatile [`ContextPayload`] rides last in
//! the user message. Images are cache-hostile, which is why OCR text is the
//! default currency and screenshots are opt-in (doc 09 §5).
//!
//! ## Status (M7, best-effort)
//! Real `reqwest` egress + response parse; **UNVERIFIED** — not exercised against
//! the live endpoint in CI. [VERIFY] the endpoint, header names, request/response
//! shape, and current pricing/TTLs against the API at build time (doc 09 §3/§5).

use async_trait::async_trait;

use aperture_contracts::{
    ContextPayload, Health, ReasoningTransport, StructuredSuggestions, TransportError, TransportId,
};

use crate::suggestion_validator::parse_response;
use crate::transports::{extract_json, render_prompt, SYSTEM_FRAMING};

/// Per-NG8 wire knobs, all sourced from settings — never hard-coded (doc 09 §3).
#[derive(Debug, Clone)]
pub struct ApiSettings {
    /// e.g. `https://api.anthropic.com/v1/messages`. // [VERIFY]
    pub endpoint: String,
    /// Model id from settings (default seed: `claude-opus-4-8`) — NOT a code constant (NG8).
    pub model: String,
    /// `anthropic-version` header value, e.g. `2023-06-01` — settings, not code (NG8). // [VERIFY]
    pub anthropic_version: String,
    /// Any `anthropic-beta` header values — settings, not code (NG8). Empty for none.
    pub beta_headers: Vec<String>,
    /// Cache TTL for the stable prefix breakpoint: `"5m"` (default) or `"1h"` (doc 09 §5).
    pub cache_ttl: String,
    /// Max output tokens (settings; small — the model returns compact JSON).
    pub max_tokens: u32,
}

/// Push transport over the Messages API (doc 09 §3).
pub struct ApiTransport {
    settings: ApiSettings,
    /// The user's API key (settings; never logged). // TODO(M9:) source from the key store.
    api_key: String,
    /// INVARIANT (doc 13 §2): the `reqwest::Client` — a socket-opening handle — is
    /// owned here, inside the gateway crate, and nowhere else.
    http: reqwest::Client,
}

impl ApiTransport {
    /// Construct from settings + the user's API key.
    pub fn new(settings: ApiSettings, api_key: impl Into<String>) -> Self {
        Self {
            settings,
            api_key: api_key.into(),
            http: reqwest::Client::builder()
                .build()
                .expect("rustls reqwest client builds"),
        }
    }

    /// Assemble the Messages request body ONCE (stable prefix first, doc 09 §5) —
    /// the single source of truth for both [`send`](Self::send) and
    /// [`wire_bytes`](ReasoningTransport::wire_bytes), so the audited hash is over
    /// the exact JSON that egresses.
    fn build_body(&self, payload: &ContextPayload) -> serde_json::Value {
        serde_json::json!({
            "model": self.settings.model,
            "max_tokens": self.settings.max_tokens,
            "system": [{
                "type": "text",
                "text": SYSTEM_FRAMING,
                "cache_control": { "type": "ephemeral", "ttl": self.settings.cache_ttl },
            }],
            "messages": [{ "role": "user", "content": render_prompt(payload) }],
        })
    }
}

#[async_trait]
impl ReasoningTransport for ApiTransport {
    fn id(&self) -> TransportId {
        TransportId::MessagesApi
    }

    async fn health(&self) -> Health {
        // Offline-safe: presence of a key ⇒ Ready (no auth probe here — a probe
        // would itself egress; the send call surfaces a real auth failure).
        if self.api_key.trim().is_empty() {
            Health::NeedsSetup("add an Anthropic API key in settings".into())
        } else {
            Health::Ready
        }
    }

    fn supports_push(&self) -> bool {
        true
    }

    /// The exact JSON body that egresses — audited by the gateway (doc 13 §3).
    fn wire_bytes(&self, payload: &ContextPayload) -> Vec<u8> {
        serde_json::to_vec(&self.build_body(payload)).unwrap_or_default()
    }

    async fn send(
        &self,
        payload: &ContextPayload,
    ) -> Result<StructuredSuggestions, TransportError> {
        // INVARIANT (doc 13 §2): the egress primitive self-guards — never spawn the
        // socket for an unapproved payload, even if a caller bypasses the gateway.
        if !payload.user_approved {
            return Err(TransportError::Other(
                "refusing to transmit an unapproved payload (two-emitter rule, doc 13 §2)".into(),
            ));
        }
        // Stable prefix first (cached): system framing; volatile payload last, in
        // the user message (doc 09 §5). Body built via build_body so wire_bytes and
        // the audited hash are the same bytes.
        let body = self.build_body(payload);

        let mut req = self
            .http
            .post(&self.settings.endpoint)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.settings.anthropic_version)
            .json(&body);
        for beta in &self.settings.beta_headers {
            req = req.header("anthropic-beta", beta);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| TransportError::Other(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(TransportError::Unhealthy(format!("Messages API HTTP {}", resp.status())));
        }
        let envelope: serde_json::Value =
            resp.json().await.map_err(|_| TransportError::MalformedResponse)?;
        // Anthropic response: { content: [ { type: "text", text: "..." }, ... ] }.
        let text = envelope
            .pointer("/content/0/text")
            .and_then(|v| v.as_str())
            .ok_or(TransportError::MalformedResponse)?;
        let json = extract_json(text).ok_or(TransportError::MalformedResponse)?;
        // One repair round-trip on malformed JSON is possible here (doc 09 §6); left
        // as a [VERIFY] follow-up — the strict-JSON instruction usually suffices.
        parse_response(json).map_err(|_| TransportError::MalformedResponse)
    }
}
