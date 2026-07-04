//! Per-model VRAM parameter table (doc 04 §2, operationalized for doc 12 §4).
//!
//! These are the **planning figures** the [`crate::budget_enforcer`] runs the R1
//! projection on *until the M5 measurement harness overwrites them with measured
//! numbers* (doc 12 §4, doc 16 M5). Every value here is a doc 04 §2 estimate and
//! is therefore `[VERIFY all on hardware]`.
//!
//! Invariant (1): the 8 GB ceiling — the enforcer's job is to keep
//! `active(weights + mmproj + kv_est(ctx) + img_act(n)) + framework +
//! co_resident_weights <= 7.0 GB` (doc 04 R1, ADR-030). This table supplies
//! every term except `kv_est`/`img_act` scaling and the co-resident sum.

use std::collections::HashMap;

/// The models the resource manager knows how to load (doc 04 §3). No third
/// loadout exists; 13B+ is out of scope (doc 04 §2, R5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelId {
    /// Qwen2.5-VL 3B Q4_K_M — the L1 default (doc 04 §3).
    Vlm3b,
    /// Qwen2.5-VL 7B Q4_K_M — the L2 opt-in, runs exclusive (doc 04 §3).
    Vlm7b,
    /// faster-whisper small (CTranslate2, GPU `stt-host`) — the L1 conditionally
    /// co-resident STT model (ADR-024; ~2 GB, the figure that forced ADR-030).
    FasterWhisperSmall,
    /// faster-whisper distil-large-v3 int8 — opt-in STT (doc 04 §2, ADR-024).
    WhisperDistilLargeV3Int8,
}

/// The VRAM line items for one model (doc 04 §2). `weights`, `mmproj`, and the
/// activation/KV *scaling* coefficients are per-model; `framework` is the shared
/// llama.cpp CUDA baseline and lives on the [`VramTable`].
///
/// All figures in **gigabytes**.
#[derive(Debug, Clone, Copy)]
pub struct VramParams {
    /// Quantized weights resident in VRAM (doc 04 §2).
    pub weights_gb: f32,
    /// Vision projector (mmproj, always FP16); `0.0` for STT models (doc 04 §2).
    pub mmproj_gb: f32,
    /// Per-image activation spike: ViT + image KV (doc 04 §2, the R2 "silent
    /// killer"). `0.0` for STT models.
    pub img_act_per_image_gb: f32,
    /// KV cache at a reference context; the enforcer scales it linearly by
    /// `ctx_tokens / kv_ref_ctx_tokens` (doc 04 §2: ~1–2 GB @ 8K for the 7–8B class).
    pub kv_ref_gb: f32,
    /// The context (in tokens) `kv_ref_gb` was estimated at (doc 04 §2).
    pub kv_ref_ctx_tokens: u32,
    /// `false` once a real OOM proved this row wrong (doc 12 §7); the enforcer
    /// conservative-caps until re-measured. Seeded `true` from the §2 estimates,
    /// flipped to "measured" (still `true`) after the M5 harness runs.
    pub measured: bool,
}

/// The seeded planning table plus the shared framework baseline. Mutable so the
/// M5 harness can overwrite rows and so [`mark_unmeasured`](VramTable::mark_unmeasured)
/// can flag a row after an OOM.
#[derive(Debug, Clone)]
pub struct VramTable {
    /// llama.cpp CUDA context baseline, charged once while any sidecar is loaded
    /// (doc 04 §2: ~0.5–1.0 GB). We seed the conservative high end.
    pub framework_gb: f32,
    rows: HashMap<ModelId, VramParams>,
}

impl VramTable {
    /// Seed from the doc 04 §2 estimates. **[VERIFY all on hardware]**; replaced
    /// wholesale by the M5 measurement harness (doc 12 §4, doc 16 M5).
    pub fn seeded() -> Self {
        let mut rows = HashMap::new();
        // TODO(M5:) replace every figure below with the measured M5 number and
        // set `measured: true` only once the harness has recorded it (doc 16 M5).
        rows.insert(
            ModelId::Vlm3b,
            VramParams {
                weights_gb: 1.93, // doc 04 §2: 3B Q4_K_M weights
                mmproj_gb: 1.34,  // doc 04 §2: 3B mmproj
                img_act_per_image_gb: 1.2, // doc 04 §2: per-image spike
                kv_ref_gb: 1.0,
                kv_ref_ctx_tokens: 8192,
                measured: false,
            },
        );
        rows.insert(
            ModelId::Vlm7b,
            VramParams {
                weights_gb: 4.68, // doc 04 §2: 7B Q4_K_M weights
                mmproj_gb: 1.35,  // doc 04 §2: 7B mmproj (FP16)
                img_act_per_image_gb: 1.2,
                kv_ref_gb: 2.0, // doc 04 §2: ~1–2 GB @ 8K, 7–8B class (high end)
                kv_ref_ctx_tokens: 8192,
                measured: false,
            },
        );
        rows.insert(
            ModelId::FasterWhisperSmall,
            VramParams {
                weights_gb: 2.0, // ADR-024: faster-whisper small ≈ 2 GB (1–2 GB range,
                //                  seed the conservative high end — this is the figure
                //                  that makes L1 co-residency conditional, ADR-030)
                mmproj_gb: 0.0,
                img_act_per_image_gb: 0.0,
                kv_ref_gb: 0.0,
                kv_ref_ctx_tokens: 0,
                measured: false,
            },
        );
        rows.insert(
            ModelId::WhisperDistilLargeV3Int8,
            VramParams {
                weights_gb: 1.48, // doc 04 §2: measured upstream
                mmproj_gb: 0.0,
                img_act_per_image_gb: 0.0,
                kv_ref_gb: 0.0,
                kv_ref_ctx_tokens: 0,
                measured: false,
            },
        );
        Self {
            framework_gb: 1.0, // doc 04 §2: ~0.5–1.0 GB; seed the conservative high end
            rows,
        }
    }

    /// The params for one model.
    pub fn params(&self, model: ModelId) -> Option<&VramParams> {
        self.rows.get(&model)
    }

    /// Overwrite a row with a measured `VramParams` (the M5 harness, doc 12 §4).
    pub fn set_measured(&mut self, model: ModelId, params: VramParams) {
        // TODO(M5:) called by the measurement harness with measured: true.
        self.rows.insert(model, params);
    }

    /// Mark a row unmeasured after a real OOM from its sidecar (doc 12 §7):
    /// the projection table was wrong, so conservative-cap and demand a re-measure.
    pub fn mark_unmeasured(&mut self, model: ModelId) {
        // TODO(M5:) on OOM, flip `measured=false`, conservative-cap, and re-run
        // the M5 measurement harness for this row (doc 12 §7).
        if let Some(p) = self.rows.get_mut(&model) {
            p.measured = false;
        }
    }
}

impl Default for VramTable {
    fn default() -> Self {
        Self::seeded()
    }
}
