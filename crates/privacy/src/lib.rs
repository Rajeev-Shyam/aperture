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
//! - [`detect_suggest`] — the first-run local scan that *suggests* exclusions the
//!   user confirms (doc 13 §4, §8; ADR-029/ADR-040).
//! - [`audit_log`] — the local-only `capture_toggle` / `cloud_send` audit trail;
//!   rows survive Purge All for 30 d (doc 13 §3, §7).
//!
//! **Exclusion matching does NOT live here.** `aperture_capture::exclusion`
//! (`ExclusionList`) owns the compiled rules, the process/class/title/`url_pattern`
//! matchers, and the private-window heuristic — it must run *inside* the capture
//! gate, before a frame is pulled (doc 05 §4), so that is where it belongs. The
//! persisted rules live in `aperture_db`'s `exclusion_list` table. This crate had
//! a second, weaker copy of that logic until M9; it was deleted rather than kept
//! in sync.
//! - [`key_manager`] — the per-install at-rest key, wrapped by DPAPI (current
//!   user) and stored in Windows Credential Manager (doc 13 §6).
//! - [`consent`] — first-run capture opt-in (default OFF), per-send approval
//!   (base mode; ADR-026's scoped always-allow still previews with a cancel
//!   window and audits every send), and voice opt-in at first PTT (doc 13 §8).
//!
//! ## Invariants honored here
//! - **(2) transparency gate:** nothing in this crate touches the network. The
//!   audit log only *records* that the gateway sent bytes; it never sends them.
//! - **(3) capture toggle:** [`consent::ConsentState::capture_enabled`] gates
//!   capture; the off-transition (release sidecars, VRAM->~0) is driven by
//!   `aperture-capture`/orchestration, recorded here as a `capture_toggle` audit
//!   event.

pub mod audit_log;
pub mod consent;
pub mod detect_suggest;
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
