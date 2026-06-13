//! # Aperture privacy, security & consent (doc 13)
//!
//! This crate is the home of the project's privacy guarantees. It does **not**
//! police the cloud boundary — that is structural: per the two-emitter rule
//! (doc 13 §2, invariant 2) only `aperture-reasoning-gateway` may open network
//! sockets or spawn the Claude CLI. This crate is egress-free by construction
//! like every other non-gateway crate, and it provides the local mechanisms
//! that make the guarantees true:
//!
//! - [`redaction`] — the ordered, deterministic redaction pipeline that runs at
//!   payload assembly, **before** preview (doc 13 §5).
//! - [`exclusion_manager`] — the exclusion list + private/incognito heuristic
//!   that stops collection at the earliest gate (doc 13 §4).
//! - [`audit_log`] — the local-only `capture_toggle` / `cloud_send` audit trail;
//!   rows survive Purge All for 30 d (doc 13 §3, §7).
//! - [`key_manager`] — the per-install at-rest key, wrapped by DPAPI (current
//!   user) and stored in Windows Credential Manager (doc 13 §6).
//! - [`consent`] — first-run capture opt-in (default OFF), per-send approval
//!   (no "always allow" in v1), and voice opt-in at first PTT (doc 13 §8).
//!
//! ## Invariants honored here
//! - **(2) transparency gate:** nothing in this crate touches the network. The
//!   audit log only *records* that the gateway sent bytes; it never sends them.
//! - **(3) capture toggle:** [`consent::ConsentState::capture_enabled`] gates
//!   capture; the off-transition (release sidecars, VRAM->~0) is driven by
//!   `aperture-capture`/orchestration, recorded here as a `capture_toggle` audit
//!   event.

// TODO(M9): privacy is the M9 milestone (doc 16). Most bodies are `todo!("M9:")`.

pub mod audit_log;
pub mod consent;
pub mod exclusion_manager;
pub mod key_manager;
pub mod redaction;

/// Errors surfaced by the privacy subsystem.
#[derive(Debug, thiserror::Error)]
pub enum PrivacyError {
    /// A redaction rule's regex failed to compile (user-defined terms, doc 13 §5).
    #[error("invalid redaction rule `{rule}`: {source}")]
    InvalidRule {
        rule: String,
        #[source]
        source: regex::Error,
    },

    /// DPAPI wrap/unwrap or Credential Manager access failed (doc 13 §6).
    /// If the wrapped key cannot be unwrapped the DB is unreadable **by design**.
    #[error("key manager error: {0}")]
    KeyManager(String),

    /// Writing/reading the local audit log failed (doc 13 §3).
    #[error("audit log error: {0}")]
    Audit(String),

    /// Serialization of a settings/audit structure failed.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}
