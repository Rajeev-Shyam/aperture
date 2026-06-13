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
    /// TODO(M7): return a fixture payload whose redactions list is asserted by the
    /// preview-panel and SC5 tests (preview bytes == wire bytes).
    pub fn redaction_fixture() -> ContextPayload {
        todo!("M7: golden payload with email x2 / secret_key x1 redactions")
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
