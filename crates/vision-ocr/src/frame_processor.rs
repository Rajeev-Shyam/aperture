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
//! **ephemeral** frame's BGRA bytes. The frame is downscaled, OCR'd, then
//! **dropped** — it is never persisted (doc 13). Only the [`ProcessedFrame`]
//! (text, confidence, embedding, thumb hash) flows onward to the
//! [`ScreenContextRow`](crate::screen_context_writer::ScreenContextRow).
//!
//! ## Budgets (doc 06 §5, gated at M2)
//! - OCR ≤ 400 ms/frame.
//! - Embed ≤ 300 ms.
//! These are measured, not enforced by a timer here; a budget overrun is logged
//! via `tracing` for the M2 gate.

use std::sync::Arc;

use aperture_embedding::Embedder;

use crate::ocr_engine::{OcrEngine, OcrOutput};
use crate::windows_media_ocr::OCR_MAX_LONG_EDGE_PX;
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
    /// Post-filter OCR output (doc 06 §2).
    pub ocr: OcrOutput,
    /// 768-d embedding of `ocr.text` (nomic-embed-text-v1.5, doc 03 §5). `None`
    /// when the text was empty or embedding failed (soft failure, doc 06 §6).
    pub embedding: Option<Vec<f32>>,
    /// Perceptual hash of the frame, computed upstream by the capture gate
    /// (ADR-032/Q72) and carried through — the only frame-derived artifact
    /// persisted (doc 03 §3). RAW FRAMES ARE NEVER STORED.
    pub thumb_phash: Option<String>,
}

/// Drives one frame through Layer A. Holds the swappable OCR engine and the
/// embedder; constructed once and reused across frames.
pub struct FrameProcessor {
    engine: Box<dyn OcrEngine>,
    embedder: Arc<dyn Embedder>,
}

impl FrameProcessor {
    /// Build the processor with a chosen OCR engine (default:
    /// [`WindowsMediaOcr`](crate::windows_media_ocr::WindowsMediaOcr)) and the
    /// shared embedder (doc 03 §5).
    pub fn new(engine: Box<dyn OcrEngine>, embedder: Arc<dyn Embedder>) -> Self {
        Self { engine, embedder }
    }

    /// Process one ephemeral frame's BGRA8 bytes (doc 06 §5).
    ///
    /// Steps: downscale (≤ 1600 px long edge) → OCR → embed the OCR text →
    /// return; the caller drops the frame buffer. `thumb_phash` is supplied by
    /// the capture-side gate (it already hashed the frame, ADR-032/Q72).
    pub fn process(
        &self,
        bgra: &[u8],
        width: u32,
        height: u32,
        thumb_phash: Option<String>,
    ) -> Result<ProcessedFrame, VisionError> {
        // 1. downscale to the OCR budget size (doc 06 §2).
        let (small, sw, sh) = downscale_bgra(bgra, width, height, OCR_MAX_LONG_EDGE_PX)?;

        // 2. OCR within the 400 ms budget (log overruns for the M2 gate).
        let t0 = std::time::Instant::now();
        let ocr = self.engine.process_frame(&small, sw, sh)?;
        let ocr_ms = t0.elapsed().as_millis() as u64;
        if ocr_ms > OCR_BUDGET_MS {
            tracing::warn!(ocr_ms, budget = OCR_BUDGET_MS, "OCR budget overrun (M2 gate)");
        }

        // 3. embed the text (soft failure ⇒ None, doc 06 §6).
        let embedding = if ocr.text.trim().is_empty() {
            None
        } else {
            let t1 = std::time::Instant::now();
            let vec = self.embedder.embed(&ocr.text);
            let embed_ms = t1.elapsed().as_millis() as u64;
            if embed_ms > EMBED_BUDGET_MS {
                tracing::warn!(embed_ms, budget = EMBED_BUDGET_MS, "embed budget overrun (M2 gate)");
            }
            vec.ok()
        };

        Ok(ProcessedFrame { ocr, embedding, thumb_phash })
    }

    /// The active engine's id (M2 gate telemetry).
    pub fn engine_id(&self) -> &'static str {
        self.engine.engine_id()
    }
}

/// Downscale a BGRA8 buffer so its longest edge is ≤ `max_edge` (doc 06 §2).
/// Never upscales. Returns `(bytes, width, height)`.
pub fn downscale_bgra(
    bgra: &[u8],
    width: u32,
    height: u32,
    max_edge: u32,
) -> Result<(Vec<u8>, u32, u32), VisionError> {
    if bgra.len() != (width as usize) * (height as usize) * 4 {
        return Err(VisionError::Image("bgra buffer size mismatch".into()));
    }
    let long_edge = width.max(height);
    if long_edge <= max_edge {
        return Ok((bgra.to_vec(), width, height));
    }
    let scale = max_edge as f32 / long_edge as f32;
    let nw = ((width as f32 * scale).round() as u32).max(1);
    let nh = ((height as f32 * scale).round() as u32).max(1);

    // image crate: BGRA isn't a native layout; swizzle to RGBA, resize, swizzle back.
    let mut rgba = bgra.to_vec();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let img = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| VisionError::Image("frame -> image buffer".into()))?;
    let resized = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle);
    let mut out = resized.into_raw();
    for px in out.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Ok((out, nw, nh))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_embedding::{EmbedError, EMBED_DIM};

    struct FakeOcr(&'static str);
    impl OcrEngine for FakeOcr {
        fn process_frame(&self, _f: &[u8], _w: u32, _h: u32) -> Result<OcrOutput, VisionError> {
            Ok(OcrOutput { text: self.0.to_string(), mean_confidence: 0.9 })
        }
        fn engine_id(&self) -> &'static str {
            "fake"
        }
    }

    struct FakeEmbedder;
    impl Embedder for FakeEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, EmbedError> {
            Ok(vec![0.5; EMBED_DIM])
        }
        fn id(&self) -> &'static str {
            "fake-embed"
        }
    }

    #[test]
    fn frame_flows_to_text_plus_embedding_and_is_never_retained() {
        let p = FrameProcessor::new(Box::new(FakeOcr("hello world")), Arc::new(FakeEmbedder));
        let bgra = vec![128u8; 8 * 8 * 4];
        let out = p
            .process(&bgra, 8, 8, Some("00ff00ff00ff00ff".into()))
            .expect("process");
        assert_eq!(out.ocr.text, "hello world");
        assert_eq!(out.embedding.as_ref().map(|v| v.len()), Some(EMBED_DIM));
        assert_eq!(out.thumb_phash.as_deref(), Some("00ff00ff00ff00ff"));
    }

    #[test]
    fn empty_ocr_text_embeds_nothing() {
        let p = FrameProcessor::new(Box::new(FakeOcr("")), Arc::new(FakeEmbedder));
        let bgra = vec![0u8; 4 * 4 * 4];
        let out = p.process(&bgra, 4, 4, None).expect("process");
        assert!(out.embedding.is_none(), "no text ⇒ nothing to embed (doc 06 §6)");
    }

    #[test]
    fn downscale_caps_the_long_edge_and_never_upscales() {
        let bgra = vec![10u8; 3200 * 400 * 4];
        let (_out, w, h) = downscale_bgra(&bgra, 3200, 400, 1600).expect("downscale");
        assert_eq!(w, 1600);
        assert_eq!(h, 200);

        let small = vec![10u8; 8 * 8 * 4];
        let (_o, w2, h2) = downscale_bgra(&small, 8, 8, 1600).expect("no-op");
        assert_eq!((w2, h2), (8, 8), "never upscale");
    }
}
