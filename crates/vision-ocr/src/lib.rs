//! Vision & OCR pipeline (doc 06).
//!
//! Two layers, deliberately separate so the cheap one is never blocked by the
//! expensive one:
//!
//! - **Layer A — cheap always-on OCR (Tier 0, CPU)** [`ocr_engine`] /
//!   [`windows_media_ocr`]: every sampled frame is downscaled, OCR'd, and the
//!   text + confidence written to `screen_context` (doc 06 §2). The OCR *text*
//!   — not the screenshot — is the context currency that gets embedded (doc 03
//!   §5) and feeds patterns and payloads.
//! - **Layer B — on-demand VLM (Tier 1, GPU, mutex)** [`vlm_layer`] /
//!   [`vlm_gating`]: only when the gate (doc 06 §4) says so, one downscaled
//!   image is enqueued as a `prio:50` GPU job through the orchestration
//!   scheduler; its structured-JSON result enriches `screen_context.vlm_summary`
//!   and the *next* pattern cycle. **The VLM never gates a bubble** (doc 02
//!   Path A invariant).
//!
//! ## Invariants honored here
//! - **8 GB VRAM ceiling / single GPU mutex:** this crate never touches the GPU
//!   directly — Layer B only [`enqueue`](aperture_contracts::gpu_job::GpuScheduler::enqueue)s
//!   a job; the orchestration crate owns the mutex and the projection check
//!   (doc 12 §3/§4). Refusals degrade to OCR-only (doc 06 §6).
//! - **Transparency gate:** nothing here opens a socket or spawns a CLI — only
//!   the reasoning-gateway crate may (doc 02 §2). The VLM call goes through the
//!   in-process scheduler trait, not the network.
//! - **Capture toggle:** raw frames are ephemeral (downscale → OCR → drop) and
//!   are *never* persisted; only OCR text, confidence, an optional VLM summary,
//!   and a perceptual hash reach the DB (doc 03 §3, doc 13).

// TODO(M2): Layer A — ocr_engine + windows_media_ocr + frame_processor + screen_context_writer.
// TODO(M5): Layer B — vlm_layer + vlm_gating wired to the real GpuScheduler.

pub mod frame_processor;
pub mod ocr_engine;
pub mod screen_context_writer;
pub mod vlm_gating;
pub mod vlm_layer;
pub mod windows_media_ocr;

pub use frame_processor::{FrameProcessor, ProcessedFrame};
pub use ocr_engine::{OcrEngine, OcrOutput};
pub use screen_context_writer::ScreenContextRow;
pub use vlm_gating::{should_wake_vlm, WakeReason};
pub use vlm_layer::{SceneJson, VlmLayer};

/// Errors surfaced by the vision pipeline. OCR and VLM failures are *soft* by
/// design: the pipeline downgrades (OCR-only, or no enrichment) rather than
/// propagating — see doc 06 §6.
#[derive(Debug, thiserror::Error)]
pub enum VisionError {
    /// The OCR engine failed on this frame (e.g. language pack missing; doc 06 §6).
    #[error("ocr engine error: {0}")]
    Ocr(String),
    /// Frame decode / downscale failed before OCR or VLM (doc 06 §2/§3).
    #[error("image pre-processing error: {0}")]
    Image(String),
    /// The VLM returned JSON that failed the scene schema even after one repair
    /// retry; the result is discarded (doc 06 §3). Advisory only — never fatal.
    #[error("vlm produced unusable output (schema-invalid after repair)")]
    VlmUnusable,
    /// The embedding step (doc 03 §5) failed for this frame's OCR text.
    #[error("embedding error: {0}")]
    Embed(String),
}
