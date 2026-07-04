//! The R1 projection check + the R3 degrade ladder (doc 04 §6, doc 12 §4).
//!
//! Invariant (1): the 8 GB VRAM ceiling. Every load/job is admitted **iff**
//! ```text
//! projected = active(weights + mmproj + kv_est(ctx_tokens) + img_act(n_images))
//!           + framework + co_resident_weights      // ADR-030: co-resident weights ARE counted
//! projected <= 7.0 GB        // 1.0 GB margin under the 8 GB ceiling (doc 04 R1)
//! ```
//! On refusal the caller is hung off the R3 degrade ladder (doc 04 §6) rather
//! than left guessing — the projection rides back in
//! [`aperture_contracts::JobError::BudgetRefused`].
//!
//! Co-residency is *conditional* (ADR-030): under image-VLM pressure the enforcer
//! unloads the co-resident STT (the swap victim) before admitting the job — the
//! [`DegradeRung::UnloadCoResidentStt`] rung. A warm-kept STT is protected from
//! pattern-VLM (prio 50 degrades to OCR-text-only instead) but yields to a
//! user/enrichment image-VLM (doc 12 §4).

use crate::vram_table::{ModelId, VramTable};

/// The hard projection ceiling: 1.0 GB under the 8 GB GPU (doc 04 R1, ADR-030;
/// amended from R1's 7.2 — the projection now counts co-resident weights).
pub const PROJECTION_CEILING_GB: f32 = 7.0;

/// The R3 degrade ladder, **in order** (doc 04 R3, doc 12 §4). The enforcer
/// proposes the next rung when a projection fails; the scheduler applies it and
/// re-projects until something fits or it reaches [`Refuse`](DegradeRung::Refuse).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradeRung {
    /// 7B -> 3B (doc 04 R3 step 1).
    DropTo3b,
    /// Shrink context: 8K -> 4K -> 2K (doc 04 R3 step 2). Carries the next ctx.
    ShrinkCtx { next_ctx_tokens: u32 },
    /// Unload the co-resident STT (the swap victim; reloaded on next PTT) —
    /// doc 04 R3 step 3, ADR-030/Q38: voice warmth yields *before* image quality.
    UnloadCoResidentStt,
    /// Drop the image -> OCR-text-only prompt (doc 04 R3 step 4, the R2 escape).
    DropImage,
    /// Queue the job behind the mutex (doc 04 R3 step 5).
    Queue,
    /// Refuse with a UI notice (doc 04 R3 step 6, the terminal rung).
    Refuse,
}

/// What the enforcer was asked to admit: a concrete loadout for one job.
#[derive(Debug, Clone, Copy)]
pub struct LoadRequest {
    pub model: ModelId,
    pub ctx_tokens: u32,
    /// 0 or 1 — R2 caps VLM input at one image (doc 04 R2); STT is always 0.
    pub n_images: u32,
}

/// The projection result the caller degrades on (doc 12 §4). On refusal,
/// `projection_gb` is exactly what rides back in `JobError::BudgetRefused`.
#[derive(Debug, Clone, Copy)]
pub struct Projection {
    pub projected_gb: f32,
    pub admit: bool,
}

/// Owns the [`VramTable`] and runs the R1 check (doc 04 §6, doc 12 §4).
#[derive(Debug)]
pub struct BudgetEnforcer {
    table: VramTable,
}

impl BudgetEnforcer {
    /// Construct over a (seeded or measured) VRAM table.
    pub fn new(table: VramTable) -> Self {
        Self { table }
    }

    /// Mutable access so the scheduler can route an OOM to
    /// [`VramTable::mark_unmeasured`] / the M5 harness to `set_measured` (doc 12 §7).
    pub fn table_mut(&mut self) -> &mut VramTable {
        &mut self.table
    }

    /// Project the VRAM for one load request (doc 04 R1, doc 12 §4, ADR-030):
    /// `active(weights + mmproj + kv_est(ctx) + img_act(n_images)) + framework
    /// + co_resident_weights`.
    pub fn project(&self, _req: LoadRequest) -> Projection {
        // TODO(M5:) compute against measured rows once the M5 harness has run:
        //   let p = self.table.params(req.model)?;
        //   let kv = if p.kv_ref_ctx_tokens > 0 {
        //       p.kv_ref_gb * (req.ctx_tokens as f32 / p.kv_ref_ctx_tokens as f32)
        //   } else { 0.0 };
        //   let img = p.img_act_per_image_gb * req.n_images as f32;
        //   let co_resident = <sum of weights_gb of every OTHER loaded sidecar>;  // ADR-030
        //   let projected = p.weights_gb + p.mmproj_gb + kv + img
        //                 + self.table.framework_gb + co_resident;
        //   Projection { projected_gb: projected, admit: projected <= PROJECTION_CEILING_GB }
        // An unmeasured row (post-OOM) must conservative-cap, not optimistic-pass (doc 12 §7).
        todo!("M5: R1 projection = active(weights+mmproj+kv+img) + framework + co_resident_weights <= 7.0 GB")
    }

    /// Given a failed [`Projection`] for `req`, return the next R3 rung to try
    /// (doc 04 R3, ADR-030 order). Returns `Refuse` once the ladder is exhausted.
    pub fn next_rung(&self, _req: LoadRequest, _failed: Projection) -> DegradeRung {
        // TODO(M5:) walk the ladder in doc 04 R3 (ADR-030) order:
        //   Vlm7b              -> DropTo3b
        //   ctx 8K             -> ShrinkCtx{4K};  4K -> ShrinkCtx{2K}
        //   co-resident STT    -> UnloadCoResidentStt (voice yields before image quality)
        //   n_images == 1      -> DropImage
        //   else               -> Queue, then Refuse
        todo!("M5: R3 ladder: 7B->3B -> ctx shrink -> unload co-resident STT -> drop image -> queue -> refuse")
    }
}
