//! STT GPU-job construction (doc 07 §3).
//!
//! Transcription is a GPU job **under the single mutex** (doc 12). This module
//! only *builds* the [`GpuJob`]; it is enqueued on the orchestration crate's
//! [`GpuScheduler`] — this crate never touches the GPU, a sidecar, or VRAM
//! accounting (doc 15 §4).
//!
//! Job spec (doc 07 §3): `{ kind: Stt, priority: 100 (highest) }`. Voice preempts
//! queued VLM jobs and is **never cancellable** (doc 12 §3) — STT runs to
//! completion or deadline. The orchestrator owns the **CPU fallback** to
//! Whisper-small when the GPU is unavailable (driver issue / projection refused,
//! doc 07 §3, §6); callers see only the [`JobOutput::Stt`] result.

use std::time::Duration;

use aperture_contracts::gpu_job::{priority, GpuJob, GpuJobKind};

/// STT deadline (doc 12 §3): 15 s [ASSUMPTION]. Expiry cancels + logs, never loops.
pub const STT_DEADLINE: Duration = Duration::from_secs(15);

/// Build the priority-100 STT [`GpuJob`] from a 16 kHz mono PCM WAV (doc 07 §3).
///
/// The returned job is handed to [`aperture_contracts::gpu_job::GpuScheduler::enqueue`];
/// preemption of lower-priority VLM jobs and CPU fallback are the orchestrator's
/// responsibility, not this crate's.
pub fn build(wav: Vec<u8>) -> GpuJob {
    GpuJob {
        kind: GpuJobKind::Stt { wav },
        // STT(voice)=100 is the highest priority (doc 12 §3) — preempts VLM.
        priority: priority::STT_VOICE,
        deadline: STT_DEADLINE,
    }
}

/// The transcription result the pipeline consumes, lifted out of
/// [`aperture_contracts::gpu_job::JobOutput::Stt`] (doc 15 §4) for ergonomics.
#[derive(Debug, Clone)]
pub struct Transcription {
    pub transcript: String,
    /// Average per-token confidence (drives the §4.4 confirm-chip gate, doc 07 §4).
    pub confidence: f32,
    pub duration_ms: u32,
}

impl Transcription {
    /// Lift a [`JobOutput`](aperture_contracts::gpu_job::JobOutput) into a
    /// [`Transcription`]; a non-STT output is a contract violation upstream.
    pub fn from_job_output(out: aperture_contracts::gpu_job::JobOutput) -> Option<Self> {
        match out {
            aperture_contracts::gpu_job::JobOutput::Stt {
                transcript,
                avg_token_confidence,
                duration_ms,
            } => Some(Self {
                transcript,
                confidence: avg_token_confidence,
                duration_ms,
            }),
            // TODO(M6:) treat a Vlm output here as a hard error (wrong job came back).
            _ => None,
        }
    }
}
