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
/// `text` is the concatenated, post-filtered line text (lines under the
/// confidence floor are already dropped — see [`windows_media_ocr`](crate::windows_media_ocr)).
/// `mean_confidence` is the mean *word* confidence over the surviving lines and
/// is what the gate in doc 06 §4 reads to decide whether to wake the VLM.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OcrOutput {
    /// Concatenated line text, post-confidence-filter (doc 06 §2).
    pub text: String,
    /// Mean word confidence in `[0.0, 1.0]` over surviving lines (doc 06 §2).
    pub mean_confidence: f32,
}

impl OcrOutput {
    /// A coarse "how much readable text is on screen" signal used by the wake
    /// gate's density branch (doc 06 §4, branch (b)). Word count is a cheap,
    /// engine-agnostic proxy; the actual `LOW` threshold lives in
    /// [`vlm_gating`](crate::vlm_gating).
    pub fn text_density(&self) -> usize {
        // TODO(M2): consider chars-per-area once frame dims are threaded through;
        // word count is the M2-stage proxy.
        self.text.split_whitespace().count()
    }
}

/// A swappable OCR backend (doc 06 §2). Implementations run **CPU-only** and must
/// honor the Layer-A budget (≤ 400 ms/frame, doc 06 §5) — they never touch the
/// GPU or the mutex.
///
/// `process_frame` takes a *decoded, pre-processed* frame as raw bytes (the
/// caller — [`FrameProcessor`](crate::frame_processor) — has already downscaled
/// to ≤ 1600 px long edge and converted to grayscale) plus a BCP-47 `lang` tag.
/// On a missing language pack the engine falls back to `en` and the pipeline
/// notes it (doc 06 §6).
pub trait OcrEngine: Send + Sync {
    /// Run OCR on one pre-processed frame. `frame` is the raw decoded image
    /// buffer (format documented by the impl); `lang` is a BCP-47 tag (e.g.
    /// `"en-US"`). See doc 06 §2.
    fn process_frame(&self, frame: &[u8], lang: &str) -> Result<OcrOutput, VisionError>;

    /// Stable identifier for telemetry / the M2 gate ("which engine produced
    /// this row"), e.g. `"windows-media-ocr"`.
    fn engine_id(&self) -> &'static str;
}
