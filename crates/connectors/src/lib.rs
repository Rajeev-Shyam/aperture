//! Deep-link / state-resumption connectors (doc 10): the expansion seam (G3).
//!
//! Each connector observes the event bus, snapshots a *resumable handle*
//! (a versioned [`ConnectorState`]), and — on a bubble click (Critical Path B,
//! doc 02 §5) — [`Connector::reconstruct`]s that state into a [`ResumeArtifact`]
//! which [`deeplinker::open`] dispatches via `ShellExecuteW` / a protocol
//! handler. The pattern engine and Bubble UI know **only the trait**, so v2
//! connectors (Slack thread, terminal cwd, Figma frame…) plug in without core
//! changes (doc 10 §1).
//!
//! Invariants honored here:
//!   * **No network, no GPU** (doc 10 §7): this whole subsystem is CPU-trivial
//!     and rides the existing event pipeline. The transparency gate is not even
//!     in play — only `aperture-reasoning-gateway` may open sockets, and nothing
//!     here does. `ShellExecuteW` hands a URL/file to the OS default handler;
//!     *we* never speak HTTP.
//!   * **Capture toggle** (doc 12 §6): capture only ever happens in response to
//!     bus events, which stop when the toggle is OFF — no sidecars, no VRAM.
//!
//! The trait, its associated types, and [`ConnectorError`] are **contracts**
//! (doc 15 §3) — they are re-exported here, never redefined.

use std::time::Duration;

// Re-export the contract surface so downstream crates depend on `aperture-connectors`
// for the concrete connectors and still get the trait from one place (doc 15 §3).
pub use aperture_contracts::connector::ConnectorError;
pub use aperture_contracts::{Connector, ConnectorState, OpenOutcome, ResumeArtifact};

pub mod browser;
pub mod deeplinker;
pub mod document;
pub mod heuristics;
pub mod ide;
pub mod registry;
mod vscode_mru;
pub mod youtube;

pub use browser::BrowserConnector;
pub use document::DocumentConnector;
pub use heuristics::SecondaryDeriver;
pub use ide::IdeConnector;
pub use registry::ConnectorRegistry;
pub use youtube::YoutubeConnector;

/// Build the registry with every v1 connector self-registered (doc 10 §1).
///
/// Called once at startup by the composition root (doc 12). v2 connectors
/// register here too — the only edit a new connector requires. Order matters:
/// YouTube registers ahead of the generic browser connector so the more-specific
/// match claims watch-URL navigations first (doc 10 §3).
pub fn default_registry() -> ConnectorRegistry {
    let mut reg = ConnectorRegistry::new();
    reg.register(Box::new(YoutubeConnector::new()));
    reg.register(Box::new(BrowserConnector::new()));
    reg.register(Box::new(DocumentConnector::new()));
    reg.register(Box::new(IdeConnector::new()));
    reg
}

/// Assemble a fresh [`ConnectorState`] row (uuid id, payload v1, TTL stamped).
/// One place so every connector stamps `stale_after_ts` the same way.
pub(crate) fn build_state(
    connector_type: &str,
    reconstruct_payload: serde_json::Value,
    captured_ts: i64,
    ttl: Duration,
) -> ConnectorState {
    ConnectorState {
        id: uuid::Uuid::new_v4().to_string(),
        connector_type: connector_type.to_string(),
        reconstruct_payload,
        payload_version: 1,
        captured_ts,
        stale_after_ts: Some(captured_ts + ttl.as_millis() as i64),
    }
}

/// Wall-clock epoch ms — used only by `validate()` (cloud payloads have no event
/// timestamp to inherit). Capture paths always use `ev.ts`.
pub(crate) fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The coalescing key for a captured state: two captures with the same
/// `(connector_type, natural_key)` describe the *same resumable resource*, so
/// the pipeline may update the existing `connector_state` row (freshest position
/// wins) instead of inserting a new one per heartbeat/tick. `None` ⇒ never
/// coalesce. Kept next to the payload definitions so the key stays in lockstep
/// with the v1 schemas.
pub fn natural_key(connector_type: &str, reconstruct_payload: &serde_json::Value) -> Option<String> {
    let field = match connector_type {
        "youtube" => "video_id",
        "browser" => "url",
        "document" | "ide" => "path",
        _ => return None,
    };
    reconstruct_payload
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}
