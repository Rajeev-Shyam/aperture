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
    /// The cloud response could not be validated into [`StructuredSuggestions`]
    /// (doc 09 §4); the caller falls back to the local answer / raw prose.
    #[error(transparent)]
    Validation(#[from] suggestion_validator::ValidationError),
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
    /// // TODO(M9:) concrete `aperture_privacy::audit_log::AuditLog` handle; M7
    /// computes + logs the record, M9 persists it to the encrypted DB.
    _audit: Arc<()>,
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
            _audit: Arc::new(()),
        }
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
        // 1. First healthy PUSH transport, or keep the local answer (nothing
        //    queued, doc 09 §6). Pull transports (MCP) are skipped here.
        let transport = self
            .pick_healthy_transport()
            .await
            .ok_or(GatewayError::NoHealthyTransport)?;
        let used_target = target_of(transport.id());

        // 2. Egress. The approved payload is transmitted here — this is the ONLY
        //    byte-moving call (doc 13 §2).
        let raw = transport.send(payload).await?;

        // 3. Audit AFTER a successful send (bytes actually left — never a phantom
        //    egress row on a failed send), over the transport's REAL wire bytes so
        //    the recorded SHA-256 matches what egressed (doc 13 §3, preview == wire).
        //    (M9 persists the row via aperture_privacy::audit_log::AuditLog.)
        let wire = transport.wire_bytes(payload);
        let record = payload_builder::record_cloud_send(payload, &wire, used_target);
        tracing::info!(
            payload_id = %record.payload_id,
            sha256 = %record.wire_sha256,
            bytes = record.byte_count,
            transport = ?record.transport,
            "cloud_send (M9: persist via aperture_privacy::audit_log)"
        );

        // 4. Re-validate every suggestion against its target connector — the cloud
        //    suggests, only connectors act (doc 09 §4).
        let validated = suggestion_validator::validate(raw, self.connectors.as_ref())?;
        Ok(validated)
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
        assert_eq!(out.answer_text.as_deref(), Some("from the ready transport"));
    }

    #[tokio::test]
    async fn no_healthy_transport_keeps_the_local_answer() {
        let g = gateway(vec![transport(
            Health::NeedsSetup("log in".into()),
            StructuredSuggestions { suggestions: vec![], answer_text: None },
        )]);
        assert!(matches!(
            g.send_with_preview(&approved_payload(), true).await,
            Err(GatewayError::NoHealthyTransport)
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
        assert!(out.suggestions.is_empty(), "no connector accepted it");
        assert!(out.answer_text.unwrap().contains("Open the deploy dashboard"));
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
        assert_eq!(out.answer_text.as_deref(), Some("via push"), "reached the push transport");
    }
}
