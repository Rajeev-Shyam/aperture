//! The Layer-A per-frame flow (doc 06 §5).
//!
//! ```text
//! frame ──► downscale ──► OCR ──► screen_context.ocr_text ──► embed (doc 03)
//!                           │
//!                           └─ gate(§4)? ──► GPU job {kind:VLM, prio:50} ──► doc 12 mutex
//!                                                   └─► vlm-host ──► JSON ──► screen_context.vlm_summary
//! ```
//!
//! This is the entry point the capture subsystem (doc 05) calls with an
//! **ephemeral** frame. The frame is decoded, downscaled, OCR'd, then **dropped**
//! — it is never persisted (doc 13). Only the [`ProcessedFrame`] (text,
//! confidence, embedding, thumb hash) flows onward to the
//! [`ScreenContextWriter`](crate::screen_context_writer).
//!
//! ## Budgets (doc 06 §5, gated at M2)
//! - OCR ≤ 400 ms/frame.
//! - Embed ≤ 300 ms.
//! These are measured, not enforced by a timer here; a budget overrun is logged
//! via `tracing` for the M2 gate.

use aperture_contracts::event::Event;

use crate::ocr_engine::{OcrEngine, OcrOutput};
use crate::VisionError;

/// OCR budget per frame (doc 06 §5). Telemetry compares against this; the M2 gate
/// asserts it on the real target.
pub const OCR_BUDGET_MS: u64 = 400;
/// Embedding budget per frame (doc 06 §5 / doc 03 §5).
pub const EMBED_BUDGET_MS: u64 = 300;

/// Everything Layer A derived from one frame, ready to persist (doc 03 §3).
/// The raw frame itself is **not** here — it has already been dropped.
#[derive(Debug, Clone)]
pub struct ProcessedFrame {
    /// The event this context attaches to (`screen_context.event_id`, doc 03 §3).
    pub event_id: i64,
    /// Post-filter OCR output (doc 06 §2).
    pub ocr: OcrOutput,
    /// 768-d embedding of `ocr.text` (nomic-embed-text-v1.5, doc 03 §5). `None`
    /// when the text was empty or embedding failed (soft failure, doc 06 §6).
    pub embedding: Option<Vec<f32>>,
    /// Perceptual hash of the (downscaled) frame for near-duplicate suppression
    /// — the only frame-derived artifact persisted (doc 03 §3). RAW FRAMES ARE
    /// NEVER STORED.
    pub thumb_phash: Option<String>,
}

/// Drives one frame through Layer A. Holds the swappable OCR engine and the
/// embedder; constructed once and reused across frames.
pub struct FrameProcessor {
    // TODO(M2): own the engine + embedder.
    //   engine: Box<dyn OcrEngine>,
    //   embedder: aperture_embedding::Embedder,
    _private: (),
}

impl FrameProcessor {
    /// Build the processor with a chosen OCR engine (default:
    /// [`WindowsMediaOcr`](crate::windows_media_ocr::WindowsMediaOcr)) and the
    /// shared embedder (doc 03 §5).
    pub fn new(_engine: Box<dyn OcrEngine>) -> Self {
        // TODO(M2): store engine + construct/borrow the aperture-embedding model.
        todo!("M2: wire OcrEngine + aperture-embedding embedder")
    }

    /// Process one ephemeral frame for `event` (doc 06 §5).
    ///
    /// Steps: decode → downscale (≤ 1600 px long edge) → grayscale → OCR → embed
    /// the OCR text → compute thumb pHash → **drop the frame**. Returns the
    /// [`ProcessedFrame`] for the writer. The wake decision (doc 06 §4) and the
    /// actual VLM enqueue are *not* done here — the orchestrator owns that and
    /// calls [`vlm_gating::should_wake_vlm`](crate::vlm_gating::should_wake_vlm)
    /// with this frame's [`OcrOutput`], because waking needs the mutex + budget
    /// state this crate cannot see.
    pub async fn process(
        &self,
        _event: &Event,
        _frame_jpeg_or_raw: &[u8],
        _lang: &str,
    ) -> Result<ProcessedFrame, VisionError> {
        // TODO(M2):
        //   1. decode `frame` (via `image`); map decode errors to VisionError::Image.
        //   2. downscale to <=1600 px long edge + grayscale (windows_media_ocr consts).
        //   3. engine.process_frame(...) within the OCR budget (log if > 400 ms).
        //   4. if !ocr.text.is_empty(): embedder.embed(&ocr.text) (log if > 300 ms);
        //      on failure -> embedding = None (soft, doc 06 §6).
        //   5. compute thumb_phash from the downscaled frame, then drop the frame.
        //   6. return ProcessedFrame; never persist or return raw bytes.
        todo!("M2: decode -> downscale -> OCR -> embed -> phash -> drop frame")
    }
}
