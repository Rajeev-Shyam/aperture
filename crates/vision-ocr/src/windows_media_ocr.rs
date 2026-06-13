//! Default Layer-A engine: in-box `Windows.Media.Ocr` (doc 06 §2).
//!
//! Why in-box: fully local, per-language packs, fast on CPU, zero VRAM — exactly
//! the Tier-0 profile (doc 06 §1). [VERIFY accuracy on dense UI text; if
//! insufficient, swap RapidOCR/Tesseract behind the [`OcrEngine`] trait without
//! touching callers.]
//!
//! Pipeline for one frame (doc 06 §2):
//!   1. downscale to ≤ 1600 px long edge (balance of OCR quality vs. speed),
//!   2. grayscale,
//!   3. run `OcrEngine.RecognizeAsync`,
//!   4. drop lines whose confidence is < 0.5,
//!   5. emit concatenated text + **mean word** confidence.

use crate::ocr_engine::{OcrEngine, OcrOutput};
use crate::VisionError;

/// Max long-edge in pixels for the Layer-A downscale (doc 06 §2)
/// [ASSUMPTION: OCR quality/speed balance].
pub const OCR_MAX_LONG_EDGE_PX: u32 = 1600;

/// Lines whose recognition confidence is below this are dropped before the text
/// is concatenated (doc 06 §2) [ASSUMPTION].
pub const MIN_LINE_CONFIDENCE: f32 = 0.5;

/// The in-box `Windows.Media.Ocr` engine.
///
/// Wraps a per-language `windows::Media::Ocr::OcrEngine`; constructed once per
/// language and reused (engine creation is comparatively expensive).
pub struct WindowsMediaOcr {
    // TODO(M2): hold the resolved language + a cached `Media::Ocr::OcrEngine`.
    //   engine: windows::Media::Ocr::OcrEngine,
    //   lang: windows::Globalization::Language,
    _private: (),
}

impl WindowsMediaOcr {
    /// Construct an engine for the user's profile language, falling back to `en`
    /// if the requested language pack is unavailable (doc 06 §6: "Language pack
    /// missing ⇒ fall back to en + notice"). [VERIFY language coverage.]
    pub fn new(_lang: &str) -> Result<Self, VisionError> {
        // TODO(M2):
        //   1. `Language::new(lang)`; if `OcrEngine::TryCreateFromLanguage` is
        //      null, fall back to `OcrEngine::TryCreateFromUserProfileLanguages`
        //      or `Language::new("en")` and flag the fallback (doc 06 §6).
        //   2. cache the engine.
        todo!("M2: create Windows.Media.Ocr engine for `lang`, fall back to en")
    }
}

impl OcrEngine for WindowsMediaOcr {
    fn process_frame(&self, _frame: &[u8], _lang: &str) -> Result<OcrOutput, VisionError> {
        // TODO(M2): the frame arriving here is already downscaled (≤ 1600 px long
        // edge) and grayscale — see `FrameProcessor`. This method:
        //   1. wrap the decoded bytes in a `SoftwareBitmap` (doc 06 §2);
        //   2. `engine.RecognizeAsync(bitmap).get()` -> `OcrResult`;
        //   3. for each `OcrLine`, average its `OcrWord.Confidence` if exposed
        //      (else use line-level confidence [VERIFY which the API surfaces]);
        //   4. drop lines below `MIN_LINE_CONFIDENCE`;
        //   5. join surviving line text with '\n'; `mean_confidence` = mean over
        //      surviving *words* (doc 06 §2). Empty result => confidence 0.0.
        //
        // Budget: ≤ 400 ms/frame (doc 06 §5) — measured at the M2 gate.
        todo!("M2: RecognizeAsync, drop <0.5-conf lines, mean word confidence")
    }

    fn engine_id(&self) -> &'static str {
        "windows-media-ocr"
    }
}
