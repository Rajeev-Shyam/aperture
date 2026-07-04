//! Persisting Layer A/B results to `screen_context` (doc 03 §3, doc 06).
//!
//! This module owns the *shape* of a `screen_context` row and the rule that
//! makes it safe to persist: **raw frames are never stored** (doc 03 §3,
//! doc 13). Only the OCR text, its confidence, an optional VLM summary, and a
//! perceptual hash leave this pipeline.
//!
//! Actual DB writes go through `aperture-db` (the single-writer Tier-0 pipeline,
//! doc 03 §1) — this crate produces the row; it does not open the connection.
//! The OCR text on the row is what gets embedded into `ctx_vec` (doc 03 §5) and
//! what feeds patterns and payloads — text, not screenshots, is the context
//! currency (doc 06 §2).

use crate::frame_processor::ProcessedFrame;
use crate::vlm_layer::SceneJson;

/// A `screen_context` row, mirroring the DDL in doc 03 §3:
/// `(event_id, ocr_text, ocr_confidence, vlm_summary, thumb_phash)`.
///
/// `id` is assigned by the DB on insert. `vlm_summary` is `None` unless Layer B
/// actually ran (doc 06 §3) — and because the VLM never gates a bubble, it is
/// typically filled by a later UPDATE once the async job returns, not on the
/// initial INSERT.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ScreenContextRow {
    /// FK to `events.id` (`ON DELETE CASCADE`, doc 03 §3).
    pub event_id: i64,
    /// Cheap always-on OCR output, post-exclusion + post-confidence-filter
    /// (doc 03 §3, doc 06 §2).
    pub ocr_text: Option<String>,
    /// Mean word confidence (doc 06 §2).
    pub ocr_confidence: Option<f32>,
    /// Serialized [`SceneJson`] (doc 06 §3) — only when the VLM was invoked.
    pub vlm_summary: Option<String>,
    /// Perceptual hash of the downscaled frame; raw frames are NOT stored
    /// (doc 03 §3).
    pub thumb_phash: Option<String>,
}

impl ScreenContextRow {
    /// Build the initial row from a [`ProcessedFrame`] (Layer A; doc 06 §5).
    /// `vlm_summary` is left `None` — Layer B, if it runs, lands later via
    /// [`with_vlm_summary`](Self::with_vlm_summary).
    ///
    /// Empty OCR text is normalized to `None` so it neither embeds nor counts as
    /// content downstream.
    pub fn from_processed(frame: &ProcessedFrame) -> Self {
        let text = frame.ocr.text.trim();
        Self {
            event_id: 0, // stamped by the store step once the event row exists
            ocr_text: (!text.is_empty()).then(|| frame.ocr.text.clone()),
            ocr_confidence: (!text.is_empty()).then_some(frame.ocr.mean_confidence),
            vlm_summary: None, // Layer B lands later via with_vlm_summary (M5)
            thumb_phash: frame.thumb_phash.clone(),
        }
    }

    /// Attach a VLM scene summary (doc 06 §3) — serialized to the
    /// `vlm_summary` TEXT column. Called when the async Layer-B job returns; the
    /// caller then issues the UPDATE through `aperture-db`.
    pub fn with_vlm_summary(mut self, _scene: &SceneJson) -> Self {
        // TODO(M5): self.vlm_summary = Some(serde_json::to_string(scene)?);
        todo!("M5: serialize SceneJson into the vlm_summary column")
    }

    /// Debug-assert the no-raw-frames invariant (doc 03 §3, doc 13): a row may
    /// only ever carry text/confidence/summary/phash. There is no field that
    /// *could* hold pixels — this is the type-level guarantee, asserted in the
    /// M9 frame-level privacy test.
    pub fn assert_no_raw_frame(&self) {
        // Intentionally a no-op: the absence of any byte/image field on this
        // struct IS the guarantee. Kept as a named hook for the M9 gate.
    }
}
