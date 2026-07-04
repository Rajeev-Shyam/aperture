//! Contract 4 — the GPU job (doc 15 §4, semantics in doc 12 §3).
//!
//! Law: callers **never** touch the GPU, a sidecar, or VRAM accounting — they
//! only [`enqueue`](GpuScheduler::enqueue) a job and handle the result.
//! [`JobError::BudgetRefused`] carries the projection so callers can degrade
//! intelligently (doc 04 R3). `gpu_busy` is an observable broadcast derived from
//! the single-permit mutex state (doc 14's degrade contract keys off it).

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Four-tier priorities (doc 12 §3, ADR-031):
/// STT(voice)=100 > user-VLM(waiting on the answer now)=80
/// > enrichment-VLM("add screen summary" while composing)=70 > pattern-VLM=50.
pub mod priority {
    pub const STT_VOICE: u8 = 100;
    pub const VLM_USER: u8 = 80;
    /// ADR-031: the "Add screen summary" enrichment affordance — useful, but the
    /// user is composing, not blocked; slightly less latency-critical than user-VLM.
    pub const VLM_ENRICHMENT: u8 = 70;
    pub const VLM_PATTERN: u8 = 50;
}

#[derive(Debug, Clone)]
pub struct GpuJob {
    pub kind: GpuJobKind,
    pub priority: u8,
    /// Interim VLM 10 s / STT 15 s — real deadlines are set by the M5/M6 measured
    /// cold-load + inference times (doc 12 §3, ADR-031/Q33); expiry cancels + logs,
    /// never loops.
    pub deadline: Duration,
}

#[derive(Debug, Clone)]
pub enum GpuJobKind {
    /// One image only (R2: image prefill is the silent killer), downscaled
    /// <=1024 px long edge, JPEG q85 (doc 06 §3).
    Vlm { image_jpeg: Vec<u8>, prompt: String },
    /// 16 kHz mono PCM WAV. STT is never cancellable (doc 12 §3).
    Stt { wav: Vec<u8> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobOutput {
    /// Structured scene JSON (doc 06 §3).
    Vlm(serde_json::Value),
    Stt {
        transcript: String,
        avg_token_confidence: f32,
        duration_ms: u32,
    },
}

/// `BudgetRefused` carries the projection so the caller can pick a degrade rung
/// (doc 04 R3) instead of guessing.
#[derive(Debug, Clone, thiserror::Error)]
pub enum JobError {
    /// The cap is 7.0 GB and the projection counts co-resident weights (doc 04 R1, ADR-030).
    #[error("budget refused: projected {projection_gb:.2} GB > 7.0 GB ceiling")]
    BudgetRefused { projection_gb: f32 },
    #[error("deadline exceeded")]
    Deadline,
    #[error("cancelled by a higher-priority job")]
    Cancelled,
    #[error("sidecar is down")]
    SidecarDown,
}

/// The single entry point to GPU execution. The orchestration crate is the only
/// implementor; no other crate may construct a sidecar client (doc 12 §1).
///
/// The mutex-derived `gpu_busy` observable (doc 11 §6, doc 14 §5) is exposed by
/// the orchestration crate directly as a `tokio::sync::broadcast::Receiver<bool>`
/// — it is deliberately *not* on this trait so the contracts crate stays free of
/// an async-runtime dependency.
#[async_trait::async_trait]
pub trait GpuScheduler: Send + Sync {
    async fn enqueue(&self, job: GpuJob) -> Result<JobOutput, JobError>;
}
