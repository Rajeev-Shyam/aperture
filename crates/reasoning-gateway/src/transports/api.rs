//! Messages API transport — **Push**, plain HTTPS (doc 09 §3, §5).
//!
//! Opening an HTTPS socket is the privileged capability the two-emitter rule
//! (doc 13 §2) confines to this crate; `reqwest` (rustls) lives here and nowhere
//! else. Needs the user's API key (settings, never logged).
//!
//! ## NG8 — model name and beta headers are SETTINGS, never code (doc 09 §3)
//! This skeleton deliberately hard-codes **no** model string and **no** beta
//! header. The model id, `anthropic-version`, and any `anthropic-beta` headers
//! are read from settings at call time so they can be retuned without a rebuild.
//! See [`ApiSettings`]. (Per the claude-api guidance the *default* model is
//! `claude-opus-4-8`, but that default is a settings seed, not a constant here.)
//!
//! ## Prompt-cache layout (doc 09 §5)
//! Render order is `tools` -> `system` -> `messages`; caching is a prefix match,
//! so the request is assembled **stable-prefix-first**:
//!
//! 1. **Stable prefix** (cached, `cache_control: ephemeral` breakpoint on its
//!    last block): system framing + the suggestions-JSON schema (doc 09 §4) +
//!    standing instructions. Byte-identical across calls.
//! 2. **Volatile payload last** (after the breakpoint, never cached): the
//!    per-call [`ContextPayload`] items.
//!
//! Cache reads price ~10% of base input; the 5-minute write costs +25% (1-hour
//! TTL ~2x) — real money on this transport and good latency on all (doc 09 §5).
//! Images are cache-hostile (any image change invalidates the cache), which is
//! why OCR text is the default currency and a screenshot is opt-in (doc 09 §5).
//!
//! // TODO(M7: [VERIFY] endpoint, header names, request/response shape, and current
//! //          pricing/TTLs at build time — doc 09 §3/§5 mark these [VERIFY].)

// TODO(M7:) HTTPS request assembly + prompt-cache breakpoint + repair round-trip land in M7.

use async_trait::async_trait;

use aperture_contracts::{
    ContextPayload, Health, ReasoningTransport, StructuredSuggestions, TransportError, TransportId,
};

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
}

/// Push transport over the Messages API (doc 09 §3).
pub struct ApiTransport {
    /// Wire knobs from settings (NG8).
    _settings: ApiSettings,
    /// The user's API key (settings; never logged). // TODO(M7:) source from key store.
    _api_key: String,
    // INVARIANT (doc 13 §2): the `reqwest::Client` — the only socket-opening handle in
    // the whole app — is owned here, inside the gateway crate, and nowhere else.
    // _http: reqwest::Client,
}

impl ApiTransport {
    /// Construct from settings + the user's API key.
    pub fn new(settings: ApiSettings, api_key: impl Into<String>) -> Self {
        Self {
            _settings: settings,
            _api_key: api_key.into(),
            // _http: reqwest::Client::builder().build().expect("rustls client"),  // TODO(M7:)
        }
    }
}

#[async_trait]
impl ReasoningTransport for ApiTransport {
    fn id(&self) -> TransportId {
        TransportId::MessagesApi
    }

    async fn health(&self) -> Health {
        // TODO(M7:) is an API key configured? Optionally a cheap auth probe.
        //           Map to Ready / NeedsSetup("add an API key") / Unavailable("offline").
        todo!("M7: API-key-present (+ optional reachability) -> Health")
    }

    async fn send(
        &self,
        _payload: &ContextPayload,
    ) -> Result<StructuredSuggestions, TransportError> {
        // INVARIANT (doc 13 §2): only reached for an already-approved payload; the
        // gateway re-checks `user_approved` before calling any transport.
        // TODO(M7:)
        //   1. assemble the request stable-prefix-first (doc 09 §5):
        //      system framing + suggestions-JSON schema (doc 09 §4) + standing instructions,
        //      with cache_control: {type:"ephemeral", ttl: settings.cache_ttl} on the last
        //      stable block; the volatile ContextPayload items go AFTER the breakpoint.
        //   2. headers from settings only (NG8): x-api-key, anthropic-version, anthropic-beta.
        //   3. POST via the crate-owned reqwest (rustls) client; this is the egress point.
        //   4. parse + suggestion_validator::validate; one repair round-trip on malformed
        //      JSON (doc 09 §6).
        //   5. mid-call cancel => drop the request future; store nothing partial (doc 09 §6).
        todo!("M7: build cache-friendly Messages request (settings-driven), POST, validate")
    }
}
