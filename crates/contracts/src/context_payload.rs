//! Contract 2 — the Context Payload (`aperture/context-payload/v1`, doc 03 §4, doc 15 §2).
//!
//! Contract law (restated from doc 15 §2):
//! - **(a)** exactly one object is built / previewed / sent;
//! - **(b)** only the preview panel may set [`ContextPayload::user_approved`];
//! - **(c)** only the reasoning gateway may consume an approved payload;
//! - **(d)** the SHA-256 of the wire bytes is audit-logged (`cloud_send`).
//!
//! "Preview == wire" is a *data-flow* property (a single object), not a UI
//! promise: the preview renders this object, edits mutate it, and Send transmits
//! exactly its serialization.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPayload {
    pub payload_id: Uuid,
    pub created_ts: i64,
    pub intent: Intent,
    pub items: Vec<PayloadItem>,
    /// Every applied redaction rule + its hit count (doc 13 §5), shown in the preview.
    pub redactions: Vec<Redaction>,
    #[serde(default)]
    pub enrichment_offered: bool,
    pub transport_target: TransportTarget,

    /// NOT part of the wire schema `v1`; the in-process approval flag.
    /// Only the preview panel sets it `true`; only the gateway reads it.
    #[serde(skip_serializing, default)]
    pub user_approved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    SummarizeCurrent,
    AnswerQuery,
    ExplainPattern,
    Custom,
}

/// A typed payload item. `screenshot` is **opt-in only** (enrichment) and is
/// pre-downscaled to <=1568 px / ~1.15 MP before preview (doc 09 §5).
/// `event_trail` is capped at 50 events (doc 03 §4).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PayloadItem {
    OcrText {
        source_event_id: i64,
        text: String,
        #[serde(default)]
        redacted: bool,
    },
    EventTrail {
        /// MUST be <= 50 (doc 03 §4); enforced by the payload builder.
        events: Vec<serde_json::Value>,
    },
    Connector {
        #[serde(rename = "type")]
        connector_type: String,
        payload: serde_json::Value,
    },
    Screenshot {
        width: u32,
        height: u32,
        data_b64: String,
    },
    UserAddition {
        text: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Redaction {
    /// e.g. `"email"`, `"window_excluded: 1Password"`, `"secret_key"`.
    pub rule: String,
    pub count: u32,
}

/// The ordered transport list lives in settings; the gateway picks the first
/// healthy one (doc 09 §3). Default order: CLI -> Desktop-MCP -> API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportTarget {
    ClaudeCli,
    ClaudeDesktopMcp,
    MessagesApi,
}

/// Hard warning threshold for serialized payload size (doc 09 §5).
pub const PAYLOAD_SIZE_WARN_BYTES: usize = 50 * 1024;
/// `event_trail` hard cap (doc 03 §4).
pub const EVENT_TRAIL_MAX: usize = 50;
