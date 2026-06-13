//! Layer B — on-demand VLM scene understanding (doc 06 §3, M5).
//!
//! Model: Qwen2.5-VL (3B default / 7B opt-in, doc 04 §3) served by the
//! `vlm-host` sidecar. This crate **never** talks to the sidecar or the GPU
//! directly — it builds one [`GpuJob`](aperture_contracts::gpu_job::GpuJob) and
//! hands it to the orchestration-owned
//! [`GpuScheduler`](aperture_contracts::gpu_job::GpuScheduler) (the single GPU
//! mutex + projection check live there; doc 12 §3/§4). That is how the 8 GB
//! ceiling is enforced from one place.
//!
//! ## Hard invariants (doc 06 §3, doc 02 Path A)
//! - **Never blocks a bubble.** The result only enriches
//!   `screen_context.vlm_summary` and improves the *next* pattern cycle. A
//!   caller must `tokio::spawn` this off the bubble path; it returns nothing the
//!   UI is allowed to wait on.
//! - **One image per job**, downscaled ≤ 1024 px long edge, JPEG q85 (doc 06 §3 /
//!   gpu_job.rs `GpuJobKind::Vlm` — "image prefill is the silent killer", R2).
//! - **One repair retry, then discard.** Schema-invalid JSON ⇒ one re-ask with a
//!   repair instruction; still invalid ⇒ drop (the pipeline never blocks on the
//!   VLM, doc 06 §3).

use std::sync::Arc;

use aperture_contracts::gpu_job::{priority, GpuJob, GpuJobKind, GpuScheduler, JobOutput};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::VisionError;

/// VLM job deadline (doc 06 §6 / doc 12 §3) [ASSUMPTION]: cold-load slow ⇒ on
/// timeout cancel + log, never retried in a loop.
pub const VLM_DEADLINE: Duration = Duration::from_secs(10);

/// Max long edge for the image handed to the sidecar (doc 06 §3); enforces the
/// OOM rule R2.
pub const VLM_MAX_LONG_EDGE_PX: u32 = 1024;

/// JPEG quality for the single VLM image (doc 06 §3).
pub const VLM_JPEG_QUALITY: u8 = 85;

/// The system prompt — a screen-understanding *function*, not a chat (doc 06 §3).
pub const VLM_SYSTEM_PROMPT: &str = "You are a screen-understanding function. \
Given one screenshot of a Windows 11 desktop, return ONLY JSON matching the schema. \
Do not guess text you cannot read.";

/// Appended on the single repair attempt when the first response failed the
/// schema (doc 06 §3).
pub const VLM_REPAIR_INSTRUCTION: &str =
    "Your previous response was not valid JSON for the required schema. \
Return ONLY a single JSON object matching the schema, with no prose.";

/// The structured scene the VLM must return (doc 06 §3). Deserialized from the
/// sidecar's [`JobOutput::Vlm`] JSON; `resumable_hint` is **advisory only** —
/// connectors validate against their own captured state before any suggestion
/// uses it (doc 06 §6, RK hallucination).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneJson {
    /// Short scene description.
    pub scene: String,
    /// The VLM's guess at the foreground app.
    pub app_guess: String,
    /// Salient entities on screen.
    pub key_entities: Vec<KeyEntity>,
    /// Advisory connector hint — never trusted directly (doc 06 §6).
    pub resumable_hint: ResumableHint,
    /// What the cheap OCR likely missed (doc 06 §3).
    pub ocr_gaps: String,
    /// Self-reported confidence in `[0.0, 1.0]`.
    pub confidence: f32,
}

/// One entity the VLM extracted from the frame (doc 06 §3 schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEntity {
    /// `url | file | video | control | text` (doc 06 §3). Kept as a string for
    /// additive tolerance of unknown kinds (doc 15 §6).
    pub kind: String,
    pub value: String,
}

/// Advisory resume hint (doc 06 §3). `payload_guess` is free-form JSON the
/// owning connector must validate before use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumableHint {
    /// `browser | youtube | document | ide | none` (doc 06 §3).
    pub connector_type: String,
    #[serde(default)]
    pub payload_guess: serde_json::Value,
}

/// Builds VLM jobs and parses their results. Holds a handle to the
/// orchestration scheduler — the *only* sanctioned path to the GPU.
pub struct VlmLayer {
    scheduler: Arc<dyn GpuScheduler>,
}

impl VlmLayer {
    /// Wire the layer to the orchestration-owned scheduler (doc 12). In tests,
    /// pass `aperture_contracts::fakes::FakeScheduler` to exercise the
    /// budget-refused / sidecar-down degrade paths without a GPU.
    pub fn new(scheduler: Arc<dyn GpuScheduler>) -> Self {
        Self { scheduler }
    }

    /// Run scene understanding on one already-downscaled, JPEG-q85 image
    /// (≤ 1024 px long edge — see [`prepare_image`]). Builds a `prio:50`
    /// pattern-VLM job (cancellable; doc 12 §3), enqueues it, then parses +
    /// (once) repairs the JSON.
    ///
    /// Returns:
    /// - `Ok(SceneJson)` on first-pass or post-repair success;
    /// - `Err(VisionError::VlmUnusable)` if still schema-invalid after the one
    ///   repair, or if the scheduler refused / the job timed out — **all soft**:
    ///   the caller proceeds OCR-only and the bubble is unaffected (doc 06 §6).
    ///
    /// MUST be called off the bubble path (e.g. inside `tokio::spawn`).
    pub async fn understand(&self, image_jpeg: Vec<u8>) -> Result<SceneJson, VisionError> {
        // TODO(M5):
        //   1. let job = self.build_job(image_jpeg, /*repair=*/false);
        //   2. match self.scheduler.enqueue(job).await {
        //        Ok(JobOutput::Vlm(v)) => parse_scene(v) or one repair round,
        //        Err(BudgetRefused|Deadline|Cancelled|SidecarDown) => VlmUnusable (log + skip, doc 06 §6),
        //      }
        //   3. on parse failure: rebuild with VLM_REPAIR_INSTRUCTION, enqueue once
        //      more, parse; still bad => VlmUnusable. NEVER loop (doc 06 §3).
        let _ = (&self.scheduler, image_jpeg);
        todo!("M5: enqueue prio:50 VLM job, parse JSON, one repair retry, else discard")
    }

    /// Construct the GPU job for one image (doc 06 §3 / gpu_job.rs).
    /// `repair` switches the prompt to the repair instruction for the single retry.
    fn build_job(&self, image_jpeg: Vec<u8>, _repair: bool) -> GpuJob {
        // TODO(M5): compose the prompt (system + schema reminder, or repair text);
        // pattern-VLM is always priority::VLM_PATTERN and cancellable (doc 12 §3).
        GpuJob {
            kind: GpuJobKind::Vlm {
                image_jpeg,
                prompt: VLM_SYSTEM_PROMPT.to_string(),
            },
            priority: priority::VLM_PATTERN,
            deadline: VLM_DEADLINE,
        }
    }
}

/// Parse + validate a sidecar `JobOutput::Vlm` value against the scene schema
/// (doc 06 §3). Returns `VlmUnusable` on any schema violation so the caller can
/// decide whether to repair or discard.
pub fn parse_scene(_output: JobOutput) -> Result<SceneJson, VisionError> {
    // TODO(M5): expect JobOutput::Vlm(value); serde_json::from_value into
    // SceneJson; clamp confidence to [0,1]; reject if `scene`/`app_guess` empty.
    todo!("M5: parse + validate JobOutput::Vlm into SceneJson")
}

/// Downscale to ≤ 1024 px long edge and re-encode JPEG q85 for the sidecar
/// (doc 06 §3, R2). One image only.
pub fn prepare_image(_frame: &[u8]) -> Result<Vec<u8>, VisionError> {
    // TODO(M5): decode (via `image`) -> downscale to VLM_MAX_LONG_EDGE_PX long
    // edge -> encode JPEG at VLM_JPEG_QUALITY. Map errors to VisionError::Image.
    todo!("M5: downscale <=1024px + JPEG q85 for the single VLM image")
}
