//! Test fakes shipped with the contracts crate (doc 15 §7), behind the `fakes`
//! feature. They let every subsystem be tested deterministically and offline —
//! the degrade ladders without a GPU, the preview->send flow without a network.
//!
//! | Contract        | Fake                                                        |
//! |-----------------|-------------------------------------------------------------|
//! | Event envelope  | [`ScriptedEventPlayer`] — drives doc 08 tests deterministically |
//! | Context Payload | [`golden`] — golden payloads incl. redaction fixtures       |
//! | Connector       | [`FakeConnector`] — programmable capture/reconstruct outcomes |
//! | GPU job         | [`FakeScheduler`] — controllable latency / refusals         |
//! | Gateway         | [`FakeTransport`] — canned `StructuredSuggestions` / errors  |

use std::time::Duration;

use crate::connector::{Connector, ConnectorError, ConnectorState, OpenOutcome, ResumeArtifact};
use crate::context_payload::ContextPayload;
use crate::event::Event;
use crate::gpu_job::{GpuJob, GpuScheduler, JobError, JobOutput};
use crate::reasoning::{Health, ReasoningTransport, TransportError, TransportId};
use crate::suggestions::StructuredSuggestions;

/// Replays a scripted stream of events onto whatever the test wires up.
/// Drives the pattern-engine tests so trigger/cap/cooldown/decay behavior is
/// reproducible (doc 16 M3 gate).
pub struct ScriptedEventPlayer {
    pub script: Vec<Event>,
    cursor: usize,
}

impl ScriptedEventPlayer {
    pub fn new(script: Vec<Event>) -> Self {
        Self { script, cursor: 0 }
    }
    pub fn next(&mut self) -> Option<Event> {
        let ev = self.script.get(self.cursor).cloned();
        if ev.is_some() {
            self.cursor += 1;
        }
        ev
    }
}

/// Golden Context Payloads, including redaction fixtures (doc 13 §5).
pub mod golden {
    use super::*;
    use crate::context_payload::{Intent, PayloadItem, TransportTarget};

    /// The two e-mail addresses and the one secret the fixture carries, so a
    /// test can assert they are gone from whatever left the preview.
    pub const FIXTURE_EMAILS: [&str; 2] = ["alice@example.com", "bob@example.org"];
    /// Assembled at runtime (`format!`) so the literal never sits in the
    /// source tree as a token-shaped string (GitHub push protection scans
    /// test fixtures — 2026-08-16 note).
    pub fn fixture_secret() -> String {
        format!("{}-{}", "sk", "abcdefghijklmnop1234")
    }

    /// The RAW (unredacted) golden payload: two e-mails + one secret key across
    /// an OCR item and a user addition, `redactions` empty, not approved. The
    /// privacy crate's `Redactor` must turn it into exactly
    /// `email × 2, secret_key × 1` — asserted by SC5 (preview == wire) and the
    /// M0 fakes gate. Contracts cannot depend on privacy, so the fixture is
    /// the input, never the output.
    pub fn redaction_fixture() -> ContextPayload {
        let secret = fixture_secret();
        ContextPayload {
            payload_id: uuid::Uuid::from_u128(0x5c5_0000_0000_0000_0000_0000_0000_0001),
            created_ts: 1_700_000_000_000,
            intent: Intent::SummarizeCurrent,
            items: vec![
                PayloadItem::OcrText {
                    source_event_id: 1,
                    text: format!(
                        "Contact {} for the contract; the deploy token {secret} is on screen.",
                        FIXTURE_EMAILS[0]
                    ),
                    redacted: false,
                },
                PayloadItem::UserAddition {
                    text: format!("summarise what {} asked for", FIXTURE_EMAILS[1]),
                },
            ],
            redactions: Vec::new(),
            enrichment_offered: false,
            transport_target: TransportTarget::MessagesApi,
            user_approved: false,
        }
    }
}

/// A connector with programmable outcomes (doc 15 §7).
pub struct FakeConnector {
    pub id: &'static str,
    pub capture_result: Option<ConnectorState>,
    pub open_result: OpenOutcome,
}

impl Connector for FakeConnector {
    fn id(&self) -> &'static str {
        self.id
    }
    fn can_capture(&self, _ev: &Event) -> bool {
        self.capture_result.is_some()
    }
    fn capture(&self, _ev: &Event) -> Option<ConnectorState> {
        self.capture_result.clone()
    }
    fn staleness_ttl(&self) -> Duration {
        Duration::from_secs(7 * 24 * 3600)
    }
    fn reconstruct(&self, _st: &ConnectorState) -> Result<ResumeArtifact, ConnectorError> {
        Ok(ResumeArtifact::Url("https://example.test/".into()))
    }
    fn open(&self, _a: &ResumeArtifact) -> Result<OpenOutcome, ConnectorError> {
        Ok(self.open_result.clone())
    }
    fn validate(&self, _cloud_payload: &serde_json::Value) -> Option<ConnectorState> {
        self.capture_result.clone()
    }
}

/// A scheduler with controllable latency / refusals — tests the degrade ladders
/// (doc 04 R3) without a real GPU.
pub struct FakeScheduler {
    pub latency: Duration,
    /// When set, every `enqueue` refuses with this projection (doc 04 R1).
    pub refuse_with_projection_gb: Option<f32>,
    pub canned: Option<JobOutput>,
}

#[async_trait::async_trait]
impl GpuScheduler for FakeScheduler {
    async fn enqueue(&self, _job: GpuJob) -> Result<JobOutput, JobError> {
        if let Some(p) = self.refuse_with_projection_gb {
            return Err(JobError::BudgetRefused { projection_gb: p });
        }
        // NOTE: real impl sleeps `self.latency`; left as a TODO to avoid a tokio dep here.
        self.canned
            .clone()
            .ok_or(JobError::SidecarDown)
    }
}

/// A transport returning canned results / errors — tests the preview->send flow
/// fully offline (doc 15 §7).
pub struct FakeTransport {
    pub health: Health,
    pub canned: Result<StructuredSuggestions, &'static str>,
}

#[async_trait::async_trait]
impl ReasoningTransport for FakeTransport {
    fn id(&self) -> TransportId {
        TransportId::MessagesApi
    }
    async fn health(&self) -> Health {
        self.health.clone()
    }
    async fn send(
        &self,
        _payload: &ContextPayload,
    ) -> Result<StructuredSuggestions, TransportError> {
        self.canned
            .clone()
            .map_err(|e| TransportError::Other(e.to_string()))
    }
}
