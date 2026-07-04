//! Layer A OCR engine abstraction (doc 06 §2).
//!
//! The default engine is the in-box `Windows.Media.Ocr` (see
//! [`crate::windows_media_ocr`]). RapidOCR/ONNX or Tesseract are fallback
//! candidates if in-box quality on dense UI text proves insufficient — and the
//! whole point of this trait is that swapping is a one-line change with no churn
//! upstream (doc 06 §2: "swap behind one `OcrEngine` trait").

use crate::VisionError;

/// The result of running one frame through an [`OcrEngine`] (doc 06 §2).
///
/// `text` is the concatenated, post-filtered line text (low-quality lines are
/// already dropped — see [`crate::windows_media_ocr`], incl. the note on the
/// in-box engine's missing confidence API). `mean_confidence` is the mean
/// per-line quality over the surviving lines and is what the gate in doc 06 §4
/// reads to decide whether to wake the VLM.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OcrOutput {
    /// Concatenated line text, post-quality-filter (doc 06 §2).
    pub text: String,
    /// Mean per-line quality/confidence in `[0.0, 1.0]` over surviving lines.
    pub mean_confidence: f32,
}

impl OcrOutput {
    /// A coarse "how much readable text is on screen" signal used by the wake
    /// gate's density branch (doc 06 §4, branch (b)). Word count is a cheap,
    /// engine-agnostic proxy; the actual `LOW` threshold lives in
    /// [`vlm_gating`](crate::vlm_gating).
    pub fn text_density(&self) -> usize {
        self.text.split_whitespace().count()
    }
}

/// A swappable OCR backend (doc 06 §2). Implementations run **CPU-only** and must
/// honor the Layer-A budget (≤ 400 ms/frame, doc 06 §5) — they never touch the
/// GPU or the mutex.
///
/// `process_frame` takes a *pre-processed* frame: raw **BGRA8** bytes already
/// downscaled to ≤ 1600 px long edge by the caller
/// ([`FrameProcessor`](crate::frame_processor)), with its dimensions. The
/// engine's language selection happens at construction (doc 06 §6 fallback).
pub trait OcrEngine: Send + Sync {
    /// Run OCR on one pre-processed BGRA8 frame (doc 06 §2).
    fn process_frame(&self, frame: &[u8], width: u32, height: u32)
        -> Result<OcrOutput, VisionError>;

    /// Stable identifier for telemetry / the M2 gate ("which engine produced
    /// this row"), e.g. `"windows-media-ocr"`.
    fn engine_id(&self) -> &'static str;
}
