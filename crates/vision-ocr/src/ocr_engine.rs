//! Layer A OCR engine abstraction (doc 06 §2).
//!
//! The default engine is the in-box `Windows.Media.Ocr` (see
//! [`crate::windows_media_ocr`]). RapidOCR/ONNX or Tesseract are fallback
//! candidates if in-box quality on dense UI text proves insufficient — and the
//! whole point of this trait is that swapping is a one-line change with no churn
//! upstream (doc 06 §2: "swap behind one `OcrEngine` trait").

use crate::VisionError;

/// One recognized word with its bounding box (doc 24 decision #3).
///
/// Coordinates are **pixels in the frame passed to
/// [`OcrEngine::process_frame`]** — i.e. the already-downscaled frame, not the
/// original capture. The image-redaction gate (`aperture_privacy::image_redaction`)
/// paints over these boxes, so they must be in the same space as the buffer
/// that is recomposed.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OcrWord {
    /// The word's text as the engine emitted it.
    pub text: String,
    /// Left edge, px.
    pub x: u32,
    /// Top edge, px.
    pub y: u32,
    /// Width, px.
    pub w: u32,
    /// Height, px.
    pub h: u32,
}

/// One recognized line: the engine's line text plus its words with geometry
/// (doc 24 decision #3). `text` is the same string that is joined into
/// [`OcrOutput::text`]; `words` may be empty for engines/paths without
/// geometry (e.g. [`crate::windows_media_ocr::aggregate_lines`]).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OcrLine {
    /// The line text (what the quality filter scored and what `text` joins).
    pub text: String,
    /// The line's words with bounding boxes, in reading order.
    pub words: Vec<OcrWord>,
}

/// The result of running one frame through an [`OcrEngine`] (doc 06 §2).
///
/// `text` is the concatenated, post-filtered line text (low-quality lines are
/// already dropped — see [`crate::windows_media_ocr`], incl. the note on the
/// in-box engine's missing confidence API). `mean_confidence` is the mean
/// per-line quality over the surviving lines and is what the gate in doc 06 §4
/// reads to decide whether to wake the VLM.
///
/// `lines` is additive (doc 15 §6; doc 24 decision #3): the same surviving
/// lines, in order, with per-word geometry for the image-redaction gate.
/// `text` and `mean_confidence` are unaffected by it — `text` feeds embeddings
/// and pattern signatures and must stay byte-identical for the same input.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OcrOutput {
    /// Concatenated line text, post-quality-filter (doc 06 §2).
    pub text: String,
    /// Mean per-line quality/confidence in `[0.0, 1.0]` over surviving lines.
    pub mean_confidence: f32,
    /// The surviving lines (post-quality-filter, same order as `text`) with
    /// word boxes (doc 24 decision #3). Empty when the engine carries no
    /// geometry; `#[serde(default)]` so older serialized rows still load.
    #[serde(default)]
    pub lines: Vec<OcrLine>,
    /// The lines the quality filter DROPPED, geometry intact (08-22 review):
    /// their text never reaches `text` or `lines` — so it never leaves the
    /// machine — but their pixels are still in the frame, and the image gate
    /// (`screen-serializer::observe_frame`) must paint over them too. Never
    /// serialized; in-memory only.
    #[serde(skip)]
    pub dropped_lines: Vec<OcrLine>,
}

impl OcrOutput {
    /// A coarse "how much readable text is on screen" signal used by the wake
    /// gate's density branch (doc 06 §4, branch (b)). Word count is a cheap,
    /// engine-agnostic proxy; the actual `LOW` threshold lives in
    /// `aperture_orchestration::tier_router` (the operational wake gate).
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
