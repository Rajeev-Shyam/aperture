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
//! (default Desktop-MCP -> CLI -> API, ADR-025 / doc 09 §3), picks the first healthy one,
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
    TransportTarget,
};

/// Errors raised by the gateway itself (distinct from a single transport's
/// [`TransportError`], doc 09 §6).
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    /// No transport in the ordered list reported [`Health::Ready`]; the local
    /// answer stands and nothing is queued (doc 09 §6). Raised by callers of
    /// [`Gateway::pick_healthy_transport`]; `send_with_preview` reports the same
    /// situation as [`GatewayError::TransportMismatch`] with `available: None`,
    /// because the user's consent named a transport and THAT is what is missing.
    #[error("no healthy transport; local answer stands")]
    NoHealthyTransport,
    /// `send_with_preview` was called with `user_approved == false`. The gateway
    /// refuses to emit — only [`preview`] may flip the flag (doc 13 §2/§3).
    #[error("refusing to send: payload not user-approved (two-emitter rule, doc 13 §2)")]
    NotApproved,
    /// The transport the preview NAMED (`payload.transport_target` — the footer
    /// line the user read before pressing Send) is not configured or not
    /// [`Health::Ready`], so nothing was sent (SDLC review 2026-08-19, finding 1).
    /// The approval is bound to that transport the way the MCP release gate
    /// binds its payloads: a Send never falls through to a transport the user
    /// did not consent to — in the shipped default order that "next one" is the
    /// metered Messages API key (ADR-010). `available` is the first Ready push
    /// transport in settings order, if any, so the shell can OFFER it as an
    /// explicit retarget + re-approve; it is never used silently.
    #[error("named transport {named:?} is not ready; nothing sent (available: {available:?})")]
    TransportMismatch {
        named: TransportTarget,
        available: Option<TransportTarget>,
    },
    /// The chosen transport failed; see the wrapped error.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The cloud response could not be validated into [`StructuredSuggestions`]
    /// (doc 09 §4); the caller falls back to the local answer / raw prose.
    #[error(transparent)]
    Validation(#[from] suggestion_validator::ValidationError),
}

/// The result of a successful Send (decision #41): the validated suggestions plus
/// whether the `cloud_send` audit row actually persisted. An audit failure never
/// fails the Send (the bytes already left — reporting the egress as failed would
/// be a worse lie than a missing row), but it must not be silent either: the
/// shell surfaces `audit_failure` as a visible UI warning.
#[derive(Debug)]
pub struct SendOutcome {
    /// The re-validated cloud response (doc 09 §4).
    pub suggestions: StructuredSuggestions,
    /// `Some(reason)` when the `cloud_send` audit row failed to persist AFTER a
    /// successful egress — the sole "what left this machine?" trail is missing
    /// this send (doc 13 §3).
    pub audit_failure: Option<String>,
}

/// The reasoning gateway (doc 09 §2). Owns the ordered, swappable transport list
/// and the single egress chokepoint.
pub struct Gateway {
    /// Ordered from settings; the gateway picks the first healthy one and falls
    /// through on health failure (doc 09 §3). Default order: Desktop-MCP -> CLI
    /// -> API (MCP-primary, ADR-025).
    transports: Vec<Box<dyn ReasoningTransport>>,
    /// Connector registry seam (doc 09 §4): the cloud can only *suggest*; every
    /// returned `reconstruct_payload` is re-validated here before a bubble offers it.
    connectors: Box<dyn suggestion_validator::ConnectorLookup>,
    /// Audit sink for the `cloud_send` row written at Send (doc 13 §3, doc 09 §5).
    /// Defaults to [`aperture_privacy::audit_log::NullAuditSink`]; the shell
    /// injects the DB-backed `AuditLog` via [`Gateway::with_audit`] (M9).
    audit: Arc<dyn aperture_privacy::audit_log::AuditSink>,
}

impl Gateway {
    /// Build a gateway from the settings-ordered transport list (doc 09 §3) and the
    /// connector registry (doc 09 §4). The order is authoritative:
    /// `pick_healthy_transport` walks it front-to-back.
    pub fn new(
        transports: Vec<Box<dyn ReasoningTransport>>,
        connectors: Box<dyn suggestion_validator::ConnectorLookup>,
    ) -> Self {
        Self {
            transports,
            connectors,
            audit: Arc::new(aperture_privacy::audit_log::NullAuditSink),
        }
    }

    /// Inject the persistent audit sink (M9, doc 13 §3). Without this the
    /// gateway still *computes* and logs every `cloud_send` record — it just has
    /// nowhere durable to put it.
    pub fn with_audit(mut self, audit: Arc<dyn aperture_privacy::audit_log::AuditSink>) -> Self {
        self.audit = audit;
        self
    }

    /// Report the health of every configured transport, in settings order — feeds
    /// the preview's "transport target" line and the fall-through notice (doc 09 §6).
    pub async fn health_report(&self) -> Vec<(TransportId, Health)> {
        let mut report = Vec::with_capacity(self.transports.len());
        for transport in &self.transports {
            report.push((transport.id(), transport.health().await));
        }
        report
    }

    /// Walk the ordered transport list and return the first that is
    /// [`Health::Ready`] **and push-capable**. Health failures fall through with a
    /// visible notice; **pull transports (MCP) are skipped on this push path** —
    /// they serve the Claude-initiated tool-handler flow, not `send_with_preview`
    /// (doc 09 §3, the push/pull asymmetry). If none qualify the caller keeps the
    /// **local** answer and queues nothing (doc 09 §6). Returns a borrow so the
    /// caller does not move it out of the ordered list.
    pub async fn pick_healthy_transport(&self) -> Option<&dyn ReasoningTransport> {
        for transport in &self.transports {
            if !transport.supports_push() {
                tracing::debug!(transport = ?transport.id(), "skipping pull-only transport on the push Send path (doc 09 §3)");
                continue;
            }
            match transport.health().await {
                Health::Ready => return Some(transport.as_ref()),
                other => tracing::info!(
                    transport = ?transport.id(),
                    status = ?other,
                    "transport not ready; falling through (doc 09 §6)"
                ),
            }
        }
        None
    }

    /// The push transport the approval was GIVEN for (SDLC review 2026-08-19,
    /// finding 1): the Ready, push-capable transport whose target equals
    /// `named` — `payload.transport_target`, the line the preview footer showed.
    ///
    /// Mirrors the MCP release gate's binding (`mcp_bridge`): consent names one
    /// transport, and a Send may use that one or none. When it is absent or not
    /// Ready, the error carries the first Ready push transport in settings
    /// order as `available`, so the shell can offer an explicit retarget — the
    /// fall-through is a user decision, never the gateway's.
    async fn pick_named_transport(
        &self,
        named: TransportTarget,
    ) -> Result<&dyn ReasoningTransport, GatewayError> {
        let mut available = None;
        for transport in &self.transports {
            if !transport.supports_push() {
                tracing::debug!(transport = ?transport.id(), "skipping pull-only transport on the push Send path (doc 09 §3)");
                continue;
            }
            let target = target_of(transport.id());
            match transport.health().await {
                Health::Ready if target == named => return Ok(transport.as_ref()),
                Health::Ready => {
                    if available.is_none() {
                        available = Some(target);
                    }
                }
                other => tracing::info!(
                    transport = ?transport.id(),
                    status = ?other,
                    "transport not ready (doc 09 §6)"
                ),
            }
        }
        tracing::warn!(
            ?named,
            ?available,
            "the transport the preview named is not ready; refusing to fall through (review finding 1)"
        );
        Err(GatewayError::TransportMismatch { named, available })
    }

    /// The single egress chokepoint (doc 13 §2/§3).
    ///
    /// **Emits ONLY when `user_approved == true`.** This is the runtime backstop
    /// behind the preview gate: even though [`preview`] is the only thing that
    /// *sets* the flag, this method *re-checks* it before any byte leaves the
    /// machine. `user_approved == false` -> [`GatewayError::NotApproved`], no
    /// socket opened, no CLI spawned.
    ///
    /// On Send: uses the transport the preview NAMED (`payload.transport_target`)
    /// and only that one — a transport the user did not see is never used, even
    /// if it is the only Ready one ([`GatewayError::TransportMismatch`], review
    /// finding 1). It transmits the approved payload and records the
    /// `cloud_send` audit row with the SHA-256 of the wire bytes (doc 13 §3) via
    /// [`payload_builder`]. Returns the source-agnostic [`StructuredSuggestions`]
    /// (doc 09 §4); on transport failure the caller retains the local answer
    /// (doc 09 §6).
    pub async fn send_with_preview(
        &self,
        payload: &ContextPayload,
        user_approved: bool,
    ) -> Result<SendOutcome, GatewayError> {
        // INVARIANT (doc 13 §2): the gateway is the ONLY emitter, and it emits ONLY
        // on explicit approval. Both conditions are checked here, before egress.
        if !user_approved || !payload.user_approved {
            return Err(GatewayError::NotApproved);
        }
        // 1. The NAMED push transport, Ready — or nothing (review finding 1).
        //    Pull transports (MCP) are skipped here; a not-ready named
        //    transport is a typed refusal, not a fall-through.
        let transport = self.pick_named_transport(payload.transport_target).await?;
        let used_target = target_of(transport.id());

        // 2. Per-transport HARD cap (decision #42), checked on the transport's
        //    REAL wire bytes BEFORE any byte moves. Distinct from the 50 KB soft
        //    preview warning (doc 09 §5); a violation is a hard stop naming the
        //    transport and the size — never an auto-shrink (decision #40).
        let wire = transport.wire_bytes(payload);
        transports::check_hard_cap(transport.id(), wire.len())?;

        // 3. Egress. The approved payload is transmitted here — this is the ONLY
        //    byte-moving call (doc 13 §2).
        let raw = transport.send(payload).await?;

        // 4. Audit AFTER a successful send (bytes actually left — never a phantom
        //    egress row on a failed send), over the transport's REAL wire bytes so
        //    the recorded SHA-256 matches what egressed (doc 13 §3, preview == wire).
        let record = payload_builder::record_cloud_send(payload, &wire, used_target);
        tracing::info!(
            payload_id = %record.payload_id,
            sha256 = %record.wire_sha256,
            bytes = record.byte_count,
            transport = ?record.transport,
            "cloud_send"
        );
        // A failed audit write does NOT fail the Send: the bytes have already
        // left, so erroring here would report a send that happened as one that
        // did not — a worse lie than a missing row. It propagates on the Ok path
        // instead (decision #41) so the shell shows a visible warning; the
        // accountability gap is real and must reach the user, not just the log.
        let audit_failure = match self.audit.record_cloud_send(record) {
            Ok(()) => None,
            Err(e) => {
                tracing::error!(%e, "cloud_send audit row FAILED to persist (doc 13 §3) — egress happened, the trail is incomplete");
                Some(e.to_string())
            }
        };

        // 5. Re-validate every suggestion against its target connector — the cloud
        //    suggests, only connectors act (doc 09 §4).
        let validated = suggestion_validator::validate(raw, self.connectors.as_ref())?;
        Ok(SendOutcome { suggestions: validated, audit_failure })
    }
}

/// Map a [`TransportId`] to its [`TransportTarget`] twin (parallel enums) so the
/// audit records the transport that **actually** egressed, not the payload's
/// (possibly-fallen-through) intended target.
fn target_of(id: TransportId) -> aperture_contracts::TransportTarget {
    use aperture_contracts::TransportTarget as T;
    match id {
        TransportId::ClaudeCli => T::ClaudeCli,
        TransportId::ClaudeDesktopMcp => T::ClaudeDesktopMcp,
        TransportId::MessagesApi => T::MessagesApi,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_contracts::fakes::FakeTransport;
    use aperture_contracts::suggestions::CloudSuggestion;
    use aperture_contracts::{
        Connector, ContextPayload as Payload, Intent, PayloadItem, StructuredSuggestions,
        TransportTarget,
    };
    use suggestion_validator::ConnectorLookup;

    /// A connector lookup that accepts nothing (all suggestions degrade to text) —
    /// enough to exercise the send/validate flow without the real registry.
    struct NoConnectors;
    impl ConnectorLookup for NoConnectors {
        fn by_type(&self, _t: &str) -> Option<&dyn Connector> {
            None
        }
    }

    fn approved_payload() -> Payload {
        Payload {
            payload_id: uuid::Uuid::nil(),
            created_ts: 0,
            intent: Intent::AnswerQuery,
            items: vec![PayloadItem::UserAddition { text: "summarize this".into() }],
            redactions: vec![],
            enrichment_offered: false,
            transport_target: TransportTarget::MessagesApi,
            user_approved: true, // as the preview gate would leave it on Send
        }
    }

    fn transport(health: Health, canned: StructuredSuggestions) -> Box<dyn ReasoningTransport> {
        Box::new(FakeTransport { health, canned: Ok(canned) })
    }

    fn gateway(transports: Vec<Box<dyn ReasoningTransport>>) -> Gateway {
        Gateway::new(transports, Box::new(NoConnectors))
    }

    #[tokio::test]
    async fn refuses_to_send_an_unapproved_payload() {
        let g = gateway(vec![transport(Health::Ready, StructuredSuggestions { suggestions: vec![], answer_text: None })]);
        let mut p = approved_payload();
        p.user_approved = false;
        // Both the arg and the flag are re-checked (two-emitter backstop, doc 13 §2).
        assert!(matches!(g.send_with_preview(&p, true).await, Err(GatewayError::NotApproved)));
        assert!(matches!(g.send_with_preview(&approved_payload(), false).await, Err(GatewayError::NotApproved)));
    }

    #[tokio::test]
    async fn falls_through_unhealthy_transports_to_the_first_ready_one() {
        let down = transport(
            Health::Unavailable("offline".into()),
            StructuredSuggestions { suggestions: vec![], answer_text: Some("SHOULD NOT BE USED".into()) },
        );
        let ready = transport(
            Health::Ready,
            StructuredSuggestions { suggestions: vec![], answer_text: Some("from the ready transport".into()) },
        );
        let g = gateway(vec![down, ready]);
        let out = g.send_with_preview(&approved_payload(), true).await.unwrap();
        assert_eq!(out.suggestions.answer_text.as_deref(), Some("from the ready transport"));
        assert!(out.audit_failure.is_none(), "the NullAuditSink never fails");
    }

    #[tokio::test]
    async fn no_healthy_transport_keeps_the_local_answer() {
        let g = gateway(vec![transport(
            Health::NeedsSetup("log in".into()),
            StructuredSuggestions { suggestions: vec![], answer_text: None },
        )]);
        // Review finding 1: the refusal names the transport the user consented
        // to, with nothing to offer instead.
        assert!(matches!(
            g.send_with_preview(&approved_payload(), true).await,
            Err(GatewayError::TransportMismatch {
                named: TransportTarget::MessagesApi,
                available: None
            })
        ));
    }

    // --- Review finding 1 (2026-08-19): the push Send is bound to the named
    // transport. `FakeTransport` always reports `MessagesApi`, so these use a
    // transport whose id is configurable and which records whether it sent.

    struct IdTransport {
        id: TransportId,
        health: Health,
        sent: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    #[async_trait::async_trait]
    impl ReasoningTransport for IdTransport {
        fn id(&self) -> TransportId {
            self.id
        }
        async fn health(&self) -> Health {
            self.health.clone()
        }
        async fn send(&self, _p: &Payload) -> Result<StructuredSuggestions, TransportError> {
            self.sent.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(StructuredSuggestions { suggestions: vec![], answer_text: Some(format!("{:?}", self.id)) })
        }
    }

    fn id_transport(
        id: TransportId,
        health: Health,
    ) -> (Box<dyn ReasoningTransport>, std::sync::Arc<std::sync::atomic::AtomicBool>) {
        let sent = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        (Box::new(IdTransport { id, health, sent: std::sync::Arc::clone(&sent) }), sent)
    }

    /// A recording audit sink: finding 1's refusal must leave NO `cloud_send` row.
    #[derive(Default)]
    struct AuditSpy(std::sync::Mutex<usize>);
    impl aperture_privacy::audit_log::AuditSink for AuditSpy {
        fn record_cloud_send(
            &self,
            _rec: aperture_privacy::audit_log::CloudSendRecord,
        ) -> Result<(), aperture_privacy::PrivacyError> {
            *self.0.lock().unwrap() += 1;
            Ok(())
        }
    }

    /// (a) The named transport is Ready ⇒ the Send uses it — even though another
    /// push transport is Ready too, and even though the named one is not first.
    #[tokio::test]
    async fn f1_send_uses_the_transport_the_preview_named() {
        use std::sync::atomic::Ordering;
        let (api, api_sent) = id_transport(TransportId::MessagesApi, Health::Ready);
        let (cli, cli_sent) = id_transport(TransportId::ClaudeCli, Health::Ready);
        let g = gateway(vec![api, cli]);
        let mut p = approved_payload();
        p.transport_target = TransportTarget::ClaudeCli;
        let out = g.send_with_preview(&p, true).await.unwrap();
        assert_eq!(out.suggestions.answer_text.as_deref(), Some("ClaudeCli"));
        assert!(cli_sent.load(Ordering::SeqCst), "sent over the named transport");
        assert!(!api_sent.load(Ordering::SeqCst), "the un-named transport was never touched");
    }

    /// (b) The named transport is NeedsSetup while another push transport is
    /// Ready ⇒ a typed refusal that OFFERS the other one; nothing sent, no
    /// audit row. This is the ADR-010 failure: `claude` off PATH must not
    /// silently bill the Messages API key.
    #[tokio::test]
    async fn f1_a_not_ready_named_transport_refuses_instead_of_falling_through() {
        use std::sync::atomic::Ordering;
        let (cli, cli_sent) = id_transport(TransportId::ClaudeCli, Health::NeedsSetup("not on PATH".into()));
        let (api, api_sent) = id_transport(TransportId::MessagesApi, Health::Ready);
        let spy = std::sync::Arc::new(AuditSpy::default());
        let g = gateway(vec![cli, api])
            .with_audit(std::sync::Arc::clone(&spy) as std::sync::Arc<dyn aperture_privacy::audit_log::AuditSink>);
        let mut p = approved_payload();
        p.transport_target = TransportTarget::ClaudeCli;
        let err = g.send_with_preview(&p, true).await.unwrap_err();
        assert!(
            matches!(
                err,
                GatewayError::TransportMismatch {
                    named: TransportTarget::ClaudeCli,
                    available: Some(TransportTarget::MessagesApi)
                }
            ),
            "got {err:?}"
        );
        assert!(!cli_sent.load(Ordering::SeqCst) && !api_sent.load(Ordering::SeqCst), "NOTHING sent");
        assert_eq!(*spy.0.lock().unwrap(), 0, "no audit row for a refused Send");
    }

    /// (c) Nothing Ready ⇒ the refusal has nothing to offer (`available: None`).
    #[tokio::test]
    async fn f1_nothing_ready_reports_no_alternative() {
        let (cli, _) = id_transport(TransportId::ClaudeCli, Health::NeedsSetup("x".into()));
        let (api, _) = id_transport(TransportId::MessagesApi, Health::Unavailable("offline".into()));
        let g = gateway(vec![cli, api]);
        let mut p = approved_payload();
        p.transport_target = TransportTarget::ClaudeCli;
        assert!(matches!(
            g.send_with_preview(&p, true).await,
            Err(GatewayError::TransportMismatch { named: TransportTarget::ClaudeCli, available: None })
        ));
    }

    #[tokio::test]
    async fn send_degrades_unactionable_cloud_suggestions_to_text() {
        // The cloud returns an actionable-looking suggestion, but NoConnectors
        // accepts none, so it must fold into answer_text (doc 09 §4).
        let canned = StructuredSuggestions {
            suggestions: vec![CloudSuggestion {
                title: "Open the deploy dashboard".into(),
                connector_type: "browser".into(),
                reconstruct_payload: serde_json::json!({ "url": "x" }),
                rationale: "you asked about the deploy".into(),
            }],
            answer_text: None,
        };
        let g = gateway(vec![transport(Health::Ready, canned)]);
        let out = g.send_with_preview(&approved_payload(), true).await.unwrap();
        assert!(out.suggestions.suggestions.is_empty(), "no connector accepted it");
        assert!(out.suggestions.answer_text.unwrap().contains("Open the deploy dashboard"));
    }

    #[tokio::test]
    async fn health_report_lists_every_transport_in_order() {
        let g = gateway(vec![
            transport(Health::Unavailable("x".into()), StructuredSuggestions { suggestions: vec![], answer_text: None }),
            transport(Health::Ready, StructuredSuggestions { suggestions: vec![], answer_text: None }),
        ]);
        let report = g.health_report().await;
        assert_eq!(report.len(), 2);
        assert!(matches!(report[0].1, Health::Unavailable(_)));
        assert!(matches!(report[1].1, Health::Ready));
    }

    // --- SC5 (doc 13 §2, doc 16 M7 "strict") — the CPU-checkable half ---------
    // The byte-level monitor (ETW / mitmproxy) is the on-hardware companion
    // (`gates/tests/sc5_network_monitor.rs`, `#[ignore]`). These prove the two
    // properties that don't need a monitor: preview == wire by hash, and zero
    // egress until an approved Send.

    /// A transport that trips a flag the instant `send` is called — the in-process
    /// egress point. If it trips without approval, SC5 is violated.
    struct Tripwire {
        sent: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    #[async_trait::async_trait]
    impl ReasoningTransport for Tripwire {
        fn id(&self) -> aperture_contracts::TransportId {
            aperture_contracts::TransportId::MessagesApi
        }
        async fn health(&self) -> Health {
            Health::Ready
        }
        async fn send(&self, _p: &Payload) -> Result<StructuredSuggestions, TransportError> {
            self.sent.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(StructuredSuggestions { suggestions: vec![], answer_text: None })
        }
    }

    #[tokio::test]
    async fn sc5_no_bytes_move_until_an_approved_send() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let sent = std::sync::Arc::new(AtomicBool::new(false));
        let g = Gateway::new(
            vec![Box::new(Tripwire { sent: std::sync::Arc::clone(&sent) })],
            Box::new(NoConnectors),
        );

        // Unapproved (both the flag false AND the arg false) → refused, transport untouched.
        let mut unapproved = approved_payload();
        unapproved.user_approved = false;
        assert!(g.send_with_preview(&unapproved, true).await.is_err());
        assert!(g.send_with_preview(&approved_payload(), false).await.is_err());
        assert!(!sent.load(Ordering::SeqCst), "SC5 VIOLATION: bytes moved before an approved Send");

        // Approved → the transport is finally reached.
        g.send_with_preview(&approved_payload(), true).await.unwrap();
        assert!(sent.load(Ordering::SeqCst), "an approved Send must actually transmit");
    }

    /// M9 (doc 13 §3): a successful Send hands the `cloud_send` record to the
    /// injected audit sink — the trail that answers "what left this machine?".
    /// A refused Send must record NOTHING (no phantom egress row).
    #[tokio::test]
    async fn m9_audit_sink_receives_exactly_the_sends_that_egressed() {
        use aperture_privacy::audit_log::{AuditSink, CloudSendRecord};
        use std::sync::Mutex;

        #[derive(Default)]
        struct Spy(Mutex<Vec<CloudSendRecord>>);
        impl AuditSink for Spy {
            fn record_cloud_send(
                &self,
                rec: CloudSendRecord,
            ) -> Result<(), aperture_privacy::PrivacyError> {
                self.0.lock().unwrap().push(rec);
                Ok(())
            }
        }

        let spy = std::sync::Arc::new(Spy::default());
        let canned = StructuredSuggestions { suggestions: vec![], answer_text: None };
        let g = gateway(vec![transport(Health::Ready, canned)])
            .with_audit(std::sync::Arc::clone(&spy) as std::sync::Arc<dyn AuditSink>);

        // Refused send: nothing egressed, so nothing may be audited.
        assert!(g.send_with_preview(&approved_payload(), false).await.is_err());
        assert!(spy.0.lock().unwrap().is_empty(), "a refused Send must not leave an audit row");

        let payload = approved_payload();
        g.send_with_preview(&payload, true).await.unwrap();
        let rows = spy.0.lock().unwrap();
        assert_eq!(rows.len(), 1, "one successful Send => exactly one audit row");
        assert_eq!(rows[0].payload_id, payload.payload_id);
        assert_eq!(rows[0].wire_sha256.len(), 64);
        assert!(rows[0].byte_count > 0);
    }

    /// An audit-write failure must not turn a completed Send into a reported
    /// failure — the bytes already left; lying about that is worse than a gap.
    /// Decision #41: the failure rides the Ok path so the shell can SHOW it.
    #[tokio::test]
    async fn m9_a_failing_audit_sink_does_not_fail_the_send_but_surfaces() {
        use aperture_privacy::audit_log::{AuditSink, CloudSendRecord};
        struct Broken;
        impl AuditSink for Broken {
            fn record_cloud_send(
                &self,
                _rec: CloudSendRecord,
            ) -> Result<(), aperture_privacy::PrivacyError> {
                Err(aperture_privacy::PrivacyError::Audit("disk full".into()))
            }
        }
        let canned = StructuredSuggestions { suggestions: vec![], answer_text: None };
        let g = gateway(vec![transport(Health::Ready, canned)])
            .with_audit(std::sync::Arc::new(Broken) as std::sync::Arc<dyn AuditSink>);
        let out = g
            .send_with_preview(&approved_payload(), true)
            .await
            .expect("egress succeeded; the Send result must reflect that");
        let reason = out.audit_failure.expect("#41: the audit failure must propagate, not vanish");
        assert!(reason.contains("disk full"), "carries the sink's reason: {reason}");
    }

    /// Decision #42: an oversized payload is refused BEFORE any byte moves, with
    /// an error naming the transport and the size — and never auto-shrunk (#40).
    #[tokio::test]
    async fn c42_hard_cap_refuses_oversized_payloads_before_any_egress() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let sent = std::sync::Arc::new(AtomicBool::new(false));
        let g = Gateway::new(
            vec![Box::new(Tripwire { sent: std::sync::Arc::clone(&sent) })],
            Box::new(NoConnectors),
        );
        // Tripwire reports TransportId::MessagesApi; its default wire_bytes is
        // the payload serialization — pad one item past the 20 MB API cap.
        let cap = transports::hard_cap_bytes(aperture_contracts::TransportId::MessagesApi);
        let mut p = approved_payload();
        p.items = vec![PayloadItem::UserAddition { text: "x".repeat(cap) }];
        let err = g.send_with_preview(&p, true).await.unwrap_err();
        assert!(
            matches!(err, GatewayError::Transport(TransportError::PayloadTooLarge(_))),
            "hard-stop error, got {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("Messages API"), "names the transport: {msg}");
        assert!(msg.contains(&cap.to_string()), "names the cap: {msg}");
        assert!(!sent.load(Ordering::SeqCst), "#42: no byte may move on a cap violation");
    }

    #[tokio::test]
    async fn sc5_preview_bytes_equal_wire_bytes_by_hash() {
        use aperture_contracts::PayloadItem;
        let redactor = aperture_privacy::redaction::Redactor::new(&[]).unwrap();
        // Assemble + preview: the bytes the user sees.
        let (payload, _) = payload_builder::build(
            Intent::SummarizeCurrent,
            vec![PayloadItem::UserAddition { text: "summarize this thread".into() }],
            TransportTarget::MessagesApi,
            &redactor,
            0,
        )
        .unwrap();
        let preview_hash = aperture_privacy::audit_log::sha256_hex(&serde_json::to_vec(&payload).unwrap());

        // Approve via the sole gate; `user_approved` is skip_serialized, so the wire
        // serialization is byte-identical to the previewed one.
        let approved = preview::PreviewSession::new(payload)
            .approve(preview::PreviewDecision::Send)
            .expect("Send yields the approved payload");
        let wire = serde_json::to_vec(&approved).unwrap();
        let record = payload_builder::record_cloud_send(&approved, &wire, TransportTarget::MessagesApi);

        assert_eq!(
            record.wire_sha256, preview_hash,
            "SC5: the canonical payload serialization is stable preview->wire (doc 13 §3)"
        );
    }

    /// SC5 (#3 fix): the AUDITED bytes are the transport's REAL egress bytes, not a
    /// bare payload re-serialization — and they embed exactly the approved payload's
    /// user-data (plus only known non-user-data envelope).
    #[tokio::test]
    async fn sc5_audit_hashes_the_transports_actual_wire_bytes() {
        use aperture_contracts::PayloadItem;
        use transports::api::{ApiSettings, ApiTransport};
        use transports::cli::CliTransport;
        let redactor = aperture_privacy::redaction::Redactor::new(&[]).unwrap();
        let (payload, _) = payload_builder::build(
            Intent::AnswerQuery,
            vec![PayloadItem::UserAddition { text: "SENTINEL-USER-DATA-42".into() }],
            TransportTarget::MessagesApi,
            &redactor,
            0,
        )
        .unwrap();

        // Each push transport's wire_bytes embed the approved user-data verbatim...
        let api = ApiTransport::new(
            ApiSettings {
                endpoint: "https://x/v1/messages".into(),
                model: "claude-opus-4-8".into(),
                anthropic_version: "2023-06-01".into(),
                beta_headers: vec![],
                cache_ttl: "5m".into(),
                max_tokens: 512,
            },
            "key",
        );
        let cli = CliTransport::new("claude");
        for wire in [api.wire_bytes(&payload), cli.wire_bytes(&payload)] {
            let s = String::from_utf8(wire.clone()).unwrap();
            assert!(s.contains("SENTINEL-USER-DATA-42"), "wire embeds the approved user-data");
            // ...and are NOT the bare payload serialization (they carry the envelope),
            // proving the old serde_json(payload) hash would NOT have matched egress.
            assert_ne!(wire, serde_json::to_vec(&payload).unwrap(), "wire != bare payload");
            // The audit hashes exactly those egress bytes.
            let rec = payload_builder::record_cloud_send(&payload, &wire, TransportTarget::MessagesApi);
            assert_eq!(rec.wire_sha256, aperture_privacy::audit_log::sha256_hex(&wire));
        }
    }

    /// #4 fix: under the default MCP-primary order, a Ready *pull* transport is
    /// skipped on the push Send path and the first Ready *push* transport is used.
    #[tokio::test]
    async fn push_send_skips_a_ready_pull_transport() {
        struct PullReady;
        #[async_trait::async_trait]
        impl ReasoningTransport for PullReady {
            fn id(&self) -> aperture_contracts::TransportId {
                aperture_contracts::TransportId::ClaudeDesktopMcp
            }
            fn supports_push(&self) -> bool {
                false
            }
            async fn health(&self) -> Health {
                Health::Ready
            }
            async fn send(&self, _p: &Payload) -> Result<StructuredSuggestions, TransportError> {
                panic!("SC/ADR-025 VIOLATION: the pull transport was pushed to");
            }
        }
        let g = gateway(vec![
            Box::new(PullReady),
            transport(
                Health::Ready,
                StructuredSuggestions { suggestions: vec![], answer_text: Some("via push".into()) },
            ),
        ]);
        let out = g.send_with_preview(&approved_payload(), true).await.unwrap();
        assert_eq!(
            out.suggestions.answer_text.as_deref(),
            Some("via push"),
            "reached the push transport"
        );
    }
}
