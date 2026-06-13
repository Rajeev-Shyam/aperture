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

// Re-export the contract surface so downstream crates depend on `aperture-connectors`
// for the concrete connectors and still get the trait from one place (doc 15 §3).
pub use aperture_contracts::connector::ConnectorError;
pub use aperture_contracts::{Connector, ConnectorState, OpenOutcome, ResumeArtifact};

pub mod browser;
pub mod deeplinker;
pub mod document;
pub mod ide;
pub mod registry;
pub mod youtube;

pub use browser::BrowserConnector;
pub use document::DocumentConnector;
pub use ide::IdeConnector;
pub use registry::ConnectorRegistry;
pub use youtube::YoutubeConnector;

/// Build the registry with every v1 connector self-registered (doc 10 §1).
///
/// Called once at startup by orchestration (doc 12). v2 connectors register
/// here too — the only edit a new connector requires.
// TODO(M4): wire this into the orchestration startup sequence (doc 12 §3).
pub fn default_registry() -> ConnectorRegistry {
    let mut reg = ConnectorRegistry::new();
    reg.register(Box::new(BrowserConnector::new()));
    reg.register(Box::new(YoutubeConnector::new()));
    reg.register(Box::new(DocumentConnector::new()));
    reg.register(Box::new(IdeConnector::new()));
    reg
}
