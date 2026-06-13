//! Reasoning & Claude integration gateway (doc 09, doc 13).
//!
//! # The two-emitter rule (doc 13 §2) — THIS CRATE IS ONE OF EXACTLY TWO EMITTERS
//!
//! **This is the only crate in Aperture permitted to open network sockets or
//! spawn the Claude CLI.** Everything else — capture, OCR, embeddings, patterns,
//! the DB — is egress-free *by construction*. The cloud boundary is architectural
//! and testable, not merely policed:
//!
//! - **(a)** A CI lint denies socket / process-spawn APIs (`reqwest`, `std::net`,
//!   `std::process::Command`/`tokio::process`) outside this crate (doc 13 §2,
//!   doc 09 §2). // TODO(M7:) wire the clippy/custom lint allow-list to this crate.
//! - **(b)** The SC5 network-monitor test asserts *zero bytes on the proactive
//!   path; bytes only after Send* at every milestone gate.
//!
//! The gateway acts **only** on a [`ContextPayload`] flagged
//! [`ContextPayload::user_approved`]` == true` by the preview panel (doc 13 §3).
//! It is **never invoked by the proactive loop** (locked answer A, doc 09 §1) —
//! the trigger is always an explicit enrichment click or voice escalation.
//!
//! # Shape
//! The gateway holds an ordered list of [`ReasoningTransport`]s from settings
//! (default CLI -> Desktop-MCP -> API, doc 09 §3), picks the first healthy one,
//! falls through on health failure, and — when offline — leaves the local answer
//! standing without queuing anything silently (doc 09 §6).
//!
//! # Module map
//! - [`payload_builder`] — assemble + redact + cap + audit (doc 09 §5, doc 13 §5).
//! - [`preview`] — the consent gate; the *only* place that sets `user_approved` (doc 13 §3).
//! - [`suggestion_validator`] — schema-check + per-connector re-validation (doc 09 §4).
//! - [`transports`] — the three swappable transports (doc 09 §3).

// TODO(M0:) contracts are frozen; this crate's public surface is faithful to doc 09/§13.
// TODO(M7:) the gateway, its transports, the preview gate, and the audit hook land in M7.

pub mod payload_builder;
pub mod preview;
pub mod suggestion_validator;
pub mod transports;

use std::sync::Arc;

use aperture_contracts::{
    ContextPayload, Health, ReasoningTransport, StructuredSuggestions, TransportError, TransportId,
};

/// Errors raised by the gateway itself (distinct from a single transport's
/// [`TransportError`], doc 09 §6).
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    /// No transport in the ordered list reported [`Health::Ready`]; the local
    /// answer stands and nothing is queued (doc 09 §6).
    #[error("no healthy transport; local answer stands")]
    NoHealthyTransport,
    /// `send_with_preview` was called with `user_approved == false`. The gateway
    /// refuses to emit — only [`preview`] may flip the flag (doc 13 §2/§3).
    #[error("refusing to send: payload not user-approved (two-emitter rule, doc 13 §2)")]
    NotApproved,
    /// The chosen transport failed; see the wrapped error.
    #[error(transparent)]
    Transport(#[from] TransportError),
}

/// The reasoning gateway (doc 09 §2). Owns the ordered, swappable transport list
/// and the single egress chokepoint.
pub struct Gateway {
    /// Ordered from settings; the gateway picks the first healthy one and falls
    /// through on health failure (doc 09 §3). Default order: CLI -> Desktop-MCP -> API.
    transports: Vec<Box<dyn ReasoningTransport>>,
    /// Audit sink for the `cloud_send` row written at Send (doc 13 §3, doc 09 §5).
    /// // TODO(M9:) concrete type from `aperture_privacy::audit_log`.
    _audit: Arc<()>,
}

impl Gateway {
    /// Build a gateway from the settings-ordered transport list (doc 09 §3).
    /// The order is authoritative: `pick_healthy_transport` walks it front-to-back.
    pub fn new(transports: Vec<Box<dyn ReasoningTransport>>) -> Self {
        // TODO(M7:) accept the `aperture_privacy` audit-log handle instead of `Arc<()>`.
        Self {
            transports,
            _audit: Arc::new(()),
        }
    }

    /// Report the health of every configured transport, in settings order — feeds
    /// the preview's "transport target" line and the fall-through notice (doc 09 §6).
    pub async fn health_report(&self) -> Vec<(TransportId, Health)> {
        // TODO(M7:) `futures::join_all` the per-transport `health()` probes.
        todo!("M7: probe each transport.health() in order")
    }

    /// Walk the ordered transport list and return the first that reports
    /// [`Health::Ready`]. Health failures fall through with a visible notice;
    /// if none are healthy the caller keeps the **local** answer and queues
    /// nothing (doc 09 §3/§6). Returns a borrow so the caller does not move it
    /// out of the ordered list.
    pub async fn pick_healthy_transport(&self) -> Option<&dyn ReasoningTransport> {
        // TODO(M7:) for each transport in order: if `health().await` is Ready, return it;
        //           else emit the visible fall-through notice and continue.
        let _ = &self.transports;
        todo!("M7: first-healthy-wins fall-through over the ordered transport list")
    }

    /// The single egress chokepoint (doc 13 §2/§3).
    ///
    /// **Emits ONLY when `user_approved == true`.** This is the runtime backstop
    /// behind the preview gate: even though [`preview`] is the only thing that
    /// *sets* the flag, this method *re-checks* it before any byte leaves the
    /// machine. `user_approved == false` -> [`GatewayError::NotApproved`], no
    /// socket opened, no CLI spawned.
    ///
    /// On Send: picks the first healthy transport, transmits the approved
    /// payload, and records the `cloud_send` audit row with the SHA-256 of the
    /// wire bytes (doc 13 §3) via [`payload_builder`]. Returns the source-agnostic
    /// [`StructuredSuggestions`] (doc 09 §4); on transport failure the caller
    /// retains the local answer (doc 09 §6).
    pub async fn send_with_preview(
        &self,
        payload: &ContextPayload,
        user_approved: bool,
    ) -> Result<StructuredSuggestions, GatewayError> {
        // INVARIANT (doc 13 §2): the gateway is the ONLY emitter, and it emits ONLY
        // on explicit approval. Both conditions are checked here, before egress.
        if !user_approved || !payload.user_approved {
            return Err(GatewayError::NotApproved);
        }
        // TODO(M7:)
        //   1. let t = self.pick_healthy_transport().await.ok_or(NoHealthyTransport)?;
        //   2. write the `cloud_send` audit row: sha256(wire_bytes) + transport + byte count
        //      (doc 13 §3) — via payload_builder::record_cloud_send / aperture_privacy::audit_log.
        //   3. let raw = t.send(payload).await?;
        //   4. suggestion_validator::validate(raw, connectors) -> StructuredSuggestions (doc 09 §4).
        let _ = &self.transports;
        todo!("M7: pick healthy transport, audit-log the send, transmit, validate suggestions")
    }
}
